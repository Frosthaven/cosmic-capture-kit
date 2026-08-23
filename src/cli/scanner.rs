//! `scan --action refresh`: make the scanner overlay that is already open re-read the screen
//! (DRAGON-695).
//!
//! ## Why it is a command and not a key
//!
//! The scanner already has an in-app refresh: pressing its own kind button again re-grabs the
//! screen and re-runs the QR / OCR passes (DRAGON-456, `App::begin_scan_shot`). That works
//! because the pointer is over our overlay. What does NOT work on Linux is a KEY, for exactly
//! the reason the recording controls exist: COSMIC's portal carries no `GlobalShortcuts`
//! interface, so nothing of ours can bind one, and the scanner overlay does not always hold
//! keyboard focus while the user is arranging the thing they want to scan. So the route is the
//! same one every other Linux hotkey in this app takes: bind a key in the desktop's own
//! shortcut settings, and it runs this binary again.
//!
//! ## What it reuses
//!
//! Everything. The transport is [`crate::daemon_ipc`]'s scanner inlet, which is the recording
//! inlet's accept loop, bounds and per-pid addressing with a one-word protocol of its own. The
//! ACTION is `begin_scan_shot`, the same call the button makes, so the CLI cannot refresh in a
//! way the button does not.
//!
//! ## Not a capture session
//!
//! Like [`crate::cli::recording`], this records no [`crate::diag::Failure`]: no capture is at
//! stake, so a failure is a stderr line, a log line and exit 1.

use crate::errln;

/// Refresh the open scanner and exit. Prints the reason and exits 1 when there is none.
#[cfg(target_os = "linux")]
pub fn run_scan_refresh() {
    let cmd = crate::daemon_ipc::ScannerCommand::Refresh;
    match crate::daemon_ipc::send_scanner_command(cmd) {
        Ok(pid) => log::info!("scan --action refresh delivered to the scanner (pid {pid})"),
        Err(why) => {
            // Both channels, like every other detached helper: stderr for a terminal run, the
            // debug log for a desktop hotkey that has no terminal at all.
            errln!("scan --action refresh: {why}");
            log::warn!("scan --action refresh: {why}");
            std::process::exit(1);
        }
    }
}

/// macOS and Windows carry no scanner-control transport, for the same reason they carry no
/// recording-control one: their overlays keep focus and their in-app scan button is reachable,
/// so nothing there was broken and nothing there was changed.
///
/// The command is still PARSED everywhere, which is the point of this arm: a `scan --action
/// refresh` typed on the wrong machine must end HERE rather than opening a scanner the user
/// did not ask for.
#[cfg(not(target_os = "linux"))]
pub fn run_scan_refresh() {
    let why = "scanner commands are Linux-only; on this platform press the scan button in the \
               capture toolbar to read the screen again";
    errln!("scan --action refresh: {why}");
    log::warn!("scan --action refresh: {why}");
    std::process::exit(1);
}
