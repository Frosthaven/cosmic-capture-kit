//! Open-URI and reveal helpers.
//!
//! THREE jobs live here and none of them is the same D-Bus call, which is what DRAGON-556
//! was: opening a real URL, opening a local FILE, and showing a FOLDER (or a file inside one)
//! in the file manager. The historical code used `OpenURI` with a string for all three, and
//! the interface documentation says plainly that `file://` URIs "are explicitly not
//! supported" by that method. Two of the three could therefore never work.
//!
//! * **A real URL** goes to the portal's `OpenURI`, which is exactly what it is for.
//! * **A local file** goes to the portal's `OpenFile`, which takes a DESCRIPTOR. The portal
//!   resolves it host-side, so it works from a process with no path in common with the host.
//! * **A folder, or a reveal**, has two routes and picks by capability:
//!   1. `org.freedesktop.FileManager1` directly. Synchronous, exact, and the only route that
//!      opens a folder ITSELF rather than its parent. Preferred wherever it can land, which
//!      is every ordinary session.
//!   2. The portal's `OpenDirectory`, a descriptor again. The portal makes the file-manager
//!      call on our behalf, which is why the Flatpak manifest deliberately does NOT grant
//!      `org.freedesktop.FileManager1`: the grant would buy nothing the portal cannot do.
//!
//! The route is chosen by a capability, [`crate::util::bus_name_reachable`], never by a
//! sandbox test: a session that can talk to the file manager keeps the direct route and
//! behaves exactly as it always has.

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

/// The file manager's own bus name, which the module doc's route 1 calls directly.
#[cfg(target_os = "linux")]
const FILE_MANAGER_NAME: &str = "org.freedesktop.FileManager1";

/// Ask the desktop's FILE MANAGER to open `dir` directly
/// (`org.freedesktop.FileManager1.ShowFolders`), the same interface [`filemanager_show_items`]
/// uses to highlight a saved capture. Returns whether the call was accepted.
///
/// This is route 1: a direct, synchronous call that simply works, and the only route that
/// opens the folder ITSELF rather than its parent. Callers must check
/// [`file_manager_reachable`] first, because a process that may not talk to this name gets a
/// plain call failure, indistinguishable from "no file manager is installed".
#[cfg(target_os = "linux")]
fn filemanager_show_folder(dir: &str) -> bool {
    (|| -> Option<()> {
        let conn = zbus::blocking::Connection::session().ok()?;
        conn.call_method(
            Some(FILE_MANAGER_NAME),
            "/org/freedesktop/FileManager1",
            Some(FILE_MANAGER_NAME),
            "ShowFolders",
            &(vec![dir], ""),
        )
        .ok()?;
        Some(())
    })()
    .is_some()
}

/// Route 1 for a FILE: open its folder with the file selected
/// (`org.freedesktop.FileManager1.ShowItems`). Same reachability rule as
/// [`filemanager_show_folder`].
#[cfg(target_os = "linux")]
fn filemanager_show_items(uri: &str) -> bool {
    (|| -> Option<()> {
        let conn = zbus::blocking::Connection::session().ok()?;
        conn.call_method(
            Some(FILE_MANAGER_NAME),
            "/org/freedesktop/FileManager1",
            Some(FILE_MANAGER_NAME),
            "ShowItems",
            &(vec![uri], ""),
        )
        .ok()?;
        Some(())
    })()
    .is_some()
}

/// Whether the DIRECT file-manager routes above can land at all from this process.
///
/// A capability, not a sandbox test. On any ordinary session this is true and the direct
/// routes run exactly as they always have. It is false only where the session bus policy
/// refuses us the name, which is the `lab/flatpak` case: the manifest withholds
/// `org.freedesktop.FileManager1` on purpose, because the portal route below reaches the
/// same file manager without the grant.
#[cfg(target_os = "linux")]
fn file_manager_reachable() -> bool {
    crate::util::bus_name_reachable(FILE_MANAGER_NAME)
}

/// Route 2: ask the OpenURI portal to show `path` in the file manager
/// (`org.freedesktop.portal.OpenURI.OpenDirectory`). Returns whether the portal accepted it.
///
/// The argument is a file DESCRIPTOR, which is what makes this work where route 1 cannot: we
/// hand over an open handle instead of a name, the portal resolves it on the host side and
/// makes the file-manager call itself. The portal opens the directory CONTAINING whatever the
/// descriptor names, with that item selected, so the path passed here is always the thing to
/// highlight, never the folder to end up in. [`folder_portal_target`] is what turns an
/// "open this folder" request into such a path.
#[cfg(target_os = "linux")]
fn portal_open_directory(path: &Path) -> bool {
    (|| -> Option<()> {
        // Read-only is enough for the portal to resolve the path, and a directory opens
        // this way on Linux just as a file does.
        let handle = std::fs::File::open(path).ok()?;
        let conn = zbus::blocking::Connection::session().ok()?;
        let opts: std::collections::HashMap<&str, zbus::zvariant::Value> =
            std::collections::HashMap::new();
        conn.call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.OpenURI"),
            "OpenDirectory",
            &("", zbus::zvariant::Fd::from(&handle), opts),
        )
        .ok()?;
        Some(())
    })()
    .is_some()
}

