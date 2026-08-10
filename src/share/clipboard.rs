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
    // PRE-FLIGHT the protocol, because spawning is not copying (owner report: Copy silently
    // did nothing inside the Flatpak).
    //
    // What this function can honestly return on Linux is "the helper started", and until now
    // that was ALSO what it did return. When the helper cannot possibly succeed, that answer
    // is not merely incomplete, it is false: `true` here makes the editor show "Copied to
    // clipboard" over an empty clipboard, and lets copy-then-delete delete the capture on the
    // strength of a copy that never happened. A wrong success is worse than a reported failure.
    //
    // The clipboard write needs a data-control global, because our model is copy-then-exit with
    // a detached helper owning the selection, and ordinary `wl_data_device` cannot do that (it
    // needs keyboard focus and the selection dies with the client). cosmic-comp hides
    // data-control from sandboxed clients, so inside a Flatpak this is always absent.
    //
    // Reporting `false` routes into machinery that already exists: DRAGON-355 made a failed
    // copy abort the delete and close, and the editor already has a clipboard-error toast. So
    // the user gets told, and nothing is destroyed on a false premise.
    if needs_window_clipboard() {
        // The caller owns this case: it has a WINDOW, and the window is the only thing that can
        // hold a selection here. Returning false without it would be the old lie inverted.
        log::debug!(
            "clipboard: no data-control global; the detached worker cannot serve a selection, \
             so the caller must write through its focused window instead"
        );
        return false;
    }
    spawn_self(if is_video { COPY_FILE } else { COPY_IMAGE }, path, &[])
}

/// HOW a clipboard copy happens in this session — THE one question every copy in the app
/// asks, and the reason it is an enum rather than a bare bool at each call site
/// (`lab/flatpak`). The Copy button, the open-time automatic copy, the OCR / QR / health /
/// share-link text copies and the capture-time delivery all route through
/// [`copy_route`]; a caller that quietly assumed one shape is exactly the drift the owner
/// hit, where the button had the fallback and the auto-copy did not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CopyRoute {
    /// The write STANDS ON ITS OWN once made: macOS/Windows put concrete bytes on the
    /// pasteboard inline, and Linux hands a detached re-exec worker the selection through
    /// data-control. Either way it outlives this process, which is the better behaviour and
    /// the only one that makes copy-then-exit work. Preferred wherever it is possible.
    Standalone,
    /// Only THIS process's focused window can hold the selection, so the write must go out
    /// over its own `wl_data_device` ([`copy_text_task`] for text, [`window_payload`] +
    /// `iced::clipboard::write_data` for a capture) and it needs a window that EXISTS and has
    /// FOCUS. The selection then lives exactly as long as this process.
    ThisWindow,
}

/// Pure, unit-tested: the route a copy takes, given whether the session advertises a
/// data-control global.
///
/// **Protocol-keyed, not sandbox-keyed**, which is what makes this fix worth more than the
/// Flatpak. `wl-clipboard-rs` binds `wlr`/`ext-data-control`, a protocol that exists so
/// clipboard MANAGERS can read the clipboard without focus, and it is privileged for exactly
/// that reason. Three quite different situations lack it and all want the same answer:
///
/// * A Flatpak on COSMIC, where cosmic-comp hides it from sandboxed clients.
/// * GNOME, which has never implemented it for anyone, sandboxed or not.
/// * Sandboxed apps on niri and Hyprland, which is why Bitwarden and RustDesk hit this too.
///
/// So the window route is not Flatpak plumbing that happens to help elsewhere; it is the
/// first time copy works at all on a compositor without data-control.
// Its one production caller is [`copy_route`]'s Linux arm — macOS and Windows have no
// data-control question to ask, their writes being inline and persistent. Compiled into every
// TEST build so the decision is proven on any host (the house pattern).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn copy_route_for(data_control: bool) -> CopyRoute {
    if data_control { CopyRoute::Standalone } else { CopyRoute::ThisWindow }
}

/// Runtime seam: THIS session's copy route. Linux asks the live protocol probe; macOS and
/// Windows are always [`CopyRoute::Standalone`], their writes being inline, synchronous and
/// persistent after exit.
pub fn copy_route() -> CopyRoute {
    #[cfg(target_os = "linux")]
    {
        copy_route_for(crate::platform::backend::wayland_protocols().data_control)
    }
    #[cfg(not(target_os = "linux"))]
    {
        CopyRoute::Standalone
    }
}

