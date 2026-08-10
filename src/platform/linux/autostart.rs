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
//! directly (nothing is installed on PATH). We write `Exec=<util::self_exe> resident`, the
//! SAME binary the resident/daemon spawn uses, so the autostart entry launches exactly
//! what the running app is — a dev build autostarts the dev build, an installed build the
//! installed one. The bare `resident` argument is the token `main()` early-branches on.
//!
//! `util::self_exe`, NOT `current_exe` (DRAGON-510). This entry is read at the NEXT login,
//! and inside an AppImage `current_exe()` names a FUSE mount that stops existing the moment
//! this process ends. Writing it here would produce an autostart entry that is dead before
//! it is ever used; `$APPIMAGE` is the file, which is still there tomorrow.
//!
//! ## Inside a Flatpak (DRAGON-618)
//!
//! Autostart was silently a no-op in the Flatpak build, and it took TWO independent
//! mistakes, either of which alone was enough to break it:
//!
//! 1. **The wrong directory.** [`autostart_dir`] asked `dirs::config_dir()`, which inside a
//!    sandbox is the app's PRIVATE store, so the entry went to
//!    `~/.var/app/<id>/config/autostart/`. No desktop session has ever scanned that.
//! 2. **The wrong command.** `Exec=` named `/app/bin/cosmic-capture-kit`, a path that exists
//!    only for processes already inside the sandbox and means nothing to the host session
//!    that would run it.
//!
//! Both are fixed here: the directory comes from `util::host_config_dir` and the command from
//! [`autostart_exec`], which spells a Flatpak launch `flatpak run <app-id> resident`.
//!
//! 3. **The wrong mechanism.** Even correct on both counts, a direct write depends on the
//!    sandbox being able to SEE `~/.config`, which only the lab manifest's (LAB-ONLY)
//!    `--filesystem=home` allows. A shippable manifest drops that grant and the write fails.
//!
//! So a Flatpak does not write the file at all. It asks the **Background portal**
//! (`org.freedesktop.portal.Background`, `RequestBackground` with `autostart: true`), which
//! has the HOST write the entry and needs no filesystem grant and no `--talk-name` (portal
//! names are allowed by default, and adding one for a portal is itself a lint error).
//! [`autostart_mechanism`] is the split, and [`request_background_autostart`] the call.
//!
//! That made registration ASYNC and possibly interactive, so `App::reconcile_login_item`
//! returns a `Task` and settles the toggle from the portal's answer. Every other package kind,
//! and macOS and Windows, still take the synchronous file/registry path unchanged.
//!
//! **Two commands, pointing opposite ways.** [`HostExecLine`] is what WE write into a desktop
//! file, and it must be runnable by the host (`flatpak run <app-id> resident`).
//! [`SandboxArgv`] is what the PORTAL is given, and it must be the in-sandbox command
//! (`cosmic-capture-kit resident`), because the portal adds the `flatpak run` wrapper itself.
//! They are separate types precisely so the two can never be swapped.

#![cfg(target_os = "linux")]

use crate::platform::AutostartRegistration as Registration;
use crate::util::{BIN_NAME, PackageKind};
use std::path::{Path, PathBuf};

/// The autostart `.desktop` file's basename (stable, so [`set`] / [`is_enabled`] agree
/// and a later toggle-off removes the exact file a toggle-on wrote).
const DESKTOP_BASENAME: &str = "cosmic-capture-kit.desktop";

/// The bare argument `main()` early-branches on to become the resident. Kept here next to
/// the `Exec=` writer so the autostart command and the branch token can't drift.
const RESIDENT_ARG: &str = "resident";

/// The autostart directory the SESSION actually reads: `~/.config/autostart` on the HOST.
///
/// `util::host_config_dir()`, NOT `dirs::config_dir()` (DRAGON-618). Outside a sandbox the two
/// are the same call and nothing changes for an ordinary build. Inside a Flatpak they are not,
/// and the difference was the whole bug: `$XDG_CONFIG_HOME` points at the app's private store,
/// so the entry landed in `~/.var/app/<id>/config/autostart/` where nothing reads it. The
/// toggle went on, the write genuinely succeeded, and nothing ever started. Confirmed on the
/// owner's machine, where exactly such an entry had sat unused since 2026-08-08.
fn autostart_dir() -> Option<PathBuf> {
    crate::util::host_config_dir().map(|d| d.join("autostart"))
}

