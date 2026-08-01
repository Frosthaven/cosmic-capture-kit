//! Flag constants for the hidden re-exec protocol and the helper that spawns a
//! detached copy of this binary.
//!
//! Both the spawn side (the other `share/` submodules) and `main`'s arg
//! dispatcher import these constants, so the flag strings can only diverge in
//! one place.

use std::path::Path;
use std::process::Command;

pub(crate) const COPY_IMAGE: &str = "--copy-image";
pub(crate) const COPY_FILE: &str = "--copy-file";
pub(crate) const COPY_TEXT: &str = "--copy-text";
pub(crate) const OPEN_URI: &str = "--open-uri";
pub(crate) const REVEAL: &str = "--reveal";
pub(crate) const NOTIFY_COPIED: &str = "--notify-copied";
pub(crate) const NOTIFY_SAVED: &str = "--notify-saved";
/// What the capture was OF, so the banner can name it (DRAGON-450). Optional: a
/// notification with no kind keeps the unnamed wording.
pub(crate) const NOTIFY_KIND: &str = "--notify-kind";
/// Why the capture is not on the clipboard, when it isn't. Optional, and only ever
/// accompanies [`NOTIFY_SAVED`].
pub(crate) const NOTIFY_REASON: &str = "--notify-reason";

/// The full prefix of the URI a Windows toast click re-enters us through (DRAGON-450):
/// `cosmic-capture-kit:reveal?path=<percent-encoded absolute path>`.
///
/// It lives here with the flags because it is the same thing they are — how the app
/// re-enters itself for one small job — even though only Windows registers the scheme
/// (`platform/windows/services.rs` builds and parses it; the installers register it).
/// `diag`'s argv classifier reads it too, so a toast-click launch is tagged `helper`
/// rather than mistaken for a GUI launch.
pub(crate) const REVEAL_URI_PREFIX: &str = "cosmic-capture-kit:reveal?path=";

/// Spawn a detached copy of this binary with `flag`, `path` and any `extra` arguments.
///
/// Returns whether the child was SPAWNED — which is the only thing this side can observe.
/// The worker's own outcome (did the Wayland selection actually get served? did the
/// notification land?) happens after we let go, so `true` means "handed off", never
/// "succeeded". [`super::clipboard::copy_to_clipboard`]'s doc spells out what that means
/// for the copy toast (DRAGON-353); the notify path ignores the value entirely.
///
/// `extra` carries the notification helper's kind/reason tokens (DRAGON-450). It is always
/// a small set of literal vocabulary words, never user content, so nothing here has to be
/// escaped or redacted.
pub(super) fn spawn_self(flag: &str, path: &Path, extra: &[&str]) -> bool {
    match std::env::current_exe() {
        Ok(exe) => match Command::new(exe).arg(flag).arg(path).args(extra).spawn() {
            Ok(_) => true,
            Err(e) => {
                log::warn!("spawn_self: could not launch the {flag} worker: {e}");
                false
            }
        },
        Err(e) => {
            log::warn!("spawn_self: current_exe failed, cannot launch the {flag} worker: {e}");
            false
        }
    }
}