/// Whether a clipboard write must go through THIS process's focused window rather than the
/// standalone path — [`copy_route`] as the bool its callers read.
pub fn needs_window_clipboard() -> bool {
    copy_route() == CopyRoute::ThisWindow
}

/// How long a copy that must ride our OWN window waits for that window to take keyboard focus
/// before it gives up and says so (`lab/flatpak`, DRAGON-550; DRAGON-587 gave it a second
/// caller and moved it here).
///
/// The wait exists because these copies are started while the surface that will carry them is
/// still an open task, and a [`CopyRoute::ThisWindow`] write needs a focused surface. In
/// practice the focus lands within a frame or two of the map, so this is a BOUND, not a
/// schedule: it is generous because the only thing at stake is a courtesy copy, and mean
/// because a window that has been sitting unfocused for seconds is one the user has clicked
/// away from, where silently seizing the clipboard later would be worse than reporting the miss.
pub const WINDOW_COPY_FOCUS_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// What a copy that has to reach the clipboard must do RIGHT NOW.
///
/// Pure and unit-tested, because this is THE seam the owner's DRAGON-550 report was about: the
/// preview's Copy BUTTON consulted [`copy_route`] and the automatic copy did not, so on a
/// session without data-control the button worked and the copy that runs on every capture
/// failed with a toast. ONE decision, read by every copy that owns a window.
///
/// DRAGON-587 is the same bug a third time, in the colour picker: the pick's hex write went out
/// in the same batch that destroyed the overlay and queued the result window, so on the window
/// route there was no focused surface to carry the selection and the hex reached nothing. It
/// asks this now, which is why the enum lives here rather than in the preview editor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CopyStep {
    /// Hand it to the standalone path — the historical behaviour, and the one every
    /// data-control Linux session, macOS and Windows takes.
    Detached,
    /// Write it out over this process's own focused window, right now.
    ThroughWindow,
    /// Nothing can be written yet: the selection has to come from our own window and that
    /// window does not hold focus. Wait, bounded by [`WINDOW_COPY_FOCUS_BUDGET`].
    WaitForFocus,
    /// The bounded wait expired with the window still unfocused. Report the miss; never claim
    /// a copy that did not happen.
    ReportFailed,
}

/// Pure, unit-tested: the [`CopyStep`] for the session's copy `route`, whether the window that
/// would carry the write holds keyboard focus, and whether the bounded wait is spent.
///
/// The `route` term is what keeps every non-window session byte-identical: it answers
/// [`CopyRoute::Standalone`] there, and the other two inputs are not even read.
pub fn copy_step(route: CopyRoute, window_focused: bool, budget_spent: bool) -> CopyStep {
    if route == CopyRoute::Standalone {
        return CopyStep::Detached;
    }
    match (window_focused, budget_spent) {
        (true, _) => CopyStep::ThroughWindow,
        (false, false) => CopyStep::WaitForFocus,
        (false, true) => CopyStep::ReportFailed,
    }
}

/// [`copy_text`], but routed through THIS window when the compositor offers no data-control
/// (`lab/flatpak`). Returns the write for the caller to run; empty on the worker path.
///
/// Text needed this every bit as much as images did, and it is the half a user hits FIRST: the
/// Health page's copy buttons, the OCR and QR results, the share links. Those all called
/// `copy_text`, which returns `()` and spawns the same detached worker, so on a compositor
/// without data-control they reported "Copied!" and put nothing anywhere.
///
/// Callers that can return a `Task` should use this. The few that cannot (a detached upload
/// child has no window at all) keep [`copy_text`], which now at least says so in the log.
pub fn copy_text_task<T: Send + 'static>(text: &str) -> cosmic::iced::Task<T> {
    if needs_window_clipboard() {
        // The plain-text sibling of `window_payload`: iced offers the standard text targets
        // over the focused surface, so every app can paste it.
        return cosmic::iced::clipboard::write(text.to_string());
    }
    copy_text(text);
    cosmic::iced::Task::none()
}

/// A clipboard payload the app's own window can serve, built from a finished capture.
///
/// This is the data half of the window fallback; the write itself is `iced::clipboard::
/// write_data`, which goes out over the focused surface's `wl_data_device`.
///
/// **What it costs, stated plainly:** Wayland makes the SOURCE application serve every paste
/// request, so this selection lives exactly as long as the process does. The detached-worker
/// path exists precisely to outlive us and is still preferred wherever data-control allows it.
/// Here the editor staying open IS the clipboard, and closing it takes the copy with it unless
/// a clipboard manager has taken ownership in the meantime.
pub struct WindowPayload {
    mimes: Vec<String>,
    bytes: std::borrow::Cow<'static, [u8]>,
}

