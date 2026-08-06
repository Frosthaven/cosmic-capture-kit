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

/// Upload a capture to a connected cloud account (DRAGON-482). Takes the file, and is
/// always accompanied by [`CLOUD_ACCOUNT`].
///
/// It joins this fixed vocabulary for the reason the ones above are here: the flag string
/// is written by the spawn side and read by `main`'s dispatcher, and a constant is what
/// stops the two from drifting. An upload is a long job, so it runs detached like every
/// other post-capture action rather than holding the editor open.
pub(crate) const CLOUD_UPLOAD: &str = "--cloud-upload";
/// Which connected account an upload goes to: a `cloud::accounts::CloudAccount::id`.
/// Random hex, so it is safe in argv and safe in a log.
pub(crate) const CLOUD_ACCOUNT: &str = "--account";
/// Create a share link for the upload and put it on the clipboard. A bare presence flag,
/// no value.
pub(crate) const CLOUD_AUTO_SHARE: &str = "--auto-share";
/// This upload's SESSION id (DRAGON-490): `cloud::session::new_session_id`'s random hex,
/// minted by the editor before the child is spawned so it can be in the child's own argv.
/// It keys the cross-process progress/cancel sidecars in `cloud::session`, so the editor
/// (which has no other handle to this detached child at all) can watch how it is getting on
/// and ask it to stop. Optional: a terminal run of `--cloud-upload` with no session id simply
/// runs with no cross-process watcher, exactly as before this existed.
pub(crate) const CLOUD_SESSION: &str = "--session";

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
/// `extra` carries the notification helper's kind/reason tokens (DRAGON-450), and the cloud
/// upload's account id (DRAGON-482) and session id (DRAGON-490). All of these are a small set
/// of literal vocabulary words or random hex, never user content, so nothing here has to be
/// escaped or redacted.
///
/// `pub(crate)` rather than `pub(super)` since DRAGON-482: `cloud::upload::spawn_upload_child`
/// is the one caller outside `share`, and it spawns through THIS function on purpose, so the
/// upload child is launched exactly the way every other detached helper is.
///
/// Spawns [`crate::util::self_exe`] rather than `current_exe()` (DRAGON-510). These children
/// are the exact case that distinction exists for: they are detached on purpose and outlive
/// the process that started them, so under an AppImage a `current_exe()` mount path can be
/// gone before the worker has finished with it.
pub(crate) fn spawn_self(flag: &str, path: &Path, extra: &[&str]) -> bool {
    match crate::util::self_exe() {
        Ok(exe) => match Command::new(exe).arg(flag).arg(path).args(extra).spawn() {
            Ok(_) => true,
            Err(e) => {
                log::warn!("spawn_self: could not launch the {flag} worker: {e}");
                false
            }
        },
        Err(e) => {
            log::warn!("spawn_self: self_exe failed, cannot launch the {flag} worker: {e}");
            false
        }
    }
}
