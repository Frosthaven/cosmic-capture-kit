//! Clipboard helpers: copy image/file/text data to the clipboard.
//!
//! Linux serves the Wayland selection from a detached re-exec worker (the
//! process must own the selection until something else is copied). macOS/Windows
//! write CONCRETE bytes (macOS: eager `NSData` via `setData:forType:`, or an `NSURL`
//! file reference; Windows: CF_DIB/CF_HDROP) that PERSIST after the process exits —
//! NOT a promised/lazy `NSImage`, whose data would die with our one-shot child before
//! paste (DRAGON-352). So there the copy happens INLINE in the spawn-side functions
//! below (no re-exec worker); the `run_copy*` workers stay no-ops there.

use std::path::Path;

#[cfg(target_os = "linux")]
use super::reexec::{COPY_FILE, COPY_IMAGE, COPY_TEXT, spawn_self};

/// The largest capture an AUTOMATIC clipboard copy will attempt, in megabytes.
///
/// # Why this is a constant and not a setting (DRAGON-353)
///
/// It WAS a setting — "Clipboard size limit", a numeric row that lived under General →
/// "After a Capture" and then, briefly, on the Preview Editor page, persisted as
/// `clipboard_max_mb`. The knob only ever existed to let a user pre-empt a copy that
/// would silently not happen. It no longer needs to: the preview editor TOASTS a named
/// error the moment it declines an automatic copy for size, so the outcome is visible
/// where it occurs. A number the user must guess at, to avoid a failure they can now
/// simply read, earns its removal.
///
/// # What the limit actually covers (corrected, DRAGON-450)
///
/// It bounds copies that put the file's BYTES on the clipboard — a still image, which
/// every platform copies as decoded pixel data (CF_DIBV5 + PNG on Windows, eager PNG/TIFF
/// `NSData` on macOS, an in-memory buffer served by the Linux worker). An unbounded copy
/// of that shape is a real hazard, so it stays bounded.
///
/// A RECORDING is not that shape and never was. Every platform copies a video as a
/// REFERENCE to the file — `CF_HDROP` on Windows, a `file://` `NSURL` on macOS,
/// `text/uri-list` from the Linux worker — a few hundred bytes whatever the recording
/// weighs. This doc used to justify the limit with a multi-gigabyte recording being read
/// whole into memory, which the code has never done; the gate's only real effect was to
/// refuse exactly the copies that would have cost nothing, while the still images it was
/// written for never come close to a gigabyte. So the delivery path now gates on
/// [`copy_embeds_bytes`] instead of on size alone: a 2 GB recording copies.
///
/// The value is exactly what the setting shipped with (its default), so no capture that
/// copied before stops copying now. A MANUAL Copy is unaffected: it always was, and
/// still is, an explicit request that ignores this limit.
pub const AUTO_COPY_MAX_MB: u64 = 1024;

/// [`AUTO_COPY_MAX_MB`] in bytes — what the size checks actually compare against.
pub const AUTO_COPY_MAX_BYTES: u64 = AUTO_COPY_MAX_MB * 1_048_576;

/// [`AUTO_COPY_MAX_MB`] as user-facing text: whole gigabytes when it divides evenly
/// ("1 GB" reads better than "1024 MB"), megabytes otherwise.
///
/// The toast that reports a skipped automatic copy is the ONLY place the limit is
/// visible now that the setting is gone, so it names the number rather than saying a
/// vague "too large" — the user still has to be able to tell whether their next capture
/// will copy.
pub fn auto_copy_limit_label() -> String {
    if AUTO_COPY_MAX_MB.is_multiple_of(1024) {
        format!("{} GB", AUTO_COPY_MAX_MB / 1024)
    } else {
        format!("{AUTO_COPY_MAX_MB} MB")
    }
}

/// Whether copying this capture EMBEDS the file's bytes on the clipboard (a still image)
/// rather than putting a REFERENCE to it there (a recording, or any other non-image path).
///
/// This is the axis [`AUTO_COPY_MAX_BYTES`] applies to, and the only one it ever should
/// have (DRAGON-450): a reference copy costs a few hundred bytes however large the file
/// is, so refusing it for size refuses nothing but the user's recording.
///
/// It must AGREE with the classifier each platform body uses to pick the actual clipboard
/// format (`platform/windows/services.rs::is_image_path` and the macOS mirror in
/// `platform/mac/services/clipboard.rs`) — those stay per-platform on purpose, so a
/// Windows-side test pins the two together rather than a shared call site merging them.
pub fn copy_embeds_bytes(path: &Path, is_video: bool) -> bool {
    !is_video && is_image_extension(path)
}

/// The still-image extensions the byte-embedding copy path can decode — the set the
/// `image` crate is built with here. See [`copy_embeds_bytes`].
fn is_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "webp"))
}

