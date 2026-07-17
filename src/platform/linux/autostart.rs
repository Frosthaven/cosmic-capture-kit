//! Linux launch-at-login via an XDG autostart entry (DRAGON-173).
//!
//! The Linux counterpart of the macOS `SMAppService` login item
//! (`crate::platform::mac::login_item`): a plain `~/.config/autostart/<id>.desktop`
//! file whose `Exec=` runs this binary with the resident flag, so the tray RESIDENT
//! (`crate::daemon_linux`) comes back after a login. Dependency-free by design (the
//! ticket's constraint): we write / read / remove the `.desktop` file directly rather
//! than pulling an XDG-autostart crate.
//!
//! The public API mirrors `login_item` so the settings handler can stay platform-shaped:
//! [`is_enabled`] (does the entry exist and point at us), [`set`] (create / remove it),
//! and — for symmetry — an implicit "always available" (unlike macOS, writing a user
//! config file needs no bundle, so there is no `Unbundled` degrade).
//!
//! ## Why the current exe, not a fixed path
//!
//! The user's PrintScreen shortcut already launches `target/release/cosmic-capture-kit`
//! directly (nothing is installed on PATH). We write `Exec=<current_exe> resident`, the
//! SAME binary the resident/daemon spawn uses, so the autostart entry launches exactly
//! what the running app is — a dev build autostarts the dev build, an installed build the
//! installed one. The bare `resident` argument is the token `main()` early-branches on.

#![cfg(target_os = "linux")]

use std::path::PathBuf;

/// The autostart `.desktop` file's basename (stable, so [`set`] / [`is_enabled`] agree
/// and a later toggle-off removes the exact file a toggle-on wrote).
const DESKTOP_BASENAME: &str = "cosmic-capture-kit.desktop";

/// The bare argument `main()` early-branches on to become the resident. Kept here next to
/// the `Exec=` writer so the autostart command and the branch token can't drift.
const RESIDENT_ARG: &str = "resident";

/// The autostart directory (`$XDG_CONFIG_HOME/autostart`, i.e. `~/.config/autostart`).
fn autostart_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("autostart"))
}

/// The full path to our autostart entry, if the config dir resolves.
fn desktop_path() -> Option<PathBuf> {
    autostart_dir().map(|d| d.join(DESKTOP_BASENAME))
}

/// The `.desktop` file contents launching this exact binary as the resident. Autostart
/// entries must be a valid desktop entry; `X-GNOME-Autostart-enabled=true` and the
/// COSMIC/GNOME/KDE-honoured keys keep it enabled across desktops.
fn desktop_contents(exe: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Cosmic Capture Kit\n\
         Comment=Keep Cosmic Capture Kit running in the background\n\
         Exec={exe} {RESIDENT_ARG}\n\
         Icon=dev.frosthaven.CosmicCaptureKit\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// Whether the autostart entry currently exists (launch-at-login is on). A plain file
/// existence check — the file is only present when [`set(true)`](set) wrote it. Never a
/// panic; a missing config dir reads as `false`.
pub fn is_enabled() -> bool {
    desktop_path().map(|p| p.exists()).unwrap_or(false)
}

/// Create (`on`) or remove (`off`) the autostart entry. On create, writes the `.desktop`
/// file with `Exec=<current_exe> resident` (creating `~/.config/autostart` if needed); on
/// remove, deletes it (a missing file is success — the desired state is "absent"). Returns
/// an honest error string on an I/O failure so the settings UI can surface it. Mirrors
/// `login_item::set`.
pub fn set(on: bool) -> Result<(), String> {
    let path = desktop_path().ok_or_else(|| "could not resolve the autostart directory".to_string())?;
    if on {
        let exe = std::env::current_exe()
            .map_err(|e| format!("could not resolve the current executable: {e}"))?;
        let exe = exe.to_string_lossy().into_owned();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, desktop_contents(&exe))
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        Ok(())
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            // Already absent is the desired state.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not remove {}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_contents_launches_the_exe_as_resident() {
        let c = desktop_contents("/opt/cck/cosmic-capture-kit");
        // The Exec line runs the given binary with the bare resident token main() branches on.
        assert!(
            c.contains("Exec=/opt/cck/cosmic-capture-kit resident\n"),
            "Exec line missing or wrong: {c:?}"
        );
        // A valid desktop entry with the autostart-enabled key.
        assert!(c.starts_with("[Desktop Entry]\n"));
        assert!(c.contains("Type=Application\n"));
        assert!(c.contains("X-GNOME-Autostart-enabled=true\n"));
    }

    #[test]
    fn resident_arg_matches_the_main_branch_token() {
        // The Exec argument must be exactly the token main() checks for a bare resident
        // launch; a drift here would autostart a GUI window instead of the resident.
        assert_eq!(RESIDENT_ARG, "resident");
    }

    #[test]
    fn desktop_basename_is_stable() {
        // set(true) and set(false) must target the same file; pin the basename.
        assert_eq!(DESKTOP_BASENAME, "cosmic-capture-kit.desktop");
    }
}
