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

pub(crate) mod reexec;
mod clipboard;
mod notify;
mod open;
mod wifi;

pub use clipboard::{
    AUTO_COPY_MAX_BYTES, auto_copy_limit_label, copy_text, copy_to_clipboard, read_text,
    run_copy, run_copy_text,
};
pub use notify::{notify, run_notify};
pub use open::{open_uri, run_open_uri, run_reveal, save_and_open};
pub use wifi::join_wifi;
