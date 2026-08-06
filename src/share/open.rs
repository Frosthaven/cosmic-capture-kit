//! Open-URI and reveal helpers.

use std::path::Path;
use std::process::Command;

use super::reexec::OPEN_URI;

/// Hand a URI to the desktop's default handler via the xdg-desktop-portal
/// `OpenURI` interface (in place of shelling out to `xdg-open`). Returns whether
/// the call was dispatched.
#[cfg(target_os = "linux")]
fn portal_open_uri(uri: &str) -> bool {
    (|| -> Option<()> {
        let conn = zbus::blocking::Connection::session().ok()?;
        let opts: std::collections::HashMap<&str, zbus::zvariant::Value> =
            std::collections::HashMap::new();
        conn.call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.OpenURI"),
            "OpenURI",
            &("", uri, opts),
        )
        .ok()?;
        Some(())
    })()
    .is_some()
}

/// Open a URI (a URL decoded from a QR code, or a `file://` folder) with the
/// desktop's default handler, detached, so the overlay can exit immediately.
pub fn open_uri(uri: &str) {
    // Detached and outliving us, so `self_exe` rather than `current_exe` (DRAGON-510).
    if let Ok(exe) = crate::util::self_exe() {
        let _ = Command::new(exe).arg(OPEN_URI).arg(uri).spawn();
    }
}

/// Ask the desktop's FILE MANAGER to open `dir` directly
/// (`org.freedesktop.FileManager1.ShowFolders`), the same interface [`run_reveal`] uses to
/// highlight a saved capture. Returns whether the call was accepted.
///
/// This exists because the portal is the WRONG tool for a folder. `OpenURI` is documented as
/// being for real URIs, and the spec directs `file://` at `OpenFile`/`OpenDirectory` (which
/// take a file descriptor, not a string). On COSMIC the portal call succeeds at the D-Bus
/// level — it returns a Request handle — and then silently opens nothing, so the caller sees
/// success and the user sees no window. `ShowFolders` is a direct, synchronous call to the
/// file manager and simply works; it is what already backs the post-capture reveal.
#[cfg(target_os = "linux")]
fn filemanager_show_folder(dir: &str) -> bool {
    (|| -> Option<()> {
        let conn = zbus::blocking::Connection::session().ok()?;
        conn.call_method(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            Some("org.freedesktop.FileManager1"),
            "ShowFolders",
            &(vec![dir], ""),
        )
        .ok()?;
        Some(())
    })()
    .is_some()
}

/// Helper: open `uri` with the desktop's default handler, then exit. Used for QR-code URLs
/// and for the settings pages' "open this folder" buttons.
///
/// A `file://` URI naming a DIRECTORY goes to the file manager first (see
/// [`filemanager_show_folder`]); everything else, and any failure, falls through to the
/// portal, which is correct for real URLs. Before this split every folder button routed
/// through the portal and silently did nothing on COSMIC.
#[cfg(target_os = "linux")]
pub fn run_open_uri(uri: &str) {
    if let Some(path) = uri.strip_prefix("file://")
        && std::path::Path::new(path).is_dir()
        && filemanager_show_folder(uri)
    {
        return;
    }
    let _ = portal_open_uri(uri);
}

/// macOS (DRAGON-230): dispatch to the LaunchServices `open` body under
/// `platform/mac/` (closed split).
#[cfg(target_os = "macos")]
pub fn run_open_uri(uri: &str) {
    crate::platform::mac::open::run_open_uri(uri);
}

/// Windows (DRAGON-229): dispatch to the shell-launch body under `platform/windows/`
/// (closed split).
#[cfg(target_os = "windows")]
pub fn run_open_uri(uri: &str) {
    crate::platform::windows::services::run_open_uri(uri);
}

/// macOS (DRAGON-230): dispatch to the Finder reveal (`open -R`) body under
/// `platform/mac/` (closed split).
#[cfg(target_os = "macos")]
pub fn run_reveal(path: &Path) {
    crate::platform::mac::open::run_reveal(path);
}

/// Windows (DRAGON-229): dispatch to the Explorer reveal body under `platform/windows/`
/// (closed split).
#[cfg(target_os = "windows")]
pub fn run_reveal(path: &Path) {
    crate::platform::windows::services::run_reveal(path);
}

/// Open the default file manager with the file highlighted, falling back to
/// opening its folder via the portal.
#[cfg(target_os = "linux")]
pub fn run_reveal(path: &Path) {
    let uri = crate::util::path_to_file_uri(path);
    let shown = (|| -> Option<()> {
        let conn = zbus::blocking::Connection::session().ok()?;
        conn.call_method(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            Some("org.freedesktop.FileManager1"),
            "ShowItems",
            &(vec![uri.as_str()], ""),
        )
        .ok()?;
        Some(())
    })();
    if shown.is_none()
        && let Some(dir) = path.parent()
    {
        portal_open_uri(&crate::util::path_to_file_uri(dir));
    }
}

/// Write `content` to a temp `.ext` file and open it with the default handler (a
/// `.vcf` contact / `.ics` event). Falls back to copying the content if the write
/// fails. The temp lives in `XDG_RUNTIME_DIR` (persists for the session, so the
/// handler can read it after we exit).
pub fn save_and_open(ext: &str, content: &str) {
    let dir = crate::util::runtime_dir();
    let path = Path::new(&dir).join(format!("cosmic-capture-kit.{}.{ext}", std::process::id()));
    if std::fs::write(&path, content).is_ok() {
        open_uri(&crate::util::path_to_file_uri(path));
    } else {
        super::clipboard::copy_text(content);
    }
}
