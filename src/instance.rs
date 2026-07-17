//! Single-instance locking and sibling-process management.
//!
//! Two independent advisory locks (via `flock`) gate how many overlapping
//! instances of this binary may run:
//!
//! * The **capture lock** (`cosmic-capture-kit.lock`) prevents a second keybind
//!   press from opening a duplicate overlay. It can be released at runtime: when
//!   an instance becomes a settings window it gives up this lock so another
//!   capture instance may start.
//!
//! * The **settings lock** (`cosmic-capture-kit-settings.lock`) ensures only one
//!   settings pane is open across all instances, even when "allow multiple
//!   instances" is on. It is held for the process lifetime on success.

use rustix::fs::{FlockOperation, flock};
use std::fs::File;
use std::sync::Mutex;

/// Holds the capture single-instance lock. Unlike the settings lock it can be
/// released at runtime: when an instance switches into the settings window it is
/// no longer "capturing", so it gives up this lock and another capture instance
/// may launch (even with "allow multiple instances" off).
static CAPTURE_LOCK: Mutex<Option<File>> = Mutex::new(None);

/// Path to the capture single-instance lock file.
fn capture_lock_path() -> String {
    format!("{}/cosmic-capture-kit.lock", crate::util::runtime_dir())
}

/// Hold a process-lifetime advisory lock so a second keybind press doesn't open
/// a duplicate overlay. Returns false if another instance already holds it.
///
/// Byte-identical across platforms: the resident menu-bar DAEMON owns its OWN,
/// separate single-instance lock ([`acquire_daemon_lock`]), so the capture lock is
/// the plain "don't open two overlays" gate on every OS — capture children spawned
/// by the daemon take it exactly like any one-shot capture instance would.
pub(crate) fn acquire_lock() -> bool {
    let file = match File::create(capture_lock_path()) {
        Ok(f) => f,
        Err(_) => return true, // can't create a lock file; fail open rather than block capture
    };
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            // Keep the fd open (held in CAPTURE_LOCK) => hold the lock until it's
            // explicitly released or the process exits.
            if let Ok(mut g) = CAPTURE_LOCK.lock() {
                *g = Some(file);
            }
            true
        }
        Err(_) => false, // another instance already holds it
    }
}

// ── Resident single-instance lock + pid (macOS daemon / Linux resident) ───────
//
// The resident process (macOS menu-bar daemon `crate::daemon`, Linux ksni resident
// `crate::daemon_linux`, DRAGON-173) is single-instance: exactly one runs, and a
// second bare `resident` launch that finds this lock held signals the running
// resident to start a capture (`SIGUSR1`), then exits — the "capture NOW" UX. The
// resident records its pid IN this lock file (safe: it holds the flock, so no reader
// can race a half-written pid — a blocked sibling only reads while it CAN'T take the
// lock). This is SEPARATE from the capture lock so capture children the resident
// spawns can still take the capture lock normally. The lock + pid + SIGUSR1/SIGTERM
// plumbing is byte-identical on both OSes (rustix flock + POSIX signals); only the
// SIGUSR2 hotkey-suspend (below) is macOS-only, since Linux's capture key is a COSMIC
// custom shortcut, not a resident-owned global hotkey.

/// Holds the resident single-instance lock for the resident's process life.
#[cfg(any(target_os = "macos", target_os = "linux"))]
static DAEMON_LOCK: Mutex<Option<File>> = Mutex::new(None);

/// Path to the resident single-instance lock file.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn daemon_lock_path() -> String {
    format!("{}/cosmic-capture-kit-daemon.lock", crate::util::runtime_dir())
}

/// The resident daemon takes this lock at startup and records its pid so a second
/// bare launch can signal it. Returns false if another daemon already holds it
/// (the caller then signals the existing daemon and exits). Fails OPEN (returns
/// true) if the lock file can't be created, so a filesystem hiccup can't wedge the
/// menu bar.
///
/// Bounded retry (DRAGON-130 hotkey restart): a hotkey/settings change restarts the
/// daemon by SIGTERM-ing the old one and spawning a fresh one at once, so the new
/// daemon can briefly find the lock still held by the exiting old daemon. We retry
/// the flock for a short window (~1.5s) so the restart reliably hands off; a COLD
/// start wins on the first attempt (the lock is free), paying no wait. If the window
/// elapses with the lock still held, a DIFFERENT daemon is genuinely up: return false
/// and let the caller signal-and-exit as before.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn acquire_daemon_lock() -> bool {
    acquire_daemon_lock_attempts(31)
}