/// Which mechanism registers autostart for a given package kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// Write `~/.config/autostart/<id>.desktop` ourselves. Every non-sandboxed kind.
    DesktopFile,
    /// Ask `org.freedesktop.portal.Background` to register it. Flatpak only, because the
    /// sandbox has no business writing the host's autostart directory.
    BackgroundPortal,
}

/// **Pure**, unit-tested: which mechanism this build must use.
///
/// Every kind is NAMED rather than swept up by a `_` wildcard, and that is deliberate. When
/// `PackageKind` gained `MacOs` and `Windows` (DRAGON-614) this stopped compiling, which is
/// exactly the right outcome: a new package kind is a question about how it registers
/// autostart, and a wildcard would have answered it silently with "write a desktop file".
///
/// `MacOs` and `Windows` cannot actually reach this module, which is `#![cfg(target_os =
/// "linux")]`, and they have their own login-item backends (`mac::login_item`,
/// `windows_autostart`). They map to the file path only so the function is total.
pub fn autostart_mechanism(kind: PackageKind) -> Mechanism {
    match kind {
        PackageKind::Flatpak => Mechanism::BackgroundPortal,
        PackageKind::Binary
        | PackageKind::AppImage
        | PackageKind::MacOs
        | PackageKind::Windows => Mechanism::DesktopFile,
    }
}

/// The `Exec=` line for an autostart entry **we** write, which the HOST session runs.
///
/// A newtype rather than a `String`, and [`SandboxArgv`] is its deliberate opposite number.
/// The two carry the same idea ("how to relaunch us as the resident") with INVERTED contents,
/// and swapping them silently produces an entry that either cannot run or runs the wrong
/// thing. Distinct types mean the compiler refuses the swap instead of a reviewer catching it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostExecLine(String);

/// The argv the Background portal launches **inside** the sandbox.
///
/// The inverse of [`HostExecLine`]: the portal writes the host-side entry itself and adds the
/// `flatpak run --command=… <app-id>` wrapper, so what it wants here is the plain in-sandbox
/// command. Handing it a host command line would produce `flatpak run … flatpak run …`.
///
/// **It can only be built by [`sandbox_argv`], which takes no arguments and always appends
/// [`RESIDENT_ARG`].** That is not tidiness, it is the guard on the worst failure mode this
/// module has: see `sandbox_argv`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SandboxArgv(Vec<String>);

/// **Pure**, unit-tested: the in-sandbox argv the Background portal should autostart.
///
/// # Why this takes no arguments
///
/// `RequestBackground`'s `commandline` option is OPTIONAL, and omitting it is catastrophic
/// rather than merely wrong. The portal then falls back to the `Exec=` line of our INSTALLED
/// desktop file, `res/dev.thedragon.CosmicCaptureKit.desktop`, which reads:
///
/// ```text
/// Exec=cosmic-capture-kit
/// ```
///
/// A bare launch with no [`RESIDENT_ARG`] does not become the tray daemon. It starts a
/// CAPTURE. So an omitted commandline would fire a screenshot overlay at every login, on every
/// machine with the setting on, which is far worse than the no-op bug this module was fixing.
///
/// The guard is structural. There is no parameter to forget, no `Option` to pass `None` for,
/// and [`SandboxArgv`] has no other constructor, so a future edit cannot drop the argument and
/// silently inherit the fallback. Nothing here should be relaxed into taking a caller-supplied
/// command without re-reading this paragraph.
///
/// The binary NAME rather than a path: inside the sandbox `/app/bin` is on `PATH`, and the
/// portal's wrapper resolves it there.
fn sandbox_argv() -> SandboxArgv {
    SandboxArgv(vec![BIN_NAME.to_string(), RESIDENT_ARG.to_string()])
}