/// The payload for `path`, or `None` when there is nothing readable to offer.
///
/// The MIME choice mirrors the detached worker exactly, so a paste target cannot tell the two
/// paths apart: a still is `image/png` bytes, a recording is a `text/uri-list` naming the file
/// (pasting a whole video as bytes is not what a file manager or chat client wants).
pub fn window_payload(path: &Path, is_video: bool) -> Option<WindowPayload> {
    if is_video {
        let uri = format!("{}\r\n", crate::util::path_to_file_uri(path));
        return Some(WindowPayload {
            mimes: vec!["text/uri-list".to_string()],
            bytes: std::borrow::Cow::Owned(uri.into_bytes()),
        });
    }
    let bytes = std::fs::read(path).ok()?;
    Some(WindowPayload {
        // `image/png` first: it is what the bytes ARE. The two `PNG` spellings are what some
        // GTK and Qt targets actually ask for, and offering them costs nothing.
        mimes: vec![
            "image/png".to_string(),
            "image/x-png".to_string(),
            "application/octet-stream".to_string(),
        ],
        bytes: std::borrow::Cow::Owned(bytes),
    })
}

impl cosmic::iced::clipboard::mime::AsMimeTypes for WindowPayload {
    fn available(&self) -> std::borrow::Cow<'static, [String]> {
        std::borrow::Cow::Owned(self.mimes.clone())
    }

    fn as_bytes(&self, mime_type: &str) -> Option<std::borrow::Cow<'static, [u8]>> {
        // Every type we advertise is the SAME bytes; we never offer a conversion we cannot do.
        self.mimes.iter().any(|m| m == mime_type).then(|| self.bytes.clone())
    }
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
    // Say so when the worker cannot possibly serve the selection. This function returns `()`,
    // so a caller has no way to learn it failed and the user just sees "Copied!" over an empty
    // clipboard; the debug log is the only channel there is. `copy_text_task` is the fix for
    // every caller that can take one, and this line is for the few that cannot.
    if needs_window_clipboard() {
        log::error!(
            "clipboard: no data-control global, so this detached text copy cannot serve a \
             selection and will be lost. A caller with a window should use `copy_text_task`."
        );
    }
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
///
/// **The Linux read is data-control too** (DRAGON-572). `wl_clipboard_rs::paste` binds the
/// SAME `wlr`/`ext-data-control` global the write-side worker serves through — that protocol
/// is what lets a fresh, unfocused connection see the selection at all. So the READ shares
/// the write side's [`copy_route`] axis exactly: on a session without the global (a Flatpak
/// on COSMIC, GNOME for everyone, sandboxed niri/Hyprland) this can NEVER succeed. It used
/// to fail silently (`MissingProtocol` swallowed by the `.ok()?`), which is precisely how
/// the editor's paste "did nothing" in the sandbox — for EVERY clipboard, plain text
/// included, not just the emoji the owner happened to test. Now it says so and answers
/// fast; a caller with a focused window must read through it instead
/// (`cosmic::iced::clipboard::read`), the read twin of [`CopyRoute::ThisWindow`].
#[cfg(target_os = "linux")]
pub fn read_text() -> Option<String> {
    use wl_clipboard_rs::paste::{get_contents, ClipboardType, MimeType, Seat};
    if needs_window_clipboard() {
        log::debug!(
            "clipboard: no data-control global, so the in-process text read cannot see the \
             selection; a caller with a focused window must read through it instead"
        );
        return None;
    }
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

    // **Say we are here before we start serving** (DRAGON-519). This process is a detached
    // re-exec of the app's own binary, so a sibling committing a capture matches it by exe
    // path in `instance::close_other_instances` and, until this marker existed, SIGTERMed it.
    // On Wayland that is not a lost helper, it is a lost CLIPBOARD: the block below owns the
    // selection, so killing it takes the user's copied capture away. First statement, so the
    // unspared window is only this process's own startup. See `instance::SHARE_MARKER`.
    let _serving = crate::instance::ShareMarker::new();

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
    // Not `let _ =`. This is a DETACHED re-exec child with no way to toast the editor, so the
    // debug log is the only channel it has, and discarding the error left the one failure a
    // user actually notices ("Copy did nothing") with no trace anywhere. The caller pre-flights
    // the protocol, so reaching a failure HERE means something else went wrong and is worth the
    // line.
    if let Err(e) = opts.copy(
        Source::Bytes(data.into_boxed_slice()),
        MimeType::Specific(mime.to_string()),
    ) {
        crate::diag::note_failure(crate::diag::Failure::SaveFailed, "clipboard copy failed");
        log::error!("clipboard: serving the {mime} selection failed: {e}");
    }
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
    // DRAGON-519, exactly as in `run_copy` above: this worker OWNS the selection while it
    // blocks, so a sibling's capture-commit sweep must spare it or the copied text is gone.
    // The text path is where that is easiest to see, because nothing re-copies it by
    // accident: a scanned QR code's contents, a Wi-Fi password, or an upload's share link
    // (`cloud::child` copies through this same helper) is on the clipboard because the user
    // asked for it and nothing else is going to put it back.
    let _serving = crate::instance::ShareMarker::new();
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

#[cfg(test)]
mod copy_route_tests {
    use super::*;

    /// The whole axis: a data-control global means the copy stands on its own, and its
    /// absence means only our own focused window can hold the selection. Every copy in the
    /// app reads THIS, so a caller cannot quietly assume one shape (`lab/flatpak`: the Copy
    /// button had the window fallback and the open-time copy did not).
    #[test]
    fn data_control_decides_the_route() {
        assert_eq!(copy_route_for(true), CopyRoute::Standalone);
        assert_eq!(copy_route_for(false), CopyRoute::ThisWindow);
    }

    /// `needs_window_clipboard` is the same decision spelled as a bool — the two can never
    /// disagree, which is what lets old call sites keep their wording.
    #[test]
    fn the_bool_spelling_tracks_the_route() {
        for data_control in [false, true] {
            assert_eq!(
                copy_route_for(data_control) == CopyRoute::ThisWindow,
                !data_control,
            );
        }
    }
}

/// The window-route ORDERING decision. Its own module because it pins ONE function and one
/// bug, seen twice: a copy that does not ask the question the Copy button asks
/// (DRAGON-550, the preview's open-time copy; DRAGON-587, the colour picker's pick).
#[cfg(test)]
mod copy_step_tests {
    use super::*;

    /// A session whose copies stand on their own (data-control Linux, macOS, Windows) is
    /// BYTE-IDENTICAL: the worker path, whatever the window is doing. The focus and the
    /// budget are not even consulted.
    #[test]
    fn a_standalone_session_always_takes_the_detached_path() {
        for focused in [false, true] {
            for spent in [false, true] {
                assert_eq!(
                    copy_step(CopyRoute::Standalone, focused, spent),
                    CopyStep::Detached,
                    "focused={focused} budget_spent={spent}"
                );
            }
        }
    }

    /// The window route with focus in hand writes immediately — the pre-opened-spinner shape,
    /// where the capture lands on a surface that is already up.
    #[test]
    fn a_focused_window_writes_straight_away() {
        assert_eq!(copy_step(CopyRoute::ThisWindow, true, false), CopyStep::ThroughWindow);
        // Even a spent budget writes when the window has focus: there is nothing to wait for.
        assert_eq!(copy_step(CopyRoute::ThisWindow, true, true), CopyStep::ThroughWindow);
    }

    /// THE bug, in both its reported shapes: the write is issued while the surface that must
    /// carry it is still an open task, so there is no focused window to serve a selection.
    /// Waiting is the fix; writing regardless is what produced the owner's failure toast on
    /// every capture, and what silently dropped the colour picker's hex.
    #[test]
    fn an_unfocused_window_waits_before_the_budget_is_spent() {
        assert_eq!(copy_step(CopyRoute::ThisWindow, false, false), CopyStep::WaitForFocus);
    }

    /// And the wait is BOUNDED, ending in an honest failure. Never a claimed success: the old
    /// behaviour at least reported the miss, and this must not regress past that.
    #[test]
    fn the_wait_ends_in_a_reported_failure_not_a_claimed_copy() {
        assert_eq!(copy_step(CopyRoute::ThisWindow, false, true), CopyStep::ReportFailed);
        assert_ne!(copy_step(CopyRoute::ThisWindow, false, true), CopyStep::ThroughWindow);
    }

    /// The budget is a bound on a courtesy copy, not a schedule: long enough that a normal
    /// map-then-focus never trips it, short enough that a user who has clicked away is not
    /// ambushed by the clipboard changing under them.
    #[test]
    fn the_focus_budget_is_seconds_not_frames_and_not_minutes() {
        assert!(WINDOW_COPY_FOCUS_BUDGET >= std::time::Duration::from_secs(1));
        assert!(WINDOW_COPY_FOCUS_BUDGET <= std::time::Duration::from_secs(10));
    }
}