/// Single-attempt variant (DRAGON-180): a CAPTURE-intent bare launch (the global
/// hotkey with `resident` on) must not sit out the restart-handoff window above —
/// for it, a held lock simply means a live daemon, and the caller should signal it
/// to capture immediately instead of burning ~1.5s to conclude the same thing.
/// Explicit `resident` (daemon-intent) launches keep the full retry window.
#[cfg(target_os = "linux")]
pub(crate) fn try_acquire_daemon_lock() -> bool {
    acquire_daemon_lock_attempts(1)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn acquire_daemon_lock_attempts(attempts: u32) -> bool {
    // Open WITHOUT truncation so a failed flock leaves the holder's recorded pid
    // intact for `signal_existing_capture` to read; the pid is (re)written only
    // AFTER we win the flock below.
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(daemon_lock_path())
    {
        Ok(f) => f,
        Err(_) => return true,
    };
    // ~1.5s total at the default 31: the first attempt is immediate (cold start wins
    // here), then up to 30 more at 50ms while a restarting predecessor releases.
    for attempt in 0..attempts {
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {
                use std::io::{Seek as _, Write as _};
                let _ = file.set_len(0);
                let _ = file.rewind();
                let _ = write!(file, "{}", std::process::id());
                let _ = file.flush();
                if let Ok(mut g) = DAEMON_LOCK.lock() {
                    *g = Some(file);
                }
                return true;
            }
            Err(_) => {
                if attempt + 1 < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    }
    false // another daemon still holds it after the handoff window
}

/// The pid recorded by the running resident daemon (the daemon-lock holder), if any.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn daemon_lock_pid() -> Option<u32> {
    std::fs::read_to_string(daemon_lock_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Resident "capture NOW" UX (DRAGON-130/173): signal the running resident (macOS
/// daemon / Linux resident) to start a fresh capture (`SIGUSR1`, drained by the
/// resident's trigger thread → a default capture child), then the caller exits.
/// Returns true if a signal was delivered. Falls back to false (caller just exits
/// quietly) if there's no recorded/live pid.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn signal_existing_capture() -> bool {
    let Some(pid) = daemon_lock_pid() else {
        return false;
    };
    if pid == std::process::id() {
        return false;
    }
    match rustix::process::Pid::from_raw(pid as i32) {
        Some(p) => rustix::process::kill_process(p, rustix::process::Signal::USR1).is_ok(),
        None => false,
    }
}

/// Resident UX (DRAGON-130/173): ask the running resident (macOS daemon / Linux
/// resident) to EXIT (SIGTERM the daemon-lock holder), used by `SetResident(false)`
/// in the settings UI so the tray/menu-bar item disappears immediately. AppKit
/// handles SIGTERM by terminating the run loop cleanly; the Linux resident installs a
/// SIGTERM handler that shuts the ksni item down and exits. Returns true if a signal
/// was delivered to a live resident.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn signal_daemon_quit() -> bool {
    let Some(pid) = daemon_lock_pid() else {
        return false;
    };
    if pid == std::process::id() {
        return false;
    }
    match rustix::process::Pid::from_raw(pid as i32) {
        Some(p) => rustix::process::kill_process(p, rustix::process::Signal::TERM).is_ok(),
        None => false,
    }
}

/// macOS (DRAGON-130, chord recorder): ask the running resident daemon to SUSPEND its
/// global "Start Capture" hotkey briefly (`SIGUSR2`). The settings window pings this
/// every ~1s while its chord recorder is armed, so the daemon un-registers its
/// PrintScreen (+ F13) Carbon hotkey and the key reaches THIS app to be recorded
/// instead of spawning a capture. The daemon auto-resumes ~3s after the last ping
/// (crash-safe: resume is expiry, never an explicit message), so a settings window
/// that dies mid-record can't leave the hotkey suspended forever. Returns true if a
/// signal was delivered to a live daemon.
#[cfg(target_os = "macos")]
pub(crate) fn signal_daemon_suspend_hotkey() -> bool {
    let Some(pid) = daemon_lock_pid() else {
        return false;
    };
    if pid == std::process::id() {
        return false;
    }
    match rustix::process::Pid::from_raw(pid as i32) {
        Some(p) => rustix::process::kill_process(p, rustix::process::Signal::USR2).is_ok(),
        None => false,
    }
}

/// Release the capture single-instance lock. Called when this instance becomes a
/// settings window, so another capture instance may now launch.
#[cfg_attr(target_os = "macos", allow(dead_code))] // mac hands settings off to a fresh process (DRAGON-153)
pub fn release_capture_lock() {
    if let Ok(mut g) = CAPTURE_LOCK.lock() {
        *g = None; // dropping the File closes the fd and releases the flock
    }
}

/// Acquire the *settings* single-instance lock so only one settings pane can be
/// open across all instances, regardless of "allow multiple instances". Held for
/// the process lifetime on success (closing settings ends the process anyway).
pub fn acquire_settings_lock() -> bool {
    let dir = crate::util::runtime_dir();
    // Open WITHOUT truncating: `File::create` would wipe the HOLDER's recorded pid
    // before flock even ran, so every blocked second attempt erased the very pid
    // that `settings_lock_pid` consumers (spare-the-pane on Linux, focus-the-pane
    // on macOS, DRAGON-153) need. Truncate only once the lock is actually ours.
    let Ok(mut file) = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(format!("{dir}/cosmic-capture-kit-settings.lock"))
    else {
        return true; // can't create a lock file; fail open
    };
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            // Record our pid so `close_other_instances` can spare the live settings
            // window (whichever instance owns the pane) when a capture commits.
            use std::io::Write as _;
            let _ = file.set_len(0);
            let _ = write!(file, "{}", std::process::id());
            std::mem::forget(file); // hold until the process exits
            true
        }
        Err(_) => false, // a settings pane is already open somewhere
    }
}

