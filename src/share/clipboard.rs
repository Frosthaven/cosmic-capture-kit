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

/// macOS: NSPasteboard writes persist after we exit, so the copy happens INLINE
/// here — no foreground-owner worker (`run_copy` stays a no-op). A screenshot goes
/// on as decoded RGBA image data (`arboard`); a recording — or any non-image path
/// — as a `file://` URL (NSPasteboard `writeObjects:`). Fire-and-forget: every
/// failure is logged, never panics (call sites surface no error).
#[cfg(target_os = "macos")]
pub fn copy_to_clipboard(path: &Path, is_video: bool) {
    let result = if is_video || !is_image_path(path) {
        mac::copy_file_url(path)
    } else {
        mac::copy_image(path)
    };
    if let Err(e) = result {
        log::warn!("macOS clipboard copy failed ({}): {e}", path.display());
    }
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

/// macOS: `arboard` text write, inline (persists after exit). Fire-and-forget.
#[cfg(target_os = "macos")]
pub fn copy_text(text: &str) {
    if let Err(e) = mac::copy_text(text) {
        log::warn!("macOS clipboard text copy failed: {e}");
    }
}

/// Whether `path` names an image the still-image clipboard path can decode. Pure
/// extension classifier (the decodable set the `image` crate is built with here:
/// PNG/JPEG/WebP); anything else copies as a file URL instead.
#[cfg(target_os = "macos")]
fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "webp"))
}

/// macOS inline clipboard writes. NSPasteboard is not main-thread-bound, so these
/// run synchronously on the caller's (update-loop) thread without a MainThreadMarker.
#[cfg(target_os = "macos")]
mod mac {
    use std::borrow::Cow;
    use std::path::Path;

    /// Copy a still image onto the pasteboard as raw RGBA (`arboard::set_image`).
    /// Decodes with the `image` crate the app already uses — nothing new is added.
    pub fn copy_image(path: &Path) -> Result<(), String> {
        let img = image::open(path)
            .map_err(|e| format!("decode {}: {e}", path.display()))?
            .to_rgba8();
        let (width, height) = (img.width() as usize, img.height() as usize);
        let data = arboard::ImageData {
            width,
            height,
            bytes: Cow::Owned(img.into_raw()),
        };
        arboard::Clipboard::new()
            .map_err(|e| format!("clipboard open: {e}"))?
            .set_image(data)
            .map_err(|e| format!("set_image: {e}"))
    }

    /// Copy `path` as a `file://` URL onto the general pasteboard (NSURL
    /// `fileURLWithPath` + `writeObjects:`). Persists after we exit.
    pub fn copy_file_url(path: &Path) -> Result<(), String> {
        use objc2::runtime::ProtocolObject;
        use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
        use objc2_foundation::{NSArray, NSString, NSURL};

        // Absolute path so Finder/paste targets resolve it regardless of cwd.
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let s = abs.to_str().ok_or_else(|| "non-utf8 path".to_string())?;
        let ns_path = NSString::from_str(s);
        let url = NSURL::fileURLWithPath(&ns_path);
        let writer: &ProtocolObject<dyn NSPasteboardWriting> = ProtocolObject::from_ref(&*url);
        let objects = NSArray::from_slice(&[writer]);
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        if pb.writeObjects(&objects) {
            Ok(())
        } else {
            Err("NSPasteboard writeObjects returned false".to_string())
        }
    }

    /// Copy plain text (`arboard::set_text`).
    pub fn copy_text(text: &str) -> Result<(), String> {
        arboard::Clipboard::new()
            .map_err(|e| format!("clipboard open: {e}"))?
            .set_text(text.to_owned())
            .map_err(|e| format!("set_text: {e}"))
    }
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn image_paths_recognized_case_insensitively() {
        for p in ["/a/b/shot.png", "/a/b/shot.PNG", "/x.jpg", "/x.JPEG", "/y.webp"] {
            assert!(is_image_path(Path::new(p)), "{p} should be an image path");
        }
    }

    #[test]
    fn non_image_paths_rejected() {
        for p in ["/a/rec.mp4", "/a/rec.mov", "/a/notes.txt", "/a/noext", "/a/.png/dir"] {
            assert!(!is_image_path(Path::new(p)), "{p} should not be an image path");
        }
    }
}
