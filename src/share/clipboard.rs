//! Clipboard helpers: copy image/file/text data to the clipboard.
//!
//! Linux serves the Wayland selection from a detached re-exec worker (the
//! process must own the selection until something else is copied). macOS/Windows
//! NSPasteboard/CF_DIB writes PERSIST after the process exits, so there the copy
//! happens INLINE in the spawn-side functions below (no re-exec worker); the
//! `run_copy*` workers stay no-ops there.

use std::path::Path;

#[cfg(target_os = "linux")]
use super::reexec::{COPY_FILE, COPY_IMAGE, COPY_TEXT, spawn_self};

/// Copy the capture to the clipboard (image data for a screenshot, a file
/// reference for a recording). Detached; persists after we exit.
#[cfg(target_os = "linux")]
pub fn copy_to_clipboard(path: &Path, is_video: bool) {
    spawn_self(if is_video { COPY_FILE } else { COPY_IMAGE }, path);
}

/// macOS (DRAGON-230): dispatch to the clipboard body under `platform/mac/` (closed
/// split). A screenshot goes on as decoded RGBA image data, a recording (or any
/// non-image path) as a `file://` URL; like Windows the write persists after we exit,
/// so the copy is INLINE here and `run_copy` stays a no-op.
#[cfg(target_os = "macos")]
pub fn copy_to_clipboard(path: &Path, is_video: bool) {
    crate::platform::mac::clipboard::copy_to_clipboard(path, is_video);
}

/// Windows (DRAGON-229): dispatch to the clipboard body under `platform/windows/`
/// (closed split). A still image goes on as image data (PNG + CF_DIBV5), a recording
/// as a CF_HDROP file list; like macOS the write persists after we exit, so the copy
/// is INLINE here and `run_copy` stays a no-op.
#[cfg(target_os = "windows")]
pub fn copy_to_clipboard(path: &Path, is_video: bool) {
    crate::platform::windows::services::copy_to_clipboard(path, is_video);
}

/// Copy plain text to the clipboard (detached; persists after we exit). The text
/// is handed to the helper via a temp file so any length or special characters are
/// safe (vs. passing it as an argv).
#[cfg(target_os = "linux")]
pub fn copy_text(text: &str) {
    let dir = crate::util::runtime_dir();
    let path = Path::new(&dir).join(format!(
        "cosmic-capture-kit.{}.cliptext",
        std::process::id()
    ));
    if std::fs::write(&path, text).is_ok() {
        spawn_self(COPY_TEXT, &path);
    }
}

/// macOS (DRAGON-230): dispatch to the arboard text write under `platform/mac/`
/// (closed split). Inline; persists after exit, so `run_copy_text` stays a no-op.
#[cfg(target_os = "macos")]
pub fn copy_text(text: &str) {
    crate::platform::mac::clipboard::copy_text(text);
}

/// Windows (DRAGON-229): dispatch to the arboard text write under `platform/windows/`
/// (closed split). Like macOS, the write persists after we exit, so `run_copy_text`
/// stays a no-op.
#[cfg(target_os = "windows")]
pub fn copy_text(text: &str) {
    crate::platform::windows::services::copy_text(text);
}

/// macOS/Windows: the copy already happened INLINE in the spawn-side functions
/// (NSPasteboard/CF_DIB writes persist without our process owning the selection),
/// so this re-exec worker has nothing to serve.
#[cfg(not(target_os = "linux"))]
pub fn run_copy(_path: &Path, _is_video: bool) {
    log::debug!("run_copy no-op: macOS/Windows copy happens inline (persists after exit)");
}

/// Serve the clipboard (foreground = this process owns it until something else
/// is copied, then it exits).
#[cfg(target_os = "linux")]
pub fn run_copy(path: &Path, is_video: bool) {
    use wl_clipboard_rs::copy::{MimeType, Options, Source};

    let (data, mime) = if is_video {
        (
            format!("{}\r\n", crate::util::path_to_file_uri(path)).into_bytes(),
            "text/uri-list",
        )
    } else {
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        (bytes, "image/png")
    };

    let mut opts = Options::new();
    opts.foreground(true); // block here serving until the clipboard is replaced
    let _ = opts.copy(
        Source::Bytes(data.into_boxed_slice()),
        MimeType::Specific(mime.to_string()),
    );
}

/// macOS/Windows: the text copy already happened INLINE (persists after exit), so
/// this re-exec worker is a no-op.
#[cfg(not(target_os = "linux"))]
pub fn run_copy_text(_path: &Path) {
    log::debug!("run_copy_text no-op: macOS/Windows copy happens inline (persists after exit)");
}

/// Serve the plain text in `path` on the clipboard (foreground = owns it until
/// replaced). Consumes the temp file. `MimeType::Text` advertises the standard
/// text targets so every app can paste it.
#[cfg(target_os = "linux")]
pub fn run_copy_text(path: &Path) {
    use wl_clipboard_rs::copy::{MimeType, Options, Source};
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let _ = std::fs::remove_file(path);
    let mut opts = Options::new();
    opts.foreground(true);
    let _ = opts.copy(Source::Bytes(bytes.into_boxed_slice()), MimeType::Text);
}
