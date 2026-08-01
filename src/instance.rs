//! Single-instance locking and sibling-process management.
//!
//! ONE advisory lock (via `flock`) gates overlapping instances of this binary:
//!
//! * The **settings lock** (`cosmic-capture-kit-settings.lock`) ensures only one
//!   settings pane is open across all instances. It is held for the process
//!   lifetime on success.
//!
//! DRAGON-351: there used to be a second one, the **capture lock**
//! (`cosmic-capture-kit.lock`), whose ONLY job was to stop a second keybind press from
//! opening a duplicate overlay while one was live — i.e. it implemented "allow multiple
//! capture instances = off". That setting is gone and multiple instances are now
//! unconditional, so nothing was left to gate: the lock had exactly one reader (the
//! launch step-aside in `main`) and its release path (`release_capture_lock`, called
//! when an instance became a settings window or its overlays closed) had already
//! degenerated to a no-op whenever the setting was on. The whole mechanism — the flock
//! file on unix, the `Local\cosmic-capture-kit.capture` named mutex on Windows — was
//! therefore deleted rather than left dangling. The RESIDENT lock below and the settings
//! lock above are untouched; a leftover `cosmic-capture-kit.lock` from an older build is
//! inert (nothing opens it) and lives in the runtime dir, which clears at logout.

// DRAGON-229: rustix (POSIX flock) is unix-only; the Windows lock functions below
// have their own (M0 fail-open) arms and never touch it. Unix selection unchanged.
#[cfg(unix)]
use rustix::fs::{FlockOperation, flock};
// DRAGON-229: the flock `File` (the settings + resident locks) and the `Mutex` static
// that holds the resident one are unix-only; Windows keeps its named-mutex handles
// inside `platform::windows::instance` instead.
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::sync::Mutex;

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

// DRAGON-438: the Windows arm of `acquire_daemon_lock` is GONE. Windows now takes the lock
// through `platform::windows::instance::acquire_daemon_lock_attempts`, driven by
// [`daemon_lock_attempts`] below, because the number of attempts depends on the launch
// INTENT there exactly as it does on Linux. The whole Windows claim/signal/recover sequence
// lives in `platform::windows::daemon::claim_or_signal`; there is no longer a bare
// "take the lock" dispatch to call, which is the point — taking the lock is only half of
// what a Windows bare launch has to decide.

// ── DRAGON-438: the Windows daemon-lock launch protocol (pure decisions) ──────────
//
// The bug this closes: on Windows `resident` defaults ON, so a BARE launch (the user
// pressing their capture key, or double-clicking the app) runs the tray daemon path. If
// the daemon lock was held it pulsed the daemon's capture event and exited(0) SILENTLY.
// But a successful pulse only proves `OpenEventW` + `SetEvent` worked — it proves nothing
// about anyone DRAINING that event. A wedged daemon therefore turned every capture launch
// into a silent no-op, with no window, no error, and nothing in the UI to retry against.
//
// Three shapes produce it:
//
// 1. The daemon's `GetMessageW` loop is blocked because its own capture spawn ran monitor
//    enumeration ON the message thread.
// 2. PERMANENT: the daemon's signal events failed to create at startup, it warned and ran
//    on, so `OpenEventW` fails forever and every later launch no-ops until it is killed.
// 3. Windows took a SINGLE lock attempt where mac/Linux retry for ~1.5s, so a restart
//    handoff could be read as "somebody else owns this".
//
// The fix is to stop trusting the pulse and make the daemon answer — in TWO stages, because
// one signal could not separate the three things that matter:
//
// * The RECEIPT ("your request reached my message loop") is the only thing a wedged daemon
//   cannot produce, so it is the ONLY signal the kill decision keys on.
// * The SPAWN ACK ("the capture child exists") comes later and may legitimately take much
//   longer. A daemon that sends a receipt but no ack is alive and working; it just could not
//   start a process. Killing it would fix nothing.
//
// Only when no receipt arrives, AND the lock holder is provably one of OUR live processes,
// is the lock taken over. These are the pure decisions behind that; the Win32 bodies live in
// `platform::windows::instance` and `platform::windows::daemon`. Compiled (and tested) on
// every platform like `crate::startup_guard`, because a Windows-only decision still has to
// be verifiable on the machine the suite runs on.

/// What signalling the running daemon actually told us. The distinction that matters is
/// "somebody acted on it" versus "the call returned success".
///
/// The daemon answers in TWO stages, and the split is what keeps the takeover honest:
///
/// * The RECEIPT says its message loop pumped our request — the one thing a wedged daemon
///   cannot fake, and therefore the only signal the KILL decision is allowed to key on.
/// * The SPAWN ACK says the capture child actually exists. It comes later and can legitimately
///   take much longer (a worker thread, monitor enumeration, then `CreateProcess` on a ~40MB
///   exe with an antivirus reading every page of it).
///
/// One combined signal could not tell "wedged" from "slow", so a merely-slow healthy daemon
/// would have been terminated. It also could not tell "spawned" from "tried to spawn".
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalProbe {
    /// Both stages: the loop pumped AND the capture child was created. The only answer that
    /// proves the whole path works end to end.
    Acked,
    /// The loop pumped (receipt arrived) but no spawn ack followed. The daemon is ALIVE and
    /// doing its job — it simply could not start a child, which is what an antivirus block or
    /// an exe locked mid-update looks like. Killing it would fix nothing and destroy a healthy
    /// tray, so this is the one unanswered state that must NOT escalate.
    ReceiptOnly,
    /// The capture event exists but the receipt/ack pair does not, which means the running
    /// daemon PREDATES this protocol. Treated as delivered: assuming an old daemon is healthy
    /// is the upgrade-safe direction, and killing one for being old would be a far worse bug
    /// than the one being fixed.
    AckUnavailable,
    /// The capture event was pulsed and the RECEIPT never came. Nothing drained it, so the
    /// message loop is not running — the wedge this ticket exists for.
    NotAcked,
    /// The capture event could not be opened even after the startup grace. On a machine where
    /// the lock IS held that is a daemon which never managed to create its events, and can
    /// therefore never be reached again by anything.
    NoCaptureEvent,
}

/// How many times to try the daemon lock, by launch INTENT (DRAGON-180's rule, ported to
/// Windows by DRAGON-438). `31` is ~1.5s at the 50ms step: the first attempt is immediate
/// so a cold start pays nothing, and the rest bridge a restart handoff.
///
/// A capture-intent launch gets ONE attempt on purpose. A held lock there just means a
/// live daemon, and the user is waiting for an overlay — so the answer is to signal that
/// daemon at once, and the ACK (not a longer wait) is what tells us whether it worked.
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
pub fn daemon_lock_attempts(daemon_intent: bool) -> u32 {
    if daemon_intent {
        31
    } else {
        1
    }
}

/// What a launch that found the daemon lock held should do next.
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaunchAction {
    /// A live daemon has the request. Exit — but SAY so; the silent exit is the bug.
    ExitDelivered,
    /// Nobody acted, and the recorded lock holder is a live process running OUR
    /// executable. Terminate it and try the lock again: we are the user's only remaining
    /// route to a capture, and the holder has proven it cannot serve one.
    KillHolderAndRetry,
    /// Nobody acted, but there is nothing safe to kill — the recorded pid is dead, or it
    /// belongs to some OTHER program (a recycled pid). Just retry the lock; a dead holder
    /// releases it to us anyway.
    RetryLock,
    /// The daemon is demonstrably ALIVE (its loop pumped) but could not spawn the child.
    /// Leave it completely alone — do not kill it, and do not try to take its lock — and let
    /// [`after_failed_takeover`] decide what THIS process does instead. Replacing a working
    /// tray would not make `CreateProcess` succeed.
    FallBackWithoutKilling,
}