/// Ask the OpenURI portal to OPEN a local file with its default handler
/// (`org.freedesktop.portal.OpenURI.OpenFile`). Returns whether the portal accepted it.
///
/// A descriptor again, and for the same reason: the interface documentation says outright
/// that `file://` URIs "are explicitly not supported" by `OpenURI` and that local files go
/// through `OpenFile`. So the string form was never going to work for a file, sandboxed or
/// not. It reached exactly one caller, [`save_and_open`]'s handoff of a scanned contact or
/// calendar event, which is why nobody noticed.
#[cfg(target_os = "linux")]
fn portal_open_file(path: &Path) -> bool {
    (|| -> Option<()> {
        let handle = std::fs::File::open(path).ok()?;
        let conn = zbus::blocking::Connection::session().ok()?;
        let opts: std::collections::HashMap<&str, zbus::zvariant::Value> =
            std::collections::HashMap::new();
        conn.call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.OpenURI"),
            "OpenFile",
            &("", zbus::zvariant::Fd::from(&handle), opts),
        )
        .ok()?;
        Some(())
    })()
    .is_some()
}

/// **Pure**, unit-tested: which path an "open this folder" request should hand the portal,
/// given the folder and any one entry inside it.
///
/// The portal opens the PARENT of whatever it is given (see [`portal_open_directory`]), so
/// naming the folder itself lands the user one level too high, with the folder merely
/// selected. Naming any entry INSIDE it lands them in the folder, with that entry selected,
/// which is what the click asked for. Which entry is incidental: every entry has the same
/// parent, so any of them produces the same window.
///
/// An empty folder has nothing to name, and then the folder itself is the honest answer: the
/// parent opens with the folder highlighted, one click from where they wanted to be. That
/// beats the alternative, which is the button doing nothing at all.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn folder_portal_target<'a>(dir: &'a Path, entry: Option<&'a Path>) -> &'a Path {
    entry.unwrap_or(dir)
}

/// Open `dir` in the desktop's file manager, by whichever route this process can reach.
/// Returns whether one of them was accepted.
#[cfg(target_os = "linux")]
fn open_folder(dir: &Path) -> bool {
    if file_manager_reachable() && filemanager_show_folder(&crate::util::path_to_file_uri(dir)) {
        return true;
    }
    // `read_dir().next()` reads one entry, not the whole folder, so this stays cheap on a
    // capture directory holding thousands of files.
    let entry = std::fs::read_dir(dir)
        .ok()
        .and_then(|mut it| it.next())
        .and_then(|e| e.ok())
        .map(|e| e.path());
    let opened = portal_open_directory(folder_portal_target(dir, entry.as_deref()));
    log::debug!(
        "open folder: file manager reachable={} portal OpenDirectory accepted={opened} \
         (target {})",
        file_manager_reachable(),
        crate::diag::path_shape(dir),
    );
    opened
}

/// Helper: open `uri` with the desktop's default handler, then exit. Used for QR-code URLs
/// and for the settings pages' "open this folder" buttons.
///
/// A `file://` URI is split by what it names: a DIRECTORY goes through [`open_folder`], which
/// picks the route this process can actually use, and a FILE through [`portal_open_file`].
/// Everything else, and any failure, falls through to the portal's `OpenURI`, which is what
/// that method is for and the only one of the three that takes a real URL.
///
/// Before this split every `file://` open routed through `OpenURI` with the string, which the
/// interface documentation says is not supported for `file://` at all: the folder buttons
/// silently did nothing on COSMIC, and so did the scanned-contact handoff.
#[cfg(target_os = "linux")]
pub fn run_open_uri(uri: &str) {
    if let Some(path) = uri.strip_prefix("file://") {
        let path = std::path::Path::new(path);
        if path.is_dir() {
            if open_folder(path) {
                return;
            }
        } else if path.is_file() && portal_open_file(path) {
            return;
        }
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

/// Open the default file manager with the file highlighted.
///
/// The same routes as [`open_folder`], and here route 2 needs no adjustment at all: the
/// portal's `OpenDirectory` opens the folder CONTAINING what it is given with that item
/// selected, which is the definition of a reveal. A session that can talk to the file manager
/// takes route 1 exactly as before.
///
/// There used to be a third step: `OpenURI` on the parent folder's `file://` string. It is
/// gone because it could never have worked. The interface documentation says `file://` URIs
/// "are explicitly not supported" by `OpenURI` and directs local paths at
/// `OpenFile`/`OpenDirectory`, so that call was reporting a Request handle and opening
/// nothing, on every desktop rather than just inside a sandbox. The portal's own
/// `OpenDirectory` already falls back to `OpenURI` on the parent itself where a file manager
/// is genuinely absent, so nothing was lost by dropping our copy of that idea. Failing here
/// now leaves a log line rather than a call that pretends.
#[cfg(target_os = "linux")]
pub fn run_reveal(path: &Path) {
    let uri = crate::util::path_to_file_uri(path);
    if file_manager_reachable() && filemanager_show_items(&uri) {
        return;
    }
    if portal_open_directory(path) {
        return;
    }
    log::warn!(
        "reveal: neither the file manager nor the portal would show {}",
        crate::diag::path_shape(path),
    );
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

#[cfg(test)]
mod folder_portal_target_tests {
    use super::folder_portal_target;
    use std::path::Path;

    /// A folder with something in it names that entry, so the portal opens the FOLDER with
    /// the entry selected. This is the debug-log button's case: the log file is in there, so
    /// the click lands in the log folder with `debug.log` highlighted.
    #[test]
    fn an_entry_inside_the_folder_is_what_gets_named() {
        let dir = Path::new("/home/u/.local/state/cck/logs");
        let entry = Path::new("/home/u/.local/state/cck/logs/debug.log");
        assert_eq!(folder_portal_target(dir, Some(entry)), entry);
    }

    /// An empty folder degrades to naming itself: the parent opens with the folder
    /// highlighted. One click short of the destination, and still a visible answer.
    #[test]
    fn an_empty_folder_names_itself() {
        let dir = Path::new("/home/u/Capture");
        assert_eq!(folder_portal_target(dir, None), dir);
    }
}