/// Copy the capture to the clipboard (image data for a screenshot, a file
/// reference for a recording). Detached; persists after we exit.
///
/// # What the returned `bool` actually means (DRAGON-353 / DRAGON-355)
///
/// Its honesty differs by platform, deliberately:
///
/// On LINUX the Wayland selection must be OWNED by a live process, so the copy is a
/// detached re-exec worker (`run_copy`, below) that blocks serving the selection until
/// something else is copied. This side sees only whether that child was SPAWNED; whether
/// it then acquired the selection happens in another process, after we have let go, with
/// no channel back. A `false` here is therefore a REAL failure (no worker at all), while a
/// `true` is "handed to a worker that normally succeeds" — NOT a verified round-trip.
///
/// On macOS/Windows the write is INLINE, concrete, and SYNCHRONOUS (see the module doc), so
/// the platform body's real success is returned (DRAGON-355): `true` means the pasteboard
/// write actually succeeded, `false` that it failed. That is a genuinely verified result the
/// preview editor gates copy-then-delete / copy-then-close on — the honesty a one-shot child
/// can offer differs by platform, and only the detached-worker Linux path cannot go further.
#[cfg(target_os = "linux")]
pub fn copy_to_clipboard(path: &Path, is_video: bool) -> bool {
    spawn_self(if is_video { COPY_FILE } else { COPY_IMAGE }, path, &[])
}

/// macOS (DRAGON-230): dispatch to the clipboard body under `platform/mac/` (closed
/// split). A screenshot goes on as eager PNG + TIFF `NSData`, a recording (or any
/// non-image path) as a `file://` URL; like Windows the CONCRETE write persists after
/// we exit, so the copy is INLINE here and `run_copy` stays a no-op. Returns the body's
/// real success (DRAGON-355) — the write is synchronous, so it is verifiable.
#[cfg(target_os = "macos")]
pub fn copy_to_clipboard(path: &Path, is_video: bool) -> bool {
    crate::platform::mac::clipboard::copy_to_clipboard(path, is_video)
}

/// Windows (DRAGON-229): dispatch to the clipboard body under `platform/windows/`
/// (closed split). A still image goes on as image data (PNG + CF_DIBV5), a recording
/// as a CF_HDROP file list; like macOS the write persists after we exit, so the copy
/// is INLINE here and `run_copy` stays a no-op. Returns the body's real success
/// (DRAGON-355) — the write is synchronous, so it is verifiable.
#[cfg(target_os = "windows")]
pub fn copy_to_clipboard(path: &Path, is_video: bool) -> bool {
    crate::platform::windows::services::copy_to_clipboard(path, is_video)
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
        let _ = spawn_self(COPY_TEXT, &path, &[]);
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

/// Read the clipboard's plain TEXT synchronously (DRAGON-354 text-annotation paste).
/// `None` when the clipboard holds no text or the read fails — the caller (the text editor's
/// paste) simply inserts nothing. Small and synchronous by contract.
///
/// Linux reads the Wayland selection directly in-process (a READ needs no detached
/// selection-owner worker, unlike the copy path); macOS/Windows dispatch to their platform
/// `arboard` reader.
#[cfg(target_os = "linux")]
pub fn read_text() -> Option<String> {
    use wl_clipboard_rs::paste::{get_contents, ClipboardType, MimeType, Seat};
    let (mut reader, _mime) =
        get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text).ok()?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut buf).ok()?;
    String::from_utf8(buf).ok()
}

/// macOS: NSPasteboard string via the platform `arboard` reader (closed split).
#[cfg(target_os = "macos")]
pub fn read_text() -> Option<String> {
    crate::platform::mac::clipboard::read_text()
}

/// Windows: clipboard string via the platform `arboard` reader (closed split).
#[cfg(target_os = "windows")]
pub fn read_text() -> Option<String> {
    crate::platform::windows::services::read_text()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The copy FLAVOUR is what the size limit may gate — not the file's size (DRAGON-450).
    #[test]
    fn only_still_images_embed_their_bytes() {
        for p in ["/a/shot.png", r"C:\a\shot.PNG", "/a/x.jpg", "/a/x.JPEG", "/a/y.webp"] {
            assert!(copy_embeds_bytes(Path::new(p), false), "{p} copies as bytes");
        }
        // A recording — and anything else we might hand over — copies as a reference.
        for p in ["/a/rec.mp4", "/a/rec.mov", "/a/notes.txt", "/a/noext"] {
            assert!(!copy_embeds_bytes(Path::new(p), false), "{p} copies as a reference");
        }
    }

    /// `is_video` wins over the extension: a video is a reference copy however it is named.
    #[test]
    fn a_video_never_embeds_its_bytes() {
        assert!(!copy_embeds_bytes(Path::new("/a/frame.png"), true));
        assert!(!copy_embeds_bytes(Path::new("/a/rec.mp4"), true));
    }

    /// THE DRAGON-450 regression pin: a huge RECORDING must not be refused for size. Its
    /// copy is a path, so the byte limit has nothing to bound — the gate is the flavour.
    #[test]
    fn a_multi_gigabyte_recording_is_not_size_gated() {
        let huge = 2 * AUTO_COPY_MAX_BYTES;
        let rec = Path::new("/a/long-session.mp4");
        assert!(!copy_embeds_bytes(rec, true));
        // The delivery path's rule, spelled out: gate only when the copy embeds bytes.
        assert!(!(copy_embeds_bytes(rec, true) && huge > AUTO_COPY_MAX_BYTES));
        // A still image of that size WOULD be gated — the limit still means something.
        let shot = Path::new("/a/enormous.png");
        assert!(copy_embeds_bytes(shot, false) && huge > AUTO_COPY_MAX_BYTES);
    }

    #[test]
    fn the_limit_label_reads_in_whole_gigabytes() {
        assert_eq!(auto_copy_limit_label(), "1 GB");
        assert_eq!(AUTO_COPY_MAX_BYTES, 1024 * 1_048_576);
    }
}