/// The recovery decision for a launch whose signal went unanswered.
///
/// The safety rule is in the third argument and it is absolute: we only ever terminate a
/// pid that is BOTH alive and running our own executable image. A recorded pid can be
/// stale and Windows recycles pids, so "the pid file said so" is never enough on its own.
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
pub fn relaunch_action(
    probe: SignalProbe,
    holder_live: bool,
    holder_is_ours: bool,
) -> RelaunchAction {
    match probe {
        // Somebody is serving the request (or is old enough that we must assume so).
        SignalProbe::Acked | SignalProbe::AckUnavailable => RelaunchAction::ExitDelivered,
        // Alive but unable to spawn. The holder state is deliberately IGNORED here: even a
        // live holder of ours is not a candidate, because it is not the thing that is broken.
        SignalProbe::ReceiptOnly => RelaunchAction::FallBackWithoutKilling,
        // Nothing drained the request: the loop is not running.
        SignalProbe::NotAcked | SignalProbe::NoCaptureEvent => {
            if holder_live && holder_is_ours {
                RelaunchAction::KillHolderAndRetry
            } else {
                RelaunchAction::RetryLock
            }
        }
    }
}

/// What to do when even the takeover failed to win the lock.
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFailedTakeover {
    /// A `resident` launch asked for a DAEMON, and some other daemon holds the lock. There
    /// is nothing for this process to do, so exit — loudly, at `warn`, never the silent
    /// `exit(0)` this ticket exists to remove.
    ExitLoud,
    /// A bare launch asked for a CAPTURE. We could not become the daemon, but we can still
    /// give the user what they pressed the key for: fall through to the one-shot capture
    /// overlay, which is exactly the historical non-resident behaviour.
    FallThroughToCapture,
}

