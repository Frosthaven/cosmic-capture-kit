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
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe).arg(OPEN_URI).arg(uri).spawn();
    }
}

/// Helper: open `uri` with the desktop's default handler (portal `OpenURI`), then
/// exit. Used for QR-code URLs.
#[cfg(target_os = "linux")]
pub fn run_open_uri(uri: &str) {
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