/// **Pure**, unit-tested: the `Exec=` line for the entry we write, given how this build was
/// delivered.
///
/// Two spellings, because what the HOST SESSION can launch is not what this process can.
///
/// * **Flatpak**: `flatpak run <app-id> resident`. The binary's own path inside the sandbox
///   (`/app/bin/cosmic-capture-kit`) exists ONLY for processes already in that sandbox, so an
///   entry naming it is unrunnable by the session that reads it. Not hypothetical: the entry
///   this replaces really did say `Exec=/app/bin/cosmic-capture-kit resident`. **Not reached in
///   production any more**, since [`autostart_mechanism`] sends a Flatpak to the portal and
///   `set` is only called on the file path. The arm stays so the function is total and so the
///   contrast with [`sandbox_argv`] is stated and TESTED in one place: these two are the
///   opposite-facing halves of the same question, and the test pair is what stops a future
///   edit quietly using one where the other belongs.
/// * **Binary / AppImage**: the executable's own absolute path, byte-identical to what this
///   function has always written.
///
/// `None` means there is nothing runnable to name, which [`set`] turns into an honest error
/// rather than an entry that cannot work.
///
/// Deliberately NOT `util::launch_command`, though the two draw the same distinction. That one
/// builds a line for a HUMAN to read and paste, so it collapses the home directory to `~` and
/// shell-quotes. A desktop entry's `Exec=` is read by the session, which does not expand `~`,
/// so borrowing it would write a path that resolves nowhere.
fn host_exec_line(kind: PackageKind, exe: Option<&Path>, app_id: &str) -> Option<HostExecLine> {
    match kind {
        PackageKind::Flatpak => Some(HostExecLine(format!(
            "flatpak run {app_id} {RESIDENT_ARG}"
        ))),
        // Named rather than wildcarded for the same reason as `autostart_mechanism`: a new
        // package kind must be a compile error here, not a silent default. macOS and Windows
        // cannot reach this Linux-only module and never write a `.desktop` entry.
        PackageKind::Binary
        | PackageKind::AppImage
        | PackageKind::MacOs
        | PackageKind::Windows => {
            Some(HostExecLine(format!("{} {RESIDENT_ARG}", exe?.display())))
        }
    }
}

/// **Pure**, unit-tested: what the "Automatically start on login" toggle should READ after a
/// registration attempt settles.
///
/// The contract is that the toggle never claims something that did not happen, which covers
/// three different endings with one rule: the portal answered and said yes, the portal
/// answered and said no (the user declined the dialog), or the request failed outright. Only
/// the first is on. An optimistic toggle that never reconciles would be the original
/// silent-success defect wearing a different hat.
pub fn settled_toggle(response: &Result<bool, String>) -> bool {
    match response {
        Ok(granted) => *granted,
        Err(_) => false,
    }
}

/// **Pure**, unit-tested: what the autostart preference should BECOME once a portal
/// request settles, or `None` when it must be left alone.
///
/// The `resident` guard is the whole point of this function, and it is not a detail
/// (DRAGON-625). The synchronous file path has always re-derived the toggle only when the
/// tray is ON, because with the tray off the login item is unregistered BY DESIGN and the
/// user's preference is being kept for when they turn the tray back on. The portal path
/// shipped without that guard: turning the tray off asked the portal for `want = false`,
/// got a perfectly truthful `Ok(false)` back, and wrote that over the preference. So
/// turning the tray off and on again silently destroyed "start on login".
///
/// `None` means do not touch the preference; `Some(v)` means set it to `v`.
pub fn settled_preference(
    resident: bool,
    current: bool,
    response: &Result<bool, String>,
) -> Option<bool> {
    if !resident {
        return None;
    }
    let granted = settled_toggle(response);
    (current != granted).then_some(granted)
}

/// **Pure**, unit-tested: what to tell the USER when a Background-portal request fails.
///
/// "the background portal is unavailable: A portal frontend implementing
/// `org.freedesktop.portal.Background` was not found" is accurate and tells a user
/// nothing (DRAGON-625). The raw text keeps going to the debug log, where it is for us.
/// This is the sentence for the person who just watched a toggle refuse to turn on.
///
/// `missing_backend` separates the two endings that need different words:
///
/// * `true`: this desktop has no Background portal at all. COSMIC today is exactly that
///   case. `xdg-desktop-portal-cosmic` declares Access, FileChooser, Screenshot, Settings
///   and ScreenCast, and Background is not among them, so the interface is never exported
///   and the request cannot even be made. The only installed backend that does implement
///   it is the KDE one, which a COSMIC session never selects.
/// * `false`: a portal is there and the request did not come back.
///
/// Kept deliberately short, in the settings-copy style: plain, terse, no jargon and no
/// D-Bus interface names.
pub fn portal_failure_message(missing_backend: bool) -> &'static str {
    if missing_backend {
        "This desktop cannot start Flatpak apps at login yet."
    } else {
        "The desktop's background service did not answer."
    }
}