/// Whether a failed takeover should end the process or fall through to a capture.
///
/// The asymmetry is the whole point: a daemon-intent launch has no work left, while a
/// capture-intent launch still owes the user an overlay. Falling through means the worst
/// case of a truly stuck daemon is "the tray is broken", not "the app does nothing".
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
pub fn after_failed_takeover(daemon_intent: bool) -> AfterFailedTakeover {
    if daemon_intent {
        AfterFailedTakeover::ExitLoud
    } else {
        AfterFailedTakeover::FallThroughToCapture
    }
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

// DRAGON-438: the Windows arm of `signal_existing_capture` is GONE, and deliberately not
// replaced by an `allow(dead_code)` shim. It pulsed the daemon's named capture event and
// reported the pulse as success — but `OpenEventW` + `SetEvent` succeed against an event
// nobody is draining, so it reported success for a wedged daemon just as readily as for a
// healthy one, and the launch then exited(0) having delivered nothing. Its replacement is
// `platform::windows::instance::signal_capture_and_wait_ack`, which requires the daemon to
// say it SPAWNED the child. Leaving the old function callable would leave the silent no-op
// one call site away from coming back. mac and Linux keep theirs above: their SIGUSR1 goes
// to a pid whose liveness they check, so it does not carry this failure mode.

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

/// Windows (DRAGON-237): ask the running daemon to EXIT (tear its tray icon down + quit),
/// used by `SetResident(false)` / a capture-hotkey change. Body under
/// `platform::windows::instance` (strict split — pulses the daemon's named quit event).
/// Returns true if a running daemon was signalled.
#[cfg(windows)]
pub(crate) fn signal_daemon_quit() -> bool {
    crate::platform::windows::instance::signal_daemon_quit()
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

/// Windows (DRAGON-237, chord recorder): ask the running daemon to SUSPEND its global
/// "Start Capture" hotkey briefly so a PrintScreen press reaches the settings recorder
/// instead of spawning a capture. Body under `platform::windows::instance` (strict split —
/// pulses the daemon's named suspend event); the daemon auto-resumes after the pings stop.
/// Returns true if a running daemon was signalled.
#[cfg(windows)]
pub(crate) fn signal_daemon_suspend_hotkey() -> bool {
    crate::platform::windows::instance::signal_daemon_suspend_hotkey()
}

/// Acquire the *settings* single-instance lock so only one settings pane can be
/// open across all instances. Held for the process lifetime on success (closing
/// settings ends the process anyway).
#[cfg(unix)]
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

/// Whether a settings pane is CURRENTLY open somewhere, probed WITHOUT retaining the lock
/// (DRAGON-353 — the preview editor's settings button needs the answer, not the lock).
///
/// A `flock` is released when its fd closes, so a non-blocking exclusive attempt on a
/// freshly-opened descriptor that is then DROPPED tells us whether someone else holds it
/// and leaves the world exactly as it found it. Crucially it opens WITHOUT truncating —
/// truncating would erase the holder's recorded pid, which `settings_lock_pid` consumers
/// need (the same trap [`acquire_settings_lock`] documents above).
///
/// Inherently a TOCTOU answer: a pane can open or close between this call and whatever the
/// caller does next. That is acceptable at the only call site — the worst case is a
/// `--settings` child that finds the lock taken and (per platform) either pokes the holder
/// or returns quietly, i.e. exactly what the other branch would have done.
#[cfg(unix)]
pub fn settings_pane_is_open() -> bool {
    let dir = crate::util::runtime_dir();
    let Ok(file) = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(format!("{dir}/cosmic-capture-kit-settings.lock"))
    else {
        // Can't even create the lock file; `acquire_settings_lock` fails OPEN in the same
        // situation, so match it and assume nothing is holding the pane.
        return false;
    };
    // Taking the lock here would mean HOLDING it (a `flock` is not released by an explicit
    // unlock we then race on), so probe and immediately drop the fd — the kernel releases
    // whatever we took as the descriptor closes.
    let held = flock(&file, FlockOperation::NonBlockingLockExclusive).is_err();
    drop(file);
    held
}

/// Windows: always `false` — deliberately, not as a stub.
///
/// The Windows settings guard is a NAMED MUTEX with no non-retaining probe, and it does not
/// need one: a blocked `--settings` launch there does not vanish (as it historically did on
/// Linux) — it writes the focus POKE the live holder polls (`main.rs`'s settings_only
/// branch → `compositor::activate_title`). So the caller's open-vs-refocus decision is moot
/// on Windows: spawning `--settings` unconditionally produces the right behaviour either
/// way, which is what answering `false` here arranges.
#[cfg(windows)]
pub fn settings_pane_is_open() -> bool {
    false
}

/// Windows (DRAGON-229): the named-mutex settings guard. Body under
/// `platform::windows::instance` (strict split); on success it also records the holder
/// pid so the sibling sweep can spare THIS settings window. Returns false if a settings
/// pane is already open somewhere; fails OPEN if the mutex can't be created.
#[cfg(windows)]
pub fn acquire_settings_lock() -> bool {
    crate::platform::windows::instance::acquire_settings_lock()
}

/// The pid recorded by the settings-lock holder (the open settings window), if any.
/// DRAGON-229: consumed only by `is_settings_instance` (Linux `/proc` sweep) and the
/// macOS focus-the-pane path, neither of which exists on Windows, so it is
/// `not(windows)`-gated to stay dead-code-free there.
#[cfg(not(windows))]
pub(crate) fn settings_lock_pid() -> Option<u32> {
    let dir = crate::util::runtime_dir();
    std::fs::read_to_string(format!("{dir}/cosmic-capture-kit-settings.lock"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

// ── Runtime state markers (DRAGON-322) ───────────────────────────────────────
//
// A RECORDING session and a fresh capture — or a SELF-CAPTURE of CCK's own preview-editor
// window — coexist as independent processes (DRAGON-351: unconditionally; this used to be
// the "allow multiple instances" setting). Two things then need cross-process knowledge
// the settings lock doesn't carry:
//
//   * the sibling sweep [`close_other_instances`] must SPARE a sibling that is
//     recording or showing a preview — those are real workspaces the user wants kept,
//     not the stale selector overlays the sweep exists to collapse; and
//   * a freshly-launched capture overlay must know a recording is ALREADY live so it
//     can DISABLE the video capture kind (only one recording at a time; a still image
//     capture may still run alongside it).
//
// Each protected instance advertises its state with a per-pid sidecar file under the
// runtime dir (the audio-meter per-pid convention, `audio/meters.rs`): created on
// entering the state, removed on leaving it / at exit. Per-pid so several
// concurrent instances never collide. A crash can leave a STALE marker, so the
// one consumer that reads markers for pids it hasn't already proven live
// ([`any_other_recording`]) re-checks liveness and sweeps dead-pid files; the sweep
// only ever queries markers for a pid it already found LIVE, so it needs no probe.
// Plain files (portable `std::fs`), so the whole mechanism is byte-identical on Linux,
// macOS and Windows — only the liveness probe below is per-platform.

/// Marker suffix: this pid has a recording in progress (incl. paused).
const RECORDING_MARKER: &str = "recording";
/// Marker suffix: this pid has a preview editor open.
const PREVIEW_MARKER: &str = "preview";
/// Marker suffix: this pid is showing the capture-failure ALERT (DRAGON-415). The
/// pile-up case this exists for is a user pressing the hotkey several times because
/// nothing happened: each press is its own child, each fails the same way, and without a
/// cross-process marker each would put up its own dialog. The first one to fail speaks;
/// the rest find this marker and exit quietly (they were about to exit anyway).
const ALERT_MARKER: &str = "alert";
/// Marker suffix: this pid is LISTENING for preview handoffs (DRAGON-336) — the
/// unix-socket sibling of its [`PREVIEW_MARKER`]. Sits in the same per-pid namespace so
/// the stale sweep below clears a SIGKILLed host's socket file for free; the transport
/// itself lives in [`crate::preview_ipc`].
const PREVIEW_SOCKET_MARKER: &str = "preview.sock";
/// Marker suffix: the recording-session LEDGER (DRAGON-421) — this pid's live recording
/// names its ffmpeg muxer, its audio FIFOs and its temp file here, so a LATER session can
/// tell that wreckage apart from a live session's working files. Written and cleared by
/// [`crate::record::recover`], which is also the only thing that removes stale ones —
/// deliberately NOT in [`sweep_stale_markers`]'s list, because deleting the ledger is the
/// LAST step of acting on it, never the first.
pub(crate) const SESSION_MARKER: &str = "session";

/// Per-pid state-marker sidecar path (`{runtime}/cosmic-capture-kit.<pid>.<suffix>`).
fn state_marker_path(pid: u32, suffix: &str) -> String {
    state_marker_path_in(&crate::util::runtime_dir(), pid, suffix)
}

/// [`state_marker_path`] against an EXPLICIT directory, so the scan helpers can be
/// exercised over a temp dir instead of the live runtime dir.
fn state_marker_path_in(dir: &str, pid: u32, suffix: &str) -> String {
    format!("{dir}/cosmic-capture-kit.{pid}.{suffix}")
}

/// The recording-session ledger path for `pid` (DRAGON-421) — a sidecar in the SAME
/// per-pid namespace as every other marker, so its owner is read off the filename and
/// its liveness answered by [`pid_is_live`], exactly like the rest.
pub(crate) fn session_ledger_path(pid: u32) -> String {
    state_marker_path(pid, SESSION_MARKER)
}

/// Split a state-marker FILENAME back into its `(pid, suffix)`, or `None` when the name
/// isn't one of ours. The lock/socket sidecars use a HYPHEN stem
/// (`cosmic-capture-kit-daemon.lock`), so the dotted prefix here never matches them.
pub(crate) fn parse_marker_name(name: &str) -> Option<(u32, &str)> {
    let rest = name.strip_prefix("cosmic-capture-kit.")?;
    let (pid_str, suffix) = rest.split_once('.')?;
    Some((pid_str.parse::<u32>().ok()?, suffix))
}

/// Whether a per-pid sidecar with this `suffix` should be removed once its owner is dead.
///
/// The rule is uniform — a per-pid file in this namespace describes a process, so it means
/// nothing once that process is gone — but the SET is worth stating in one place, because
/// exactly one member of the namespace is deliberately excluded (see below).
///
/// DRAGON-421 added the recorder's audio FIFOs (`<pid>.<token>.{mic,sys}mix.pcm`, minted in
/// `record::owned::try_start_owned_audio`). They are per-pid sidecars like everything else
/// here, and a crashed recording left them behind to accumulate until the runtime dir
/// cleared at logout. They are swept by the SAME rule rather than by a second sweeper of
/// their own — which is also why a plain still capture now clears them, not just a
/// recording.
///
/// The session LEDGER ([`SESSION_MARKER`]) is deliberately NOT in the set. It is the
/// EVIDENCE a dead session leaves about its own wreckage — which ffmpeg it spawned, which
/// temp that ffmpeg is holding — and `record::recover` removes it as the LAST step of
/// acting on it. Sweeping it here would throw away the record before anything read it.
fn sweep_when_owner_is_dead(suffix: &str) -> bool {
    // DRAGON-336: the preview-handoff SOCKET rides the same per-pid namespace, so a
    // host killed with SIGKILL (which leaves its socket file behind) is cleaned up
    // here too — otherwise a recycled pid would inherit a socket nothing listens on.
    matches!(suffix, RECORDING_MARKER | PREVIEW_MARKER | PREVIEW_SOCKET_MARKER | ALERT_MARKER)
        || is_audio_fifo_suffix(suffix)
}

/// Whether a marker suffix names one of the recorder's audio FIFOs — the `<token>` in
/// `cosmic-capture-kit.<pid>.<token>.micmix.pcm` is per-pre-flight, so the suffix is
/// matched by its tail rather than compared whole.
pub(crate) fn is_audio_fifo_suffix(suffix: &str) -> bool {
    suffix.ends_with(".micmix.pcm") || suffix.ends_with(".sysmix.pcm")
}

/// Remove every per-pid sidecar whose owning pid is dead — for EVERY kind
/// [`sweep_when_owner_is_dead`] covers.
///
/// DRAGON-336: `any_other_recording` already swept the RECORDING markers it scans, but
/// nothing ever swept the PREVIEW ones — they are only probed by pid, for a sibling
/// `close_other_instances` already found live, so a dead pid's marker is never visited.
/// A preview that died without reaching `finish_session` (SIGKILL, OOM, a panic) therefore
/// leaked its marker until the runtime dir cleared at logout, and a RECYCLED pid would then
/// be wrongly spared from the collapse sweep. Cheap (one readdir) and best-effort
/// throughout: a marker we fail to remove is simply retried next launch.
pub(crate) fn sweep_stale_markers() {
    sweep_stale_markers_in(&crate::util::runtime_dir(), std::process::id(), &pid_is_live);
}

/// [`sweep_stale_markers`] against an EXPLICIT directory, self pid and liveness oracle (the
/// `*_in` idiom the other scan helpers here use), returning what it removed so a caller can
/// report it. DRAGON-421's recovery sweep calls THIS rather than growing a second readdir
/// with the same rule in it.
pub(crate) fn sweep_stale_markers_in(
    dir: &str,
    self_pid: u32,
    live: &dyn Fn(u32) -> bool,
) -> Vec<std::path::PathBuf> {
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return removed;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((pid, suffix)) = parse_marker_name(name) else {
            continue;
        };
        if !sweep_when_owner_is_dead(suffix) {
            continue;
        }
        if pid == self_pid || live(pid) {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed.push(entry.path());
        }
    }
    removed
}

/// Create (`active`) or remove this instance's RECORDING marker. Set when a recording
/// actually starts, cleared when it ends (even though this process lives on into the
/// video preview) so other overlays re-enable the video kind promptly.
pub(crate) fn set_recording_marker(active: bool) {
    set_self_marker(RECORDING_MARKER, active);
}

/// Create (`active`) or remove this instance's PREVIEW-open marker. Set when a preview
/// editor opens, cleared at `finish_session` (a preview close ends the process).
pub(crate) fn set_preview_marker(active: bool) {
    set_self_marker(PREVIEW_MARKER, active);
}

/// Create (`active`) or remove this instance's failure-ALERT marker (DRAGON-415). Held
/// only while the modal is actually on screen, so nothing is suppressed a moment longer
/// than the dialog exists.
///
/// Called only from the macOS and Windows alert presenters (DRAGON-436 added the second) —
/// the mechanism itself is portable `std::fs`, like every other marker, so it stays here
/// with its siblings rather than growing a second, platform-shaped copy inside each plugin.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub(crate) fn set_alert_marker(active: bool) {
    set_self_marker(ALERT_MARKER, active);
}

/// Whether ANOTHER live instance is showing a failure alert right now. Sweeps markers left
/// by dead pids as it scans, so a child killed mid-dialog can never mute the next one.
///
/// BEST-EFFORT, and the callers treat it that way. Both presenters ask this and then call
/// [`set_alert_marker`], which is check-then-set: two children failing in the same instant
/// can both find no marker and both put a dialog up. The cost of that race is one extra
/// dialog in the rare case; a real mutual exclusion (a named mutex, an atomic create) would
/// buy little on a path that is already the unhappy one. Do not document it as a guarantee.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub(crate) fn any_other_alert() -> bool {
    any_other_marker_in(&crate::util::runtime_dir(), std::process::id(), ALERT_MARKER)
}

fn set_self_marker(suffix: &str, active: bool) {
    let path = state_marker_path(std::process::id(), suffix);
    if active {
        let _ = std::fs::File::create(&path);
    } else {
        let _ = std::fs::remove_file(&path);
    }
}

/// Whether the capture overlay should SPARE a sibling instance from the collapse sweep,
/// as a pure function of that sibling's observed state. A recording or preview sibling is
/// a live workspace to keep; a BARE SELECTOR sibling still gets collapsed, which is the
/// unrelated "only one selection overlay on screen" rule.
///
/// DRAGON-351: this used to be gated on the "allow multiple capture instances" setting
/// too (`allow_multiple && (recording || preview)`); that setting is gone and the sparing
/// is now unconditional. The predicate itself deliberately STAYS — macOS and Windows spawn
/// a process per preview and depend on it (Windows has no preview-handoff transport at
/// all, so its sweep is the only thing standing between a recording session and a fresh
/// capture).
///
/// DRAGON-438 added `alert`. A sibling showing the DRAGON-415 capture-failure dialog is
/// mid-conversation with the user: the sweep would `TerminateProcess` it, so the dialog
/// explaining why a capture failed would vanish the instant the user retried — and retrying
/// after a failure is the single most likely thing they do. Worse, that child holds the
/// cross-process alert marker (the one that makes the FIRST failure speak and the rest stay
/// quiet), so killing it mid-dialog can leave a pile of failures with nobody left to explain
/// any of them. It is a window the user must act on, like a recording or a preview, so it is
/// spared like one.
///
/// Both sweeps read this flag, and each platform pairs a WRITER with a READER:
///
/// * macOS: `platform/mac/services/alert.rs` writes the marker; the `/proc`-style sweep body
///   below reads it.
/// * Windows: `platform/windows/alert.rs` writes it as of DRAGON-436 (the Windows failure
///   alert, in review on its own branch); the Toolhelp sweep in
///   `platform::windows::instance::close_other_instances` reads it through [`marker_flags`].
///   The reader landed here first ON PURPOSE — it composes with that branch, so the sparing
///   is live the moment the two merge rather than needing a follow-up.
/// * Linux writes no alert marker (its failures reach a terminal), so the flag is always
///   false there and the sweep is byte-identical.
pub fn should_spare_sibling(recording: bool, preview: bool, alert: bool) -> bool {
    recording || preview || alert
}

/// Whether the VIDEO capture kind should be offered. Disabled while another instance is
/// already recording (only one recording at a time; still image capture stays allowed).
pub fn video_capture_allowed(external_recording: bool) -> bool {
    !external_recording
}

// DRAGON-351: `wants_own_instance(allow_multiple)` lived here — the named seam for "should
// this launch run as its OWN process, or step aside for the running one?". Every launch now
// wants its own instance unconditionally, so the seam has no decision left to document and
// the `main.rs` step-aside it fed (plus the capture lock behind it, see the module doc) is
// gone with it. Its sibling `handoff_allowed(allow_multiple)` — the DRAGON-336 preview
// handoff gate, marked TEMPORARY from the day it was written — went the same way: a
// finished capture now always tries the running preview host (`try_handoff_capture`).

/// Whether ANOTHER live instance currently has a recording in progress (its recording
/// marker exists and its pid is still alive). Consumed by the capture overlay to DISABLE
/// the video kind while a recording runs elsewhere. Sweeps markers left behind by dead
/// pids as it scans, so a crashed recorder can't wedge the toggle off forever.
pub(crate) fn any_other_recording() -> bool {
    any_other_marker_in(&crate::util::runtime_dir(), std::process::id(), RECORDING_MARKER)
}

/// Whether any pid OTHER than `self_pid` holds a live `marker` in `dir`. The body
/// `any_other_recording` has always had, generalised over the marker kind (DRAGON-415
/// needs the same question for the failure alert) and over the directory, so it can be
/// exercised against a temp dir like the other `*_in` scan helpers. Stale markers from
/// dead pids are swept as we scan, so a crashed instance can never hold a state open
/// forever.
fn any_other_marker_in(dir: &str, self_pid: u32, marker: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let suffix = format!(".{marker}");
    let mut found = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix("cosmic-capture-kit.") else {
            continue;
        };
        let Some(pid_str) = rest.strip_suffix(&suffix) else {
            continue;
        };
        let Some(pid) = pid_str.parse::<u32>().ok() else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        if pid_is_live(pid) {
            found = true;
        } else {
            // Stale marker from a crashed instance — sweep it so the state re-opens.
            let _ = std::fs::remove_file(entry.path());
        }
    }
    found
}

// ── Preview HOST discovery (DRAGON-336) ──────────────────────────────────────
//
// The multi-document preview keeps ONE process hosting several preview windows, so a
// fresh capture child needs to FIND that host to hand its file over
// (`crate::preview_ipc`). There is deliberately no second registry: a preview host is
// exactly a pid whose [`PREVIEW_MARKER`] exists and is LIVE — the same marker
// `close_other_instances` already spares. The transport socket is that pid's
// [`PREVIEW_SOCKET_MARKER`] sibling ([`preview_socket_path`]); a marker with no socket is
// simply a preview that isn't hosting, and the child's connect fails into its own
// preview. Unix-only: Windows has no unix-socket transport (see `preview_ipc`).

/// The preview-handoff socket path for `pid` — the sibling of that pid's preview marker.
#[cfg(unix)]
pub(crate) fn preview_socket_path(pid: u32) -> String {
    state_marker_path(pid, PREVIEW_SOCKET_MARKER)
}

/// Every OTHER live preview host, in the order a child should try them: most recently
/// opened first (its marker's mtime), ties broken by pid so the order is deterministic.
/// Markers left by dead pids are swept as we scan, exactly like [`any_other_recording`].
#[cfg(unix)]
pub(crate) fn live_preview_hosts() -> Vec<u32> {
    live_preview_hosts_in(&crate::util::runtime_dir(), std::process::id())
}

/// The pid of a live preview host, if any (excluding self) — the first candidate of
/// [`live_preview_hosts`]. The "is anyone hosting?" question in one call.
#[cfg(unix)]
pub(crate) fn live_preview_host() -> Option<u32> {
    live_preview_hosts().into_iter().next()
}

/// [`live_preview_hosts`] against an EXPLICIT runtime dir + self pid, so the scan (and
/// its stale sweep) is testable without touching the live runtime dir.
#[cfg(unix)]
fn live_preview_hosts_in(dir: &str, self_pid: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(u32, Option<std::time::SystemTime>)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((pid, suffix)) = parse_marker_name(name) else {
            continue;
        };
        if suffix != PREVIEW_MARKER || pid == self_pid {
            continue;
        }
        if !pid_is_live(pid) {
            // A preview that died without reaching `finish_session`: drop its marker AND
            // its handoff socket so no child ever tries to hand a capture to a corpse.
            let _ = std::fs::remove_file(entry.path());
            let _ = std::fs::remove_file(state_marker_path_in(dir, pid, PREVIEW_SOCKET_MARKER));
            continue;
        }
        found.push((pid, entry.metadata().ok().and_then(|m| m.modified().ok())));
    }
    order_preview_hosts(found)
}

