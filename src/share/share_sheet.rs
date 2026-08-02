//! The SHARE seam (DRAGON-467): hand a finished capture to the desktop's own share sheet.
//!
//! This is the same shape as [`super::clipboard`]: a portable capability QUESTION plus a
//! portable ACTION, each with per-OS arms, so the preview editor's toolbar carries no `cfg`
//! at all. The editor asks [`share_available`] whether to light the button up, and calls
//! [`share_file`] when it is pressed.
//!
//! # Where each platform stands
//!
//! A share sheet is a NATIVE, window-anchored UI on the platforms that have one, so each
//! arm below is a capability answer plus, where the answer is yes, one plugin body:
//!
//! * **Windows** is DONE (DRAGON-474): `IDataTransferManagerInterop::ShowShareUIForWindow`
//!   against our own `HWND`, with a `DataRequested` handler that supplies the file as a
//!   `StorageFile`. The body lives in `platform/windows/services.rs` (closed split), and
//!   this file carries only the dispatch. Windows 8.1 and up, so no build gate: Windows 10
//!   gets the same flyout.
//! * **macOS** is DONE (DRAGON-480): `NSSharingServicePicker`, shown relative to a rect in
//!   the preview window's `NSView`. The anchor was the whole difficulty: the picker is a
//!   popover, so it needs the real view and a real rect. The body lives in
//!   `platform/mac/services/share.rs`; [`share_file`] below is the best-effort, id-less
//!   fallback it documents (the real UI path bypasses this seam function entirely, going
//!   through `preview::share::finish_share_sheet`'s `window::run_with_handle` branch instead,
//!   since only that call site still knows exactly which of several simultaneous preview
//!   documents, DRAGON-336, the click belongs to).
//! * **Linux** has no desktop-wide share sheet to call. There is no `org.freedesktop.portal`
//!   interface for "share this file with an app of the user's choosing" (the portals cover
//!   opening, printing, mailing and file transfer, none of which is a share sheet), and
//!   COSMIC ships nothing of its own. So the honest answer here is a permanent `false`
//!   rather than a stand-in that opens something else.
//!
//! Keeping the seam in the shared tree is what made the Windows arm cheap: the button, its
//! rendering, its tooltip and its message already existed and were exercised on every
//! platform, so turning a platform on is this file's arm plus one plugin body, with nothing
//! to rediscover in the UI.

use std::path::Path;

/// Whether this system can hand a file to a native share sheet. Windows only (DRAGON-474).
///
/// The preview editor only BUILDS its Share button when this is true: a share sheet is a
/// capability of the MACHINE, and a permanently dead control on a desktop that will never
/// have one is just noise. See `preview::chrome::share_group`.
#[cfg(target_os = "windows")]
pub fn share_available() -> bool {
    true
}

/// Whether this system can hand a file to a native share sheet. macOS too, since DRAGON-480.
///
/// The preview editor only BUILDS its Share button when this is true: a share sheet is a
/// capability of the MACHINE, and a permanently dead control on a desktop that will never
/// have one is just noise. See `preview::chrome::share_group`.
#[cfg(target_os = "macos")]
pub fn share_available() -> bool {
    true
}

/// Whether this system can hand a file to a native share sheet. `false` on Linux: there is
/// no such thing there. See the module doc.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn share_available() -> bool {
    false
}

/// Windows (DRAGON-474): dispatch to the share-sheet body under `platform/windows/`
/// (closed split). `is_video` only picks the TITLE the sheet shows over the payload; the
/// file itself goes across as a `StorageFile` either way.
///
/// Must be called on the thread that owns our windows (the iced update loop), because the
/// interop anchors the flyout to an `HWND` the CALLING thread owns. `PreviewMsg::Share` is
/// exactly that thread.
#[cfg(target_os = "windows")]
pub fn share_file(path: &Path, is_video: bool) -> Result<(), String> {
    crate::platform::windows::services::share_file(path, is_video)
}

/// macOS (DRAGON-480): the id-less fallback into `platform/mac/services/share.rs`'s
/// best-effort (key-window) body. See that module's doc and this file's for why the real UI
/// path does not call this function.
#[cfg(target_os = "macos")]
pub fn share_file(path: &Path, is_video: bool) -> Result<(), String> {
    crate::platform::mac::share::share_file(path, is_video)
}

/// Hand `path` to the system share sheet. `is_video` distinguishes a recording from a still
/// for platforms whose sheet types its payload.
///
/// `Err` carries a sentence for the user, like every other fallible path in this crate. The
/// caller only reaches this after [`share_available`] said yes, so on Linux this is the
/// belt-and-braces case (a capability that changed under us, or a caller that forgot to
/// ask).
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn share_file(path: &Path, is_video: bool) -> Result<(), String> {
    log::warn!(
        "share: refused, no share sheet on this platform (video={is_video}, {})",
        crate::diag::path_shape(path)
    );
    Err("Sharing isn't available on this system.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The capability and the action must agree: where nothing is wired, asking to share is
    /// refused with a sentence rather than silently doing nothing. A platform that flips
    /// `share_available` has to give `share_file` a body in the same change, and this is what
    /// fails if it does not. On Windows the capability is true (DRAGON-474), so the body
    /// under test is the plugin's and this early-returns; `chrome.rs`'s
    /// `the_share_button_and_the_share_action_read_one_capability` drives that side.
    #[test]
    fn an_unavailable_share_refuses_with_a_reason() {
        if share_available() {
            return;
        }
        let err = share_file(Path::new("/shots/a.png"), false).expect_err("must refuse");
        assert!(err.contains("Sharing"), "the reason names the action: {err:?}");
        // The runtime-string house rule: no em/en-dashes in user-facing copy.
        assert!(!err.contains('\u{2014}') && !err.contains('\u{2013}'), "no dashes in {err:?}");
    }
}
