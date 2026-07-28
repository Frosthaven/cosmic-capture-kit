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

/// Spawn a detached copy of this binary with `flag` and `path` as arguments.
///
/// Returns whether the child was SPAWNED — which is the only thing this side can observe.
/// The worker's own outcome (did the Wayland selection actually get served? did the
/// notification land?) happens after we let go, so `true` means "handed off", never
/// "succeeded". [`super::clipboard::copy_to_clipboard`]'s doc spells out what that means
/// for the copy toast (DRAGON-353); the notify path ignores the value entirely.
pub(super) fn spawn_self(flag: &str, path: &Path) -> bool {
    match std::env::current_exe() {
        Ok(exe) => match Command::new(exe).arg(flag).arg(path).spawn() {
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