/// Order host candidates for a handoff attempt: newest marker first (the most recently
/// opened preview host is the one the user is most likely looking at), unknown mtimes
/// last, ties broken by ascending pid so the order never depends on readdir.
#[cfg(unix)]
fn order_preview_hosts(mut found: Vec<(u32, Option<std::time::SystemTime>)>) -> Vec<u32> {
    found.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    found.into_iter().map(|(pid, _)| pid).collect()
}

/// Whether `pid` is a live process. Linux reads `/proc/<pid>`; other unix uses a
/// signal-0 probe (`ESRCH` = gone, `EPERM` = alive but not ours); Windows delegates to
/// the platform body. Best-effort — a false "live" only keeps the video toggle disabled
/// a little longer, never a hard failure.
#[cfg(target_os = "linux")]
pub(crate) fn pid_is_live(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn pid_is_live(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 delivers nothing; it only probes existence/permission.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
#[cfg(windows)]
pub(crate) fn pid_is_live(pid: u32) -> bool {
    crate::platform::windows::instance::pid_is_live(pid)
}

/// Terminate every OTHER running CAPTURE instance of this binary (used when a capture
/// is committed, so a multi-instance session collapses to just the one that fired).
/// Matches by executable path via `/proc/<pid>/exe`; signalling a dead pid is a
/// harmless no-op, so no bookkeeping file is needed.
///
/// Settings windows are deliberately spared: a settings pane is its own thing (often
/// a separate `--settings` process), and ending a capture must never close it.
///
/// DRAGON-322 (unconditional since DRAGON-351): a RECORDING or PREVIEW sibling is also
/// spared (its per-pid state marker) so a recording session + a capture — including a
/// self-capture of the open preview — coexist. A bare selector sibling still collapses.
///
/// LINUX ONLY (DRAGON-459). This body enumerates `/proc`, which does not exist on
/// macOS: it used to be `#[cfg(not(windows))]` and so compiled there, but every run
/// hit the `read_dir("/proc")` failure below and returned immediately — the sweep was
/// inert on mac, closing nothing. See the mac twin further down for the real fix.
#[cfg(target_os = "linux")]
pub fn close_other_instances() {
    // DRAGON-336: drop dead pids' markers BEFORE reading them below, so a recycled pid
    // can never inherit a crashed preview's marker and be wrongly spared.
    sweep_stale_markers();
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
        // DRAGON-322: keep a live recording / preview sibling.
        let recording =
            std::path::Path::new(&state_marker_path(pid, RECORDING_MARKER)).exists();
        let preview = std::path::Path::new(&state_marker_path(pid, PREVIEW_MARKER)).exists();
        // DRAGON-438: and a sibling mid-failure-dialog.
        let alert = std::path::Path::new(&state_marker_path(pid, ALERT_MARKER)).exists();
        if should_spare_sibling(recording, preview, alert) {
            log::info!(
                "DRAGON-322: sparing sibling pid {pid} (recording={recording} preview={preview} alert={alert})"
            );
            continue;
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

/// macOS (DRAGON-459): every live pid on the system, via `libproc`'s `proc_listallpids`
/// — the mac analog of Linux's `/proc` readdir. Part of `libSystem` (linked into every
/// process already), so no new crate or framework. `None` becomes an empty sweep rather
/// than a panic: a transient failure here should cost one skipped sweep, never a crash.
///
/// Sized generously above the reported byte count: the live process count can grow
/// between the size query and the fetch (a process starting is normal churn, unlike
/// `mac_argv`'s single already-exec'd target), and a short buffer here would silently
/// truncate the list rather than error, which would degrade back toward DRAGON-459's
/// original bug (siblings quietly never found) instead of away from it.
#[cfg(target_os = "macos")]
fn mac_all_pids() -> Vec<u32> {
    // SAFETY: a null buffer with the documented sentinel; the raw byte count needed is
    // handed back rather than written through, per `proc_listallpids`'s contract.
    let needed = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return Vec::new();
    }
    let capacity = (needed as usize / std::mem::size_of::<libc::pid_t>()) + 64;
    let mut buf: Vec<libc::pid_t> = vec![0; capacity];
    // SAFETY: `buf` is sized for `capacity` pid_t entries; the byte length passed
    // matches that allocation exactly.
    let bytes = unsafe {
        libc::proc_listallpids(
            buf.as_mut_ptr() as *mut libc::c_void,
            (capacity * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
        )
    };
    if bytes <= 0 {
        return Vec::new();
    }
    let count = (bytes as usize / std::mem::size_of::<libc::pid_t>()).min(buf.len());
    buf.truncate(count);
    buf.into_iter().map(|p| p as u32).collect()
}

/// macOS (DRAGON-459): `pid`'s executable path via `libproc`'s `proc_pidpath` — the mac
/// analog of Linux's `/proc/<pid>/exe` readlink. `None` for a dead/inaccessible pid,
/// exactly like the Linux body's `read_link` failure.
#[cfg(target_os = "macos")]
fn mac_exe_path(pid: u32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is exactly `PROC_PIDPATHINFO_MAXSIZE` bytes, the size this call
    // documents as sufficient for any path it can return.
    let len = unsafe {
        libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32)
    };
    if len <= 0 {
        return None;
    }
    buf.truncate(len as usize);
    Some(std::path::PathBuf::from(std::ffi::OsString::from_vec(buf)))
}

/// macOS (DRAGON-459): the pure parsing half of [`mac_argv`] — pulled apart from the
/// `sysctl` call so the buffer layout (undocumented by Apple, but stable and the same
/// one `ps`/Activity Monitor read) is unit-testable without a live process.
///
/// Layout: a leading `argc` (native-endian `i32`), then the saved executable path
/// (NUL-terminated), then NUL padding out to the first non-NUL byte, then exactly
/// `argc` NUL-terminated strings (`argv[0..argc)`), then the environment (ignored —
/// we stop once `argc` strings are read).
#[cfg(target_os = "macos")]
fn parse_procargs2(buf: &[u8]) -> Vec<String> {
    if buf.len() < 4 {
        return Vec::new();
    }
    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]).max(0) as usize;
    if argc == 0 {
        return Vec::new();
    }
    let mut i = 4;
    // Skip the saved executable path.
    while i < buf.len() && buf[i] != 0 {
        i += 1;
    }
    // Skip the NUL padding between the exec path and argv[0].
    while i < buf.len() && buf[i] == 0 {
        i += 1;
    }
    let mut argv = Vec::with_capacity(argc);
    for _ in 0..argc {
        if i >= buf.len() {
            break;
        }
        let start = i;
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        argv.push(String::from_utf8_lossy(&buf[start..i]).into_owned());
        i += 1; // the NUL terminator
    }
    argv
}

/// macOS (DRAGON-459): `pid`'s argv via `sysctl(KERN_PROCARGS2)` — the mac analog of
/// Linux's `/proc/<pid>/cmdline`. Empty on any failure (permission, dead pid, or a
/// buffer `sysctl` rejects), matching the Linux body's `unwrap_or(false)` fallback shape
/// one level up in [`is_resident_instance`] / [`is_settings_instance`].
#[cfg(target_os = "macos")]
fn mac_argv(pid: u32) -> Vec<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    // SAFETY: `oldp` null + a valid `oldlenp` is the documented size-query form.
    let queried = unsafe {
        libc::sysctl(mib.as_mut_ptr(), mib.len() as libc::c_uint, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0)
    };
    if queried != 0 || size == 0 {
        return Vec::new();
    }
    // A target's argv is fixed at exec time and cannot grow between the two calls, but
    // sysctl still wants oldlenp to be strictly enough — pad a little rather than retry.
    size += 16;
    let mut buf = vec![0u8; size];
    let mut got = size;
    // SAFETY: `buf` is exactly `got` bytes, matching `oldlenp`.
    let fetched = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut got,
            std::ptr::null_mut(),
            0,
        )
    };
    if fetched != 0 || got < 4 {
        return Vec::new();
    }
    buf.truncate(got);
    parse_procargs2(&buf)
}