/// Ask the Background portal to register (or unregister) autostart for this app.
///
/// Returns whether autostart is registered AFTER the request, which is not the same as what
/// was asked for: the user can decline the dialog. The caller settles the toggle from the
/// answer through [`settled_toggle`].
///
/// The portal remembers its decision per app id, so a repeat request for an already-granted
/// app is normally silent. That is the same behaviour `platform::global_shortcuts` documents
/// for its own one-time dialog, and it is why this path does not bother avoiding a redundant
/// request: it fires at most once per user click.
///
/// `dbus_activatable(false)`: we are not a D-Bus activatable service, so the portal must write
/// a real `Exec=` entry rather than expect an activation name.
#[cfg(target_os = "linux")]
pub async fn request_background_autostart(want: bool) -> Result<bool, String> {
    use ashpd::desktop::background::Background;

    // `sandbox_argv()` and not a caller-supplied command: see its doc for why the argument
    // list is not a parameter anywhere in this module.
    let SandboxArgv(argv) = sandbox_argv();
    let response = Background::request()
        .reason("Start Cosmic Capture Kit's tray icon when you log in")
        .auto_start(want)
        .command(&argv)
        .dbus_activatable(false)
        .send()
        .await
        .map_err(|e| {
            // The raw portal error is for US, in the debug log. The returned string is
            // what the user is shown, so it says what the failure MEANS (DRAGON-625).
            // `PortalNotFound` is matched, not string-sniffed: ashpd gives the missing
            // interface its own variant, so the distinction cannot rot with its wording.
            log::debug!("background portal request failed: {e}");
            portal_failure_message(matches!(e, ashpd::Error::PortalNotFound(_))).to_string()
        })?
        .response()
        .map_err(|e| {
            log::debug!("background portal returned no grant: {e}");
            "The request to start at login was declined.".to_string()
        })?;
    Ok(response.auto_start())
}

/// The full path to our autostart entry, if the config dir resolves.
fn desktop_path() -> Option<PathBuf> {
    autostart_dir().map(|d| d.join(DESKTOP_BASENAME))
}

/// The `.desktop` file contents launching this build as the resident. Autostart entries must
/// be a valid desktop entry; `X-GNOME-Autostart-enabled=true` and the COSMIC/GNOME/KDE-honoured
/// keys keep it enabled across desktops.
///
/// Takes the finished [`HostExecLine`] rather than a bare executable path, because a Flatpak's
/// command is not a path at all, and rather than a `String`, so a [`SandboxArgv`] intended for
/// the portal can never be spliced in here by mistake.
fn desktop_contents(HostExecLine(exec): &HostExecLine) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Cosmic Capture Kit\n\
         Comment=Keep Cosmic Capture Kit running in the background\n\
         Exec={exec}\n\
         Icon=dev.thedragon.CosmicCaptureKit\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// **Pure**, unit-tested: every basename an autostart entry for this build could carry.
///
/// Two names exist because two mechanisms write the file and only one of them is ours.
/// [`set`] writes [`DESKTOP_BASENAME`]. The Background portal names the entry after the
/// APP ID instead, and it picks that itself: `RequestBackground` takes no filename. So on
/// a Flatpak the registration can legitimately sit under either name, and looking only
/// for ours reports a registered app as off. DRAGON-625: the two silently disagreed.
///
/// Ours is listed LAST, so a portal-written entry is the one that answers on the single
/// package kind that can have both.
fn entry_basenames(kind: PackageKind) -> Vec<String> {
    match kind {
        PackageKind::Flatpak => vec![
            format!("{}.desktop", crate::util::FLATPAK_APP_ID),
            DESKTOP_BASENAME.to_string(),
        ],
        // Named rather than wildcarded for the same reason as `autostart_mechanism`.
        PackageKind::Binary
        | PackageKind::AppImage
        | PackageKind::MacOs
        | PackageKind::Windows => vec![DESKTOP_BASENAME.to_string()],
    }
}

