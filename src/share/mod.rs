//! Post-capture sharing, run in single-threaded re-execs of ourselves (so we
//! never fork from the GUI threads):
//!   * clipboard via the Wayland data-control protocol (persists after we exit,
//!     like `wl-copy`),
//!   * a "Copied"/"Saved" desktop notification whose click reveals the file.
//!
//! The notifier stays alive only long enough to handle a click on its own
//! notification, then exits — it's not a daemon. (A truly zero-process click
//! handler would need an installed `.desktop` + D-Bus activation; we avoid that
//! so there's nothing to install or clean up.)
//!
//! **Both of those workers OUTLIVE the app, so a later capture used to kill them**
//! (DRAGON-519). Every capture commit runs `instance::close_other_instances`, which matches
//! siblings by executable path, and a re-exec of ourselves is exactly that. The clipboard
//! worker is the case that cost real data: on Wayland it does not hand the selection to
//! anything, it SERVES the selection, so ending it empties the user's clipboard. Both now
//! hold `instance::ShareMarker` for as long as they work, which is what the sweep reads to
//! spare them. `instance::SHARE_MARKER` carries the whole account, including why
//! `--open-uri` and `--reveal` are deliberately left out and why Windows has no exposure.

pub(crate) mod reexec;
mod clipboard;
mod notify;
mod open;
mod share_sheet;
mod wifi;

pub use clipboard::{
    AUTO_COPY_MAX_BYTES, CopyRoute, CopyStep, WINDOW_COPY_FOCUS_BUDGET, auto_copy_limit_label,
    copy_embeds_bytes, copy_route, copy_step, copy_text, copy_to_clipboard, copy_text_task,
    needs_window_clipboard, read_text, run_copy, run_copy_text, window_payload,
};
// `UploadOutcome` + `run_upload_notify` are DRAGON-482's additions: the upload child posts
// its own banner in-process (it is already a detached helper), so unlike the capture banner
// there is no `--notify-*` re-exec pair to export alongside them.
pub use notify::{
    CopyOutcome, NotifyKind, UploadOutcome, notify, notify_from_argv, run_notify,
    run_upload_notify,
};
pub use open::{open_uri, run_open_uri, run_reveal, save_and_open};
pub use share_sheet::{share_available, share_file};
pub use wifi::join_wifi;