/// macOS (DRAGON-459): the same sibling sweep as Linux, byte-for-byte the same DECISION
/// (identity by executable path, then the settings / recording / preview / alert /
/// resident sparing rules, unwidened), sourced from `libproc` instead of `/proc` (which
/// does not exist on macOS — see the Linux twin's doc comment for the bug this replaces).
#[cfg(target_os = "macos")]
pub fn close_other_instances() {
    sweep_stale_markers();
    let self_pid = std::process::id();
    let Some(self_exe) = mac_exe_path(self_pid) else {
        return;
    };
    for pid in mac_all_pids() {
        if pid == self_pid {
            continue;
        }
        if mac_exe_path(pid).as_ref() != Some(&self_exe) {
            continue;
        }
        if is_settings_instance(pid) {
            continue; // never close a settings window
        }
        // DRAGON-322: keep a live recording / preview sibling.
        let recording =
            std::path::Path::new(&state_marker_path(pid, RECORDING_MARKER)).exists();
        let preview = std::path::Path::new(&state_marker_path(pid, PREVIEW_MARKER)).exists();
        // DRAGON-438: and a sibling mid-failure-dialog.
        let alert = std::path::Path::new(&state_marker_path(pid, ALERT_MARKER)).exists();
        if should_spare_sibling(recording, preview, alert) {
            log::info!(
                "DRAGON-322: sparing sibling pid {pid} (recording={recording} preview={preview} alert={alert})"
            );
            continue;
        }
        if is_resident_instance(pid) {
            continue;
        }
        if let Some(p) = rustix::process::Pid::from_raw(pid as i32) {
            let _ = rustix::process::kill_process(p, rustix::process::Signal::TERM);
        }
    }
}