/// The pid recorded by the settings-lock holder (the open settings window), if any.
pub(crate) fn settings_lock_pid() -> Option<u32> {
    let dir = crate::util::runtime_dir();
    std::fs::read_to_string(format!("{dir}/cosmic-capture-kit-settings.lock"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Terminate every OTHER running CAPTURE instance of this binary (used when a capture
/// is committed, so a multi-instance session collapses to just the one that fired).
/// Matches by executable path via `/proc/<pid>/exe`; signalling a dead pid is a
/// harmless no-op, so no bookkeeping file is needed.
///
/// Settings windows are deliberately spared: a settings pane is its own thing (often
/// a separate `--settings` process), and ending a capture must never close it.
pub fn close_other_instances() {
    let Ok(self_exe) = std::env::current_exe() else {
        return;
    };
    let self_pid = std::process::id();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        if std::fs::read_link(format!("/proc/{pid}/exe")).ok().as_ref() != Some(&self_exe) {
            continue;
        }
        if is_settings_instance(pid) {
            continue; // never close a settings window
        }
        // Never close the resident daemon either (DRAGON-183): it is the SAME
        // executable, so the exe-path match above catches it — and committing a
        // capture must not tear down the tray resident (which may even have
        // spawned this very capture). Two complementary checks: the daemon-lock
        // holder covers a bare-launch daemon (DRAGON-181), the `resident` argv
        // covers a daemon mid-restart before it wins the lock.
        if is_resident_instance(pid) {
            continue;
        }
        if let Some(p) = rustix::process::Pid::from_raw(pid as i32) {
            let _ = rustix::process::kill_process(p, rustix::process::Signal::TERM);
        }
    }
}

/// Whether `pid` is the resident daemon: the daemon-lock holder, or a process
/// launched with the literal `resident` argument (the autostart / toggle-on shape;
/// also covers a restarting daemon that hasn't won the lock yet). Never swept by
/// [`close_other_instances`].
fn is_resident_instance(pid: u32) -> bool {
    if daemon_lock_pid() == Some(pid) {
        return true;
    }
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|c| c.split(|b| *b == 0).any(|arg| arg == b"resident"))
        .unwrap_or(false)
}

/// Whether `pid` is a settings window — either launched with `--settings` (cmdline)
/// or the instance that became the settings pane via the gear button (it owns the
/// settings lock and recorded its pid there). Such instances are never auto-closed.
fn is_settings_instance(pid: u32) -> bool {
    if settings_lock_pid() == Some(pid) {
        return true;
    }
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| b.split(|&c| c == 0).any(|a| a == b"--settings"))
        .unwrap_or(false)
}
