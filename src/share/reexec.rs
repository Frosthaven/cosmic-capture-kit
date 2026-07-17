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
pub(super) fn spawn_self(flag: &str, path: &Path) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe).arg(flag).arg(path).spawn();
    }
}