/// Windows (DRAGON-229): the Toolhelp sibling-capture sweep. Body under
/// `platform::windows::instance` (strict split) — matches siblings by full exe path,
/// spares the settings window (its recorded pid), and force-terminates the rest (the
/// analog of the Linux uncaught SIGTERM).
///
/// DRAGON-322: the Windows body SPARES a recording / preview sibling (its per-pid state
/// marker) exactly like the unix body, keeping a recording session + a concurrent capture
/// alive.
#[cfg(windows)]
pub fn close_other_instances() {
    // DRAGON-336: same pre-read stale sweep as the unix body (the helper is portable
    // std::fs), so `marker_flags` below can't see a dead pid's leftover marker.
    sweep_stale_markers();
    crate::platform::windows::instance::close_other_instances();
}

/// DRAGON-322: whether `pid`'s per-pid RECORDING / PREVIEW / ALERT marker exists. The
/// predicate the Windows sibling sweep uses (its pids come from a live Toolhelp snapshot,
/// so no liveness probe is needed here). Windows-only — the unix sweep inlines the same
/// three `Path::exists` checks against [`state_marker_path`].
///
/// The ALERT flag is DRAGON-438: see [`should_spare_sibling`] for why a child showing the
/// failure dialog must survive the next capture's sweep.
#[cfg(windows)]
pub(crate) fn marker_flags(pid: u32) -> (bool, bool, bool) {
    (
        std::path::Path::new(&state_marker_path(pid, RECORDING_MARKER)).exists(),
        std::path::Path::new(&state_marker_path(pid, PREVIEW_MARKER)).exists(),
        std::path::Path::new(&state_marker_path(pid, ALERT_MARKER)).exists(),
    )
}

/// Whether `pid` is the resident daemon: the daemon-lock holder, or a process
/// launched with the literal `resident` argument (the autostart / toggle-on shape;
/// also covers a restarting daemon that hasn't won the lock yet). Never swept by
/// [`close_other_instances`]. LINUX ONLY (DRAGON-459) — see the mac twin below.
#[cfg(target_os = "linux")]
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
/// LINUX ONLY (DRAGON-459) — see the mac twin below.
#[cfg(target_os = "linux")]
fn is_settings_instance(pid: u32) -> bool {
    if settings_lock_pid() == Some(pid) {
        return true;
    }
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| b.split(|&c| c == 0).any(|a| a == b"--settings"))
        .unwrap_or(false)
}

/// macOS (DRAGON-459) twin of [`is_resident_instance`]: same two checks, argv sourced
/// from [`mac_argv`] instead of `/proc/<pid>/cmdline`.
#[cfg(target_os = "macos")]
fn is_resident_instance(pid: u32) -> bool {
    if daemon_lock_pid() == Some(pid) {
        return true;
    }
    mac_argv(pid).iter().any(|arg| arg == "resident")
}