/// **Pure**, unit-tested: whether an autostart entry can still RUN, given its contents.
///
/// A `.desktop` file existing is not the same as launch-at-login working, and the gap
/// between the two is a defect this module already shipped (DRAGON-625). `Exec=` carries
/// an absolute path recorded when the toggle was flipped, and the artifact it names can
/// move. On the owner's machine DRAGON-590 relocated the AppImage two hours after the
/// entry was written, and every login since refused it:
///
/// ```text
/// systemd-xdg-autostart-generator: /home/…/cosmic-capture-kit.desktop:
///   not generating unit, executable specified in Exec= does not exist.
/// ```
///
/// while [`is_enabled`] kept answering `true` off a bare `exists()` and the settings
/// toggle kept claiming the feature was on. This asks the question the session asks.
///
/// The rules, in order:
///
/// * No `Exec=` line at all: not runnable, so `false`.
/// * Our own writer always ends the line with the bare [`RESIDENT_ARG`], and a path may
///   contain spaces, so the WHOLE command minus that token is tried as a path first.
/// * Then the first whitespace-separated token, which is what a portal-written
///   `Exec=/usr/bin/flatpak run … resident` needs.
/// * A candidate that is not absolute is a `PATH` lookup (`flatpak run …`). The session's
///   `PATH` cannot be honestly reproduced from inside a sandbox, so that reads as LIVE.
///
/// So this only ever calls an entry dead when it names an ABSOLUTE path that is not
/// there, which is exactly the case the generator refuses. It never guesses.
fn entry_is_live_with(contents: &str, exists: impl Fn(&Path) -> bool) -> bool {
    let Some(exec) = contents
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("Exec="))
    else {
        return false;
    };
    let exec = exec.trim();
    let whole = exec
        .strip_suffix(RESIDENT_ARG)
        .map(str::trim_end)
        .unwrap_or(exec);
    let Some(first) = exec.split_whitespace().next() else {
        return false;
    };
    for cand in [whole, first] {
        // Not a path: a PATH lookup we cannot resolve, so never call it dead.
        if !cand.starts_with('/') {
            return true;
        }
        if exists(Path::new(cand)) {
            return true;
        }
    }
    false
}

/// **Pure**, unit-tested: fold the entries that EXIST into one registration state.
///
/// `present` is one flag per entry file that could be read, saying whether that entry is
/// live. The empty slice therefore means no entry exists at all, which is
/// [`Absent`](Registration::Absent), and it is a genuinely different fact from a file that
/// exists and cannot run (DRAGON-628): opening the settings window repairs the second and
/// must never invent the first. Any live entry wins, since one runnable entry is enough for
/// the session to start us.
fn registration_from(present: &[bool]) -> Registration {
    if present.is_empty() {
        Registration::Absent
    } else if present.iter().any(|&live| live) {
        Registration::Live
    } else {
        Registration::Stale
    }
}

/// What the session has on file for us: no entry, a dead entry, or a runnable one.
///
/// Never a panic. A missing config dir, or a directory we cannot read, reports
/// [`Absent`](Registration::Absent), which is the conservative answer: it is the one state
/// that never authorises a write on settings-open.
///
/// **A stale entry reads as not-live, and nothing here deletes it** (DRAGON-625). That is the
/// self-healing answer rather than a destructive one: `App::reconcile_login_item` compares
/// this against what the user wants, and where it used to short-circuit on "the file is
/// there" it now sees a difference and calls [`set`], which REWRITES the entry with this
/// build's current path. So a stale entry is repaired in place, either the next time a toggle
/// is touched or the next time the settings window opens. We never remove a file from the
/// user's home to make a setting look tidy.
pub fn registration() -> Registration {
    let Some(dir) = autostart_dir() else {
        return Registration::Absent;
    };
    let present: Vec<bool> = entry_basenames(crate::util::package_kind())
        .iter()
        .filter_map(|name| std::fs::read_to_string(dir.join(name)).ok())
        .map(|c| entry_is_live_with(&c, |p| p.exists()))
        .collect();
    registration_from(&present)
}

/// Whether launch-at-login is actually registered: an entry exists under one of
/// [`entry_basenames`] AND still names something runnable ([`entry_is_live_with`]).
///
/// Derived from [`registration`] rather than reading the directory a second time, so the
/// boolean the reconcile compares against and the three-state the settings-open repair
/// consults can never disagree about the same files.
pub fn is_enabled() -> bool {
    registration().is_live()
}

/// Create (`on`) or remove (`off`) the autostart entry. On create, writes the `.desktop`
/// file with `Exec=<util::self_exe> resident` (creating `~/.config/autostart` if needed); on
/// remove, deletes it (a missing file is success — the desired state is "absent"). Returns
/// an honest error string on an I/O failure so the settings UI can surface it. Mirrors
/// `login_item::set`.
pub fn set(on: bool) -> Result<(), String> {
    let path = desktop_path().ok_or_else(|| "could not resolve the autostart directory".to_string())?;
    if on {
        // A Flatpak needs no executable path at all (its command is `flatpak run <app-id>`),
        // so `self_exe` is only consulted for the kinds that do, and its failure is only an
        // error for those. Asking for it unconditionally would fail a Flatpak launch on a
        // value it was never going to use.
        let exe = crate::util::self_exe().ok();
        let exec = host_exec_line(
            crate::util::package_kind(),
            exe.as_deref(),
            crate::util::FLATPAK_APP_ID,
        )
        .ok_or_else(|| "could not resolve the current executable".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, desktop_contents(&exec))
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

    /// DRAGON-618: a Flatpak's entry must name a command the HOST can run. The binary's own
    /// in-sandbox path is meaningless to the session that reads the file, and writing it is
    /// exactly what made autostart a silent no-op.
    /// Only a Flatpak may use the portal, and every other kind must keep writing the file.
    /// A wrong answer here either bypasses the sandbox rule or breaks a working build.
    #[test]
    fn only_a_flatpak_uses_the_background_portal() {
        assert_eq!(
            autostart_mechanism(PackageKind::Flatpak),
            Mechanism::BackgroundPortal
        );
        assert_eq!(
            autostart_mechanism(PackageKind::Binary),
            Mechanism::DesktopFile
        );
        assert_eq!(
            autostart_mechanism(PackageKind::AppImage),
            Mechanism::DesktopFile
        );
        // Unreachable in this Linux-only module, but the function is total and the answer
        // must not be the portal: those platforms have their own login-item backends.
        assert_eq!(
            autostart_mechanism(PackageKind::MacOs),
            Mechanism::DesktopFile
        );
        assert_eq!(
            autostart_mechanism(PackageKind::Windows),
            Mechanism::DesktopFile
        );
    }

    /// THE guard on the worst failure mode in this module. If the portal is ever handed a
    /// command without the resident token, it falls back to our desktop file's bare
    /// `Exec=cosmic-capture-kit`, and every login fires a CAPTURE overlay instead of starting
    /// the tray daemon. `sandbox_argv` takes no arguments so this cannot be dropped by an
    /// edit, and this test pins the contents in case someone rewrites it anyway.
    #[test]
    fn the_portal_argv_always_carries_the_resident_token() {
        let SandboxArgv(argv) = sandbox_argv();
        assert_eq!(argv, vec![BIN_NAME.to_string(), RESIDENT_ARG.to_string()]);
        assert!(
            argv.iter().any(|a| a == RESIDENT_ARG),
            "without the resident token the portal autostarts a CAPTURE, not the daemon"
        );
        // The portal wraps this itself; handing it a host command line would produce a
        // nested `flatpak run … flatpak run …`.
        assert!(
            !argv.iter().any(|a| a.contains("flatpak")),
            "the portal wants the IN-SANDBOX argv, not the host launch command"
        );
    }

    /// The toggle reads as ON only when the registration actually happened. A decline and a
    /// failure are both OFF, which is the whole honest-failure contract.
    #[test]
    fn the_toggle_only_reads_on_when_registration_succeeded() {
        assert!(settled_toggle(&Ok(true)));
        assert!(!settled_toggle(&Ok(false)), "the user declined the dialog");
        assert!(!settled_toggle(&Err("portal unavailable".into())));
    }

    #[test]
    fn a_flatpak_autostarts_through_the_flatpak_command() {
        let got = host_exec_line(
            PackageKind::Flatpak,
            // The in-sandbox path, which must NOT appear in the result even though it is
            // what `self_exe()` really returns inside the sandbox.
            Some(Path::new("/app/bin/cosmic-capture-kit")),
            "dev.thedragon.CosmicCaptureKit",
        );
        assert_eq!(
            got,
            Some(HostExecLine(
                "flatpak run dev.thedragon.CosmicCaptureKit resident".to_string()
            ))
        );
        assert!(
            !got.unwrap().0.contains("/app/bin"),
            "the sandbox path must never reach the host's autostart entry"
        );
    }

    /// The two kinds that DO launch by path keep exactly the command they always had.
    #[test]
    fn a_binary_and_an_appimage_autostart_by_path() {
        for kind in [PackageKind::Binary, PackageKind::AppImage] {
            assert_eq!(
                host_exec_line(kind, Some(Path::new("/opt/cck/cosmic-capture-kit")), "ignored"),
                Some(HostExecLine("/opt/cck/cosmic-capture-kit resident".to_string())),
                "{kind:?} must launch the executable directly"
            );
        }
    }

    /// No executable and no Flatpak means nothing runnable to write, which `set` reports as
    /// an error instead of writing an entry that cannot work.
    #[test]
    fn no_executable_yields_no_command() {
        assert_eq!(host_exec_line(PackageKind::Binary, None, "ignored"), None);
        assert_eq!(host_exec_line(PackageKind::AppImage, None, "ignored"), None);
        // A Flatpak needs no path, so it still produces a command.
        assert!(host_exec_line(PackageKind::Flatpak, None, "app.id").is_some());
    }

    #[test]
    fn desktop_contents_launches_the_exe_as_resident() {
        let c = desktop_contents(&HostExecLine("/opt/cck/cosmic-capture-kit resident".to_string()));
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

/// DRAGON-625: the entry we look for, and whether it can still run.
#[cfg(test)]
mod entry_liveness_tests {
    use super::*;

    fn entry(exec: &str) -> String {
        desktop_contents(&HostExecLine(exec.to_string()))
    }

    /// The portal names the file after the APP ID and we name it after the binary. A
    /// Flatpak must look for both or a registered app reads as off.
    #[test]
    fn a_flatpak_looks_for_the_portals_name_too() {
        let names = entry_basenames(PackageKind::Flatpak);
        assert_eq!(
            names,
            vec![
                "dev.thedragon.CosmicCaptureKit.desktop".to_string(),
                "cosmic-capture-kit.desktop".to_string(),
            ],
            "the portal's app-id name must be checked, and first"
        );
        for kind in [PackageKind::Binary, PackageKind::AppImage] {
            assert_eq!(
                entry_basenames(kind),
                vec!["cosmic-capture-kit.desktop".to_string()],
                "{kind:?} never has a portal-written entry"
            );
        }
    }

    /// THE regression this closes. The owner's entry named an AppImage that DRAGON-590
    /// moved, and `is_enabled` answered true off a bare `exists()` for a file the session
    /// refuses at every login.
    #[test]
    fn an_exec_naming_a_missing_binary_is_not_live() {
        let c = entry("/gone/CosmicCaptureKit-x86_64.AppImage resident");
        assert!(
            !entry_is_live_with(&c, |_| false),
            "a dead absolute Exec must read as not registered"
        );
        assert!(
            entry_is_live_with(&c, |_| true),
            "the same entry is live once the binary is back"
        );
    }

    /// A path with spaces must not be truncated at the first space, or a legitimate
    /// entry would read as dead forever and be rewritten on every reconcile.
    #[test]
    fn a_path_with_spaces_survives() {
        let c = entry("/opt/my apps/cosmic-capture-kit resident");
        assert!(entry_is_live_with(&c, |p| p
            == Path::new("/opt/my apps/cosmic-capture-kit")));
    }

    /// `flatpak run …` is a PATH lookup we cannot resolve from inside a sandbox, so it
    /// must never be called dead. Guessing here would disable a working entry.
    #[test]
    fn a_path_lookup_is_never_called_dead() {
        let c = entry("flatpak run dev.thedragon.CosmicCaptureKit resident");
        assert!(entry_is_live_with(&c, |_| false));
    }

    /// What the PORTAL writes: an absolute launcher plus arguments. The whole string is
    /// not a path, so the first token has to be tried too.
    #[test]
    fn a_portal_written_entry_resolves_through_its_first_token() {
        let c = entry("/usr/bin/flatpak run --command=cosmic-capture-kit dev.thedragon.CosmicCaptureKit resident");
        assert!(
            entry_is_live_with(&c, |p| p == Path::new("/usr/bin/flatpak")),
            "the launcher exists, so the entry is live"
        );
    }

    #[test]
    fn a_file_without_an_exec_line_is_not_live() {
        assert!(!entry_is_live_with("[Desktop Entry]\nType=Application\n", |_| true));
    }

    /// DRAGON-628: no entry at all and a dead entry are DIFFERENT facts. The resident daemon
    /// rewrites the second and must never invent the first, because an absent entry may be
    /// one the user removed in their desktop's autostart editor.
    #[test]
    fn absent_and_stale_are_not_the_same_state() {
        assert_eq!(registration_from(&[]), Registration::Absent);
        assert_eq!(registration_from(&[false]), Registration::Stale);
        assert_eq!(registration_from(&[true]), Registration::Live);
    }

    /// A Flatpak can carry two entries (ours and the portal's). One runnable entry is enough
    /// for the session to start us, so any live one wins and the pair is not called stale.
    #[test]
    fn one_live_entry_is_enough_however_many_dead_ones_sit_beside_it() {
        assert_eq!(registration_from(&[false, true]), Registration::Live);
        assert_eq!(registration_from(&[true, false]), Registration::Live);
        assert_eq!(registration_from(&[false, false]), Registration::Stale);
    }
}

/// DRAGON-625: settling the toggle, and what the user is told when it fails.
#[cfg(test)]
mod portal_settle_tests {
    use super::*;

    /// The defect: with the tray OFF the portal is asked for `false`, answers `false`
    /// truthfully, and that used to overwrite the user's preference. Turning the tray off
    /// and on again destroyed "start on login".
    #[test]
    fn a_tray_off_answer_never_touches_the_preference() {
        assert_eq!(settled_preference(false, true, &Ok(false)), None);
        assert_eq!(settled_preference(false, true, &Ok(true)), None);
        assert_eq!(settled_preference(false, true, &Err("boom".into())), None);
    }

    /// With the tray on, the toggle follows what the portal actually granted.
    #[test]
    fn a_tray_on_answer_settles_the_toggle() {
        // Asked for on, granted: already true, so nothing to write.
        assert_eq!(settled_preference(true, true, &Ok(true)), None);
        // Asked for on, declined: falls back to off.
        assert_eq!(settled_preference(true, true, &Ok(false)), Some(false));
        // Asked for on, failed outright: same honest fallback.
        assert_eq!(settled_preference(true, true, &Err("boom".into())), Some(false));
        // Granted while the toggle read off: comes back on.
        assert_eq!(settled_preference(true, false, &Ok(true)), Some(true));
    }

    /// The user-facing copy must say what happened to THEM, with no interface names.
    #[test]
    fn the_failure_copy_carries_no_jargon() {
        for missing in [true, false] {
            let m = portal_failure_message(missing);
            assert!(!m.is_empty());
            assert!(m.ends_with('.'), "settings copy is a sentence: {m:?}");
            for jargon in ["portal", "D-Bus", "dbus", "org.freedesktop", "frontend"] {
                assert!(
                    !m.to_ascii_lowercase().contains(&jargon.to_ascii_lowercase()),
                    "{m:?} leaks the implementation word {jargon:?}"
                );
            }
        }
        // The two endings must not read the same, or the split is pointless.
        assert_ne!(portal_failure_message(true), portal_failure_message(false));
    }
}