/// macOS (DRAGON-459) twin of [`is_settings_instance`]: same two checks, argv sourced
/// from [`mac_argv`] instead of `/proc/<pid>/cmdline`.
#[cfg(target_os = "macos")]
fn is_settings_instance(pid: u32) -> bool {
    if settings_lock_pid() == Some(pid) {
        return true;
    }
    mac_argv(pid).iter().any(|arg| arg == "--settings")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DRAGON-351: sparing is now a pure function of the SIBLING's state — the "allow
    /// multiple capture instances" setting that used to gate it is gone. A bare selector
    /// sibling still collapses ("one selection overlay on screen"), which is why the
    /// predicate survives the setting's removal instead of becoming `true`.
    #[test]
    fn spare_recording_and_preview_siblings_but_never_a_bare_selector() {
        assert!(!should_spare_sibling(false, false, false)); // bare selector overlay -> collapse
        assert!(should_spare_sibling(true, false, false)); // recording session
        assert!(should_spare_sibling(false, true, false)); // open preview (self-capture)
        assert!(should_spare_sibling(true, true, false)); // recording WITH a preview open
    }

    /// DRAGON-438: a sibling showing the capture-failure dialog is spared.
    ///
    /// The scenario, which is the whole reason the flag exists: a capture fails and puts up
    /// the dialog; the user's next move is to press the capture key again; that new child
    /// commits and runs the sibling sweep, which would `TerminateProcess` the dialog the
    /// user was still reading. The alert child is a window the user must ACT ON, so it
    /// belongs with the recording and preview cases, not with the bare selector overlay that
    /// is meant to collapse.
    #[test]
    fn spare_a_sibling_that_is_showing_the_failure_dialog() {
        // A child doing nothing but showing the dialog — no recording, no preview — is
        // exactly the shape the sweep used to kill, and is spared on the alert flag alone.
        assert!(should_spare_sibling(false, false, true));
        // Sparing is additive: adding the dialog to an already-spared sibling cannot make it
        // collapsible, and the bare selector (nothing at all) is still the ONLY case that
        // collapses. That last assertion is the one worth keeping — the rule is "spare every
        // window the user must act on", not "spare everything".
        for recording in [false, true] {
            for preview in [false, true] {
                assert!(
                    should_spare_sibling(recording, preview, true),
                    "an alert must survive whatever else the sibling is doing \
                     (recording={recording} preview={preview})"
                );
            }
        }
        assert!(
            !should_spare_sibling(false, false, false),
            "a sibling showing nothing the user must act on still collapses"
        );
    }

    // ── DRAGON-438: the Windows daemon-lock launch protocol ──────────────────
    // Windows-only decisions, run on every host: this is a dual-boot shared tree and the
    // suite is the only gate, so the table has to be verifiable wherever it runs.

    /// The intent split (DRAGON-180's Linux rule, now Windows'): a daemon-intent launch
    /// keeps the ~1.5s restart-handoff window, a capture-intent launch takes ONE attempt
    /// and relies on the ACK rather than on waiting longer.
    #[test]
    fn lock_attempts_follow_launch_intent() {
        assert_eq!(daemon_lock_attempts(true), 31);
        assert_eq!(daemon_lock_attempts(false), 1);
        // The retry window must be a real one, or the handoff it exists for cannot happen.
        assert!(daemon_lock_attempts(true) > daemon_lock_attempts(false));
    }

    /// An ACK, or an old daemon that cannot give one, both mean the request is somebody
    /// else's now. The `AckUnavailable` arm is the upgrade-safety rule: a daemon from
    /// before this protocol has no ack event, and must never be killed for being old.
    #[test]
    fn an_acked_or_pre_ack_daemon_is_treated_as_having_delivered() {
        for probe in [SignalProbe::Acked, SignalProbe::AckUnavailable] {
            for holder_live in [false, true] {
                for holder_is_ours in [false, true] {
                    assert_eq!(
                        relaunch_action(probe, holder_live, holder_is_ours),
                        RelaunchAction::ExitDelivered,
                        "{probe:?} live={holder_live} ours={holder_is_ours}"
                    );
                }
            }
        }
    }

    /// THE bug: a pulse that nobody drains, and a capture event that cannot even be
    /// opened (the daemon whose events failed to create at startup, which is permanent).
    /// Both mean the message loop is not running, so both take over — but only from a
    /// holder we can prove is ours.
    #[test]
    fn an_unanswered_signal_takes_over_only_a_live_holder_that_is_ours() {
        for probe in [SignalProbe::NotAcked, SignalProbe::NoCaptureEvent] {
            assert_eq!(
                relaunch_action(probe, true, true),
                RelaunchAction::KillHolderAndRetry,
                "{probe:?}"
            );
        }
    }

    /// A daemon that PUMPED but could not spawn is never killed, whatever the holder state
    /// says. The receipt already proved its message loop is running, so it is not the wedge
    /// this ticket hunts — it is a machine that cannot start a process right now (antivirus
    /// blocking the exe, or the exe locked mid-update). Terminating a healthy tray would not
    /// make `CreateProcess` succeed, and would cost the user their tray on top of their
    /// capture.
    ///
    /// This is the row that keeps the single-signal version's worst failure from coming
    /// back: with one combined ack, a merely SLOW spawn (a ~40MB exe under an AV scan) read
    /// identically to a wedge, and got the daemon terminated for it.
    #[test]
    fn a_daemon_that_pumped_but_could_not_spawn_is_never_killed() {
        for holder_live in [false, true] {
            for holder_is_ours in [false, true] {
                assert_eq!(
                    relaunch_action(SignalProbe::ReceiptOnly, holder_live, holder_is_ours),
                    RelaunchAction::FallBackWithoutKilling,
                    "live={holder_live} ours={holder_is_ours}"
                );
            }
        }
    }

    /// The safety rule, stated as its own test because getting it wrong means killing a
    /// stranger's process. A recorded pid can be stale, and Windows RECYCLES pids, so a
    /// pid that is alive but running someone else's image must never be terminated —
    /// only retried against.
    #[test]
    fn a_foreign_or_dead_holder_is_never_killed() {
        for probe in [SignalProbe::NotAcked, SignalProbe::NoCaptureEvent] {
            // Alive, but NOT our executable: a recycled pid. Retry, never terminate.
            assert_eq!(relaunch_action(probe, true, false), RelaunchAction::RetryLock);
            // Dead: nothing to kill, and its lock is already ours for the taking.
            assert_eq!(relaunch_action(probe, false, true), RelaunchAction::RetryLock);
            assert_eq!(relaunch_action(probe, false, false), RelaunchAction::RetryLock);
        }
    }

    /// The whole matrix in one sweep, so a new `SignalProbe` variant cannot be added
    /// without a decision being made for it here.
    #[test]
    fn the_relaunch_table_is_total() {
        for probe in [
            SignalProbe::Acked,
            SignalProbe::ReceiptOnly,
            SignalProbe::AckUnavailable,
            SignalProbe::NotAcked,
            SignalProbe::NoCaptureEvent,
        ] {
            for holder_live in [false, true] {
                for holder_is_ours in [false, true] {
                    let expect = match probe {
                        SignalProbe::Acked | SignalProbe::AckUnavailable => {
                            RelaunchAction::ExitDelivered
                        }
                        SignalProbe::ReceiptOnly => RelaunchAction::FallBackWithoutKilling,
                        _ if holder_live && holder_is_ours => RelaunchAction::KillHolderAndRetry,
                        _ => RelaunchAction::RetryLock,
                    };
                    assert_eq!(
                        relaunch_action(probe, holder_live, holder_is_ours),
                        expect,
                        "{probe:?} live={holder_live} ours={holder_is_ours}"
                    );
                }
            }
        }
    }

    /// After a takeover that still could not win the lock: a `resident` launch has no work
    /// left and exits (loudly — the silent exit IS the bug), while a bare capture launch
    /// still owes the user the overlay they pressed a key for. That fall-through is what
    /// downgrades the worst case from "the app does nothing" to "the tray is broken".
    #[test]
    fn a_failed_takeover_still_gives_a_capture_launch_its_capture() {
        assert_eq!(after_failed_takeover(false), AfterFailedTakeover::FallThroughToCapture);
        assert_eq!(after_failed_takeover(true), AfterFailedTakeover::ExitLoud);
    }

    #[test]
    fn video_kind_gated_on_external_recording() {
        assert!(video_capture_allowed(false)); // nobody recording -> video offered
        assert!(!video_capture_allowed(true)); // another instance recording -> disabled
    }

    #[test]
    fn state_marker_path_is_per_pid_and_named() {
        let rec = state_marker_path(4242, RECORDING_MARKER);
        let prev = state_marker_path(4242, PREVIEW_MARKER);
        assert!(rec.ends_with("cosmic-capture-kit.4242.recording"), "{rec}");
        assert!(prev.ends_with("cosmic-capture-kit.4242.preview"), "{prev}");
        assert_ne!(rec, prev);
    }

    /// DRAGON-336: the sweep's filename parser is the inverse of `state_marker_path`,
    /// and must not claim the hyphen-stemmed lock/socket sidecars living in the same dir.
    #[test]
    fn marker_names_round_trip_and_ignore_lock_sidecars() {
        for suffix in [RECORDING_MARKER, PREVIEW_MARKER] {
            let path = state_marker_path(4242, suffix);
            let name = path.rsplit('/').next().unwrap();
            assert_eq!(
                parse_marker_name(name),
                Some((4242, suffix)),
                "should round-trip {name}"
            );
        }
        // Not markers: the lock/socket sidecars (hyphen stem, no pid) and junk.
        for name in [
            "cosmic-capture-kit-daemon.lock",
            "cosmic-capture-kit-settings.lock",
            "cosmic-capture-kit-recording.sock",
            "cosmic-capture-kit.notapid.preview",
            "something-else.4242.preview",
        ] {
            assert_eq!(parse_marker_name(name), None, "should ignore {name}");
        }
    }

    /// DRAGON-421: the sweep's SET is a contract in both directions. The recorder's audio
    /// FIFOs joined it (a crashed recording used to leave them until logout, and they are
    /// per-pid sidecars like everything else) — but the session LEDGER must stay OUT of it,
    /// because it is the evidence `record::recover` reads before removing it itself.
    #[test]
    fn the_dead_owner_sweep_covers_the_audio_fifos_but_never_the_session_ledger() {
        for suffix in [
            RECORDING_MARKER,
            PREVIEW_MARKER,
            PREVIEW_SOCKET_MARKER,
            ALERT_MARKER,
            "0.micmix.pcm",
            "13.sysmix.pcm",
        ] {
            assert!(sweep_when_owner_is_dead(suffix), "should sweep {suffix}");
        }
        assert!(
            !sweep_when_owner_is_dead(SESSION_MARKER),
            "the ledger is evidence, not litter: sweeping it here would throw away the \
             record of a crashed session's wreckage before anything acted on it"
        );
        // The bench FIFOs (`--bench-record`) are a developer tool with their own cleanup
        // and no pid-liveness story; they are deliberately not claimed.
        assert!(!sweep_when_owner_is_dead("bench-mic.pcm"));
    }

    /// The sweep removes a dead owner's sidecars and NOTHING else — the live pid's files
    /// and our own are untouched. This is the same guarantee `record::recover` leans on to
    /// keep a concurrent recording's FIFOs (DRAGON-322/351).
    #[cfg(unix)]
    #[test]
    fn the_dead_owner_sweep_spares_live_and_self_owned_sidecars() {
        let dir = std::env::temp_dir().join(format!("cck-sweep-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.to_string_lossy().into_owned();

        let dead = 999_999_101;
        let live = 999_999_102;
        let selfish = 999_999_103;
        for pid in [dead, live, selfish] {
            std::fs::File::create(state_marker_path_in(&dir, pid, "0.micmix.pcm")).unwrap();
            std::fs::File::create(state_marker_path_in(&dir, pid, RECORDING_MARKER)).unwrap();
        }
        // A ledger for the dead pid: it must SURVIVE this sweep (recover reads it).
        std::fs::File::create(state_marker_path_in(&dir, dead, SESSION_MARKER)).unwrap();

        let removed = sweep_stale_markers_in(&dir, selfish, &|p| p == live);

        assert_eq!(removed.len(), 2, "only the dead pid's two sidecars: {removed:?}");
        assert!(!std::path::Path::new(&state_marker_path_in(&dir, dead, "0.micmix.pcm")).exists());
        assert!(std::path::Path::new(&state_marker_path_in(&dir, dead, SESSION_MARKER)).exists(),
            "the ledger survives to be acted on");
        for pid in [live, selfish] {
            assert!(
                std::path::Path::new(&state_marker_path_in(&dir, pid, "0.micmix.pcm")).exists(),
                "a live (or our own) recording keeps its FIFOs — deleting them would break it"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DRAGON-415: the generalised marker scan (`any_other_recording`'s body, now shared
    /// with the failure alert) answers only for OTHER pids that are still alive, is keyed
    /// to the exact marker kind, and sweeps dead pids' markers as it goes — so a child
    /// killed mid-dialog can never mute the next one.
    #[cfg(unix)]
    #[test]
    fn marker_scan_is_per_kind_live_and_non_self() {
        let dir = std::env::temp_dir().join(format!("cck-marker-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.to_string_lossy().into_owned();

        let live = std::process::id(); // certainly alive
        let dead = {
            let mut child = std::process::Command::new("true").spawn().unwrap();
            child.wait().unwrap();
            child.id()
        };
        let selfish = 999_999_002; // stands in as "us" for this scan

        // Nothing at all: the first failure in a quiet system must speak.
        assert!(!any_other_marker_in(&dir, selfish, ALERT_MARKER));

        // Our OWN marker is never an excuse to stay silent.
        std::fs::File::create(state_marker_path_in(&dir, selfish, ALERT_MARKER)).unwrap();
        assert!(!any_other_marker_in(&dir, selfish, ALERT_MARKER));

        // A dead pid's marker is swept, not obeyed.
        std::fs::File::create(state_marker_path_in(&dir, dead, ALERT_MARKER)).unwrap();
        assert!(!any_other_marker_in(&dir, selfish, ALERT_MARKER));
        assert!(
            !std::path::Path::new(&state_marker_path_in(&dir, dead, ALERT_MARKER)).exists(),
            "a dead pid's alert marker must be swept"
        );

        // A different marker kind is a different question.
        std::fs::File::create(state_marker_path_in(&dir, live, RECORDING_MARKER)).unwrap();
        assert!(!any_other_marker_in(&dir, selfish, ALERT_MARKER));
        assert!(any_other_marker_in(&dir, selfish, RECORDING_MARKER));

        // A live sibling actually showing one: stay quiet.
        std::fs::File::create(state_marker_path_in(&dir, live, ALERT_MARKER)).unwrap();
        assert!(any_other_marker_in(&dir, selfish, ALERT_MARKER));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DRAGON-336: the preview-handoff SOCKET is a per-pid sidecar in the same namespace,
    /// distinct from the marker it sits beside (so a socket file can never be mistaken for
    /// "a preview is open") and parsed by the sweep so a SIGKILLed host's socket is cleared.
    #[cfg(unix)]
    #[test]
    fn preview_socket_is_a_distinct_per_pid_sidecar() {
        let sock = preview_socket_path(4242);
        assert!(sock.ends_with("cosmic-capture-kit.4242.preview.sock"), "{sock}");
        assert_ne!(sock, state_marker_path(4242, PREVIEW_MARKER));
        let name = sock.rsplit('/').next().unwrap();
        assert_eq!(parse_marker_name(name), Some((4242, PREVIEW_SOCKET_MARKER)));
    }

    /// Host discovery is marker-driven: live preview pids only, never self, dead pids'
    /// markers (and their sockets) swept as we scan.
    #[cfg(unix)]
    #[test]
    fn preview_host_discovery_skips_self_and_dead_pids() {
        let dir = std::env::temp_dir().join(format!("cck-host-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.to_string_lossy().into_owned();

        let live = std::process::id(); // a pid that is certainly alive
        // A spawned-and-reaped child is a pid that is certainly dead. (Pid 0 is NOT a
        // usable stand-in: kill(0, 0) probes the caller's own process group and reports
        // it alive on macOS.)
        let dead = {
            let mut child = std::process::Command::new("true").spawn().unwrap();
            child.wait().unwrap();
            child.id()
        };
        let selfish = 999_999_001; // stands in as "us" for this scan

        for pid in [live, dead, selfish] {
            std::fs::File::create(state_marker_path_in(&dir, pid, PREVIEW_MARKER)).unwrap();
        }
        std::fs::File::create(state_marker_path_in(&dir, dead, PREVIEW_SOCKET_MARKER)).unwrap();
        // A RECORDING marker is a different state and must never look like a host.
        std::fs::File::create(state_marker_path_in(&dir, live, RECORDING_MARKER)).unwrap();

        let hosts = live_preview_hosts_in(&dir, selfish);
        assert_eq!(hosts, vec![live], "only the live, non-self preview pid hosts");
        assert!(
            !std::path::Path::new(&state_marker_path_in(&dir, dead, PREVIEW_MARKER)).exists(),
            "a dead pid's marker must be swept"
        );
        assert!(
            !std::path::Path::new(&state_marker_path_in(&dir, dead, PREVIEW_SOCKET_MARKER)).exists(),
            "a dead pid's handoff socket must be swept with it"
        );
        // An empty dir has no hosts (the first-capture case).
        let empty = std::env::temp_dir().join(format!("cck-host-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(live_preview_hosts_in(&empty.to_string_lossy(), selfish).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    /// Candidate order is deterministic: newest marker first, unknown mtimes last, ties
    /// by ascending pid — so a child always tries the most recently opened host first.
    #[cfg(unix)]
    #[test]
    fn preview_hosts_are_ordered_newest_first() {
        use std::time::{Duration, SystemTime};
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let t1 = t0 + Duration::from_secs(5);
        assert_eq!(
            order_preview_hosts(vec![(7, Some(t0)), (9, Some(t1)), (3, None)]),
            vec![9, 7, 3]
        );
        // Same mtime -> ascending pid, never readdir order.
        assert_eq!(
            order_preview_hosts(vec![(9, Some(t0)), (3, Some(t0)), (7, Some(t0))]),
            vec![3, 7, 9]
        );
        assert!(order_preview_hosts(Vec::new()).is_empty());
    }

    /// DRAGON-459: `parse_procargs2`'s buffer layout, pinned against a hand-built
    /// `KERN_PROCARGS2` buffer (real values in a synthetic byte layout — no live process
    /// needed, matching the project's "unit-test the pure logic island" convention).
    #[cfg(target_os = "macos")]
    #[test]
    fn parse_procargs2_reads_argv_after_the_exec_path_and_its_padding() {
        // argc=3, exec path "/bin/foo", three NULs of padding, then three argv strings.
        let mut buf = Vec::new();
        buf.extend_from_slice(&3i32.to_ne_bytes());
        buf.extend_from_slice(b"/bin/foo\0");
        buf.extend_from_slice(&[0u8; 3]); // padding before argv[0]
        buf.extend_from_slice(b"foo\0--region\0--no-editor\0");
        // Trailing bytes (environment) must never be read past argc strings.
        buf.extend_from_slice(b"SOME_ENV=1\0");
        assert_eq!(parse_procargs2(&buf), vec!["foo", "--region", "--no-editor"]);
    }

    /// A resident daemon's argv is a single bare token — the exact shape
    /// `is_resident_instance` looks for.
    #[cfg(target_os = "macos")]
    #[test]
    fn parse_procargs2_finds_the_bare_resident_argument() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&1i32.to_ne_bytes());
        buf.extend_from_slice(b"/Applications/Cosmic Capture Kit.app/Contents/MacOS/cosmic-capture-kit\0");
        buf.extend_from_slice(&[0u8; 1]);
        buf.extend_from_slice(b"resident\0");
        assert_eq!(parse_procargs2(&buf), vec!["resident"]);
    }

    /// `argc=0` and a too-short buffer both yield an empty argv rather than panicking —
    /// the shape a truncated or unreadable `sysctl` answer would produce.
    #[cfg(target_os = "macos")]
    #[test]
    fn parse_procargs2_degrades_to_empty_rather_than_panicking() {
        assert!(parse_procargs2(&[]).is_empty());
        assert!(parse_procargs2(&[1, 2, 3]).is_empty()); // shorter than the argc field itself
        assert!(parse_procargs2(&0i32.to_ne_bytes()).is_empty()); // argc=0
        // argc claims more strings than the buffer actually holds.
        let mut truncated = Vec::new();
        truncated.extend_from_slice(&5i32.to_ne_bytes());
        truncated.extend_from_slice(b"/bin/foo\0");
        truncated.extend_from_slice(&[0u8]);
        truncated.extend_from_slice(b"only-one\0");
        assert_eq!(parse_procargs2(&truncated), vec!["only-one"]);
    }
}
