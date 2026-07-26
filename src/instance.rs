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

// DRAGON-229: rustix (POSIX flock) is unix-only; the Windows lock functions below
// have their own (M0 fail-open) arms and never touch it. Unix selection unchanged.
#[cfg(unix)]
use rustix::fs::{FlockOperation, flock};
// DRAGON-229: both the flock `File` and the `Mutex` static that holds it are unix-only;
// Windows keeps its named-mutex handles inside `platform::windows::instance` instead.
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::sync::Mutex;

/// Holds the capture single-instance lock. Unlike the settings lock it can be
/// released at runtime: when an instance switches into the settings window it is
/// no longer "capturing", so it gives up this lock and another capture instance
/// may launch (even with "allow multiple instances" off).
///
/// DRAGON-229: unix-only — this holds the flock `File`. Windows holds its capture NAMED
/// MUTEX handle inside `platform::windows::instance` instead (strict split).
#[cfg(unix)]
static CAPTURE_LOCK: Mutex<Option<File>> = Mutex::new(None);

/// Path to the capture single-instance lock file.
#[cfg(unix)]
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
#[cfg(unix)]
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

/// Windows (DRAGON-229): the named-mutex capture guard. Body under
/// `platform::windows::instance` (strict split). Returns false if another capture
/// instance already holds the mutex; fails OPEN if the mutex can't be created, matching
/// the unix lock-file-create fail-open.
#[cfg(windows)]
pub(crate) fn acquire_lock() -> bool {
    crate::platform::windows::instance::acquire_lock()
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

/// Windows (DRAGON-237): the daemon single-instance guard. Body under
/// `platform::windows::instance` (strict split — a named mutex + a recorded pid). Returns
/// false if another daemon already holds it; fails OPEN if the mutex can't be created.
#[cfg(windows)]
pub(crate) fn acquire_daemon_lock() -> bool {
    crate::platform::windows::instance::acquire_daemon_lock()
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

/// Windows (DRAGON-237): the "capture NOW" second-launch UX. Body under
/// `platform::windows::instance` (strict split — pulses the daemon's named capture event).
/// Returns true if a running daemon was signalled.
#[cfg(windows)]
pub(crate) fn signal_existing_capture() -> bool {
    crate::platform::windows::instance::signal_existing_capture()
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

/// Release the capture single-instance lock. Called when this instance becomes a
/// settings window, so another capture instance may now launch.
#[cfg_attr(target_os = "macos", allow(dead_code))] // mac hands settings off to a fresh process (DRAGON-153)
pub fn release_capture_lock() {
    #[cfg(unix)]
    if let Ok(mut g) = CAPTURE_LOCK.lock() {
        *g = None; // dropping the File closes the fd and releases the flock
    }
    // Windows (DRAGON-229): close the capture named-mutex handle so another capture
    // instance may create it fresh (strict split — body under platform::windows).
    #[cfg(windows)]
    crate::platform::windows::instance::release_capture_lock();
}

/// Acquire the *settings* single-instance lock so only one settings pane can be
/// open across all instances, regardless of "allow multiple instances". Held for
/// the process lifetime on success (closing settings ends the process anyway).
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
// With "allow multiple instances" ON a RECORDING session and a fresh capture — or a
// SELF-CAPTURE of CCK's own preview-editor window — coexist as independent processes.
// Two things then need cross-process knowledge the single-instance locks don't carry:
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
// allow-multiple instances never collide. A crash can leave a STALE marker, so the
// one consumer that reads markers for pids it hasn't already proven live
// ([`any_other_recording`]) re-checks liveness and sweeps dead-pid files; the sweep
// only ever queries markers for a pid it already found LIVE, so it needs no probe.
// Plain files (portable `std::fs`), so the whole mechanism is byte-identical on Linux,
// macOS and Windows — only the liveness probe below is per-platform.

/// Marker suffix: this pid has a recording in progress (incl. paused).
const RECORDING_MARKER: &str = "recording";
/// Marker suffix: this pid has a preview editor open.
const PREVIEW_MARKER: &str = "preview";

/// Per-pid state-marker sidecar path (`{runtime}/cosmic-capture-kit.<pid>.<suffix>`).
fn state_marker_path(pid: u32, suffix: &str) -> String {
    format!("{}/cosmic-capture-kit.{pid}.{suffix}", crate::util::runtime_dir())
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

fn set_self_marker(suffix: &str, active: bool) {
    let path = state_marker_path(std::process::id(), suffix);
    if active {
        let _ = std::fs::File::create(&path);
    } else {
        let _ = std::fs::remove_file(&path);
    }
}

/// Whether the capture overlay should SPARE a sibling instance from the collapse sweep,
/// as a pure function of the setting + that sibling's observed state. With "allow
/// multiple instances" ON a recording or preview sibling is a live workspace to keep;
/// a bare selector sibling still gets collapsed. With it OFF the historical
/// single-instance collapse applies (nothing extra is spared), so a fresh capture still
/// replaces an existing preview exactly as before.
pub fn should_spare_sibling(allow_multiple: bool, recording: bool, preview: bool) -> bool {
    allow_multiple && (recording || preview)
}

/// Whether the VIDEO capture kind should be offered. Disabled while another instance is
/// already recording (only one recording at a time; still image capture stays allowed).
pub fn video_capture_allowed(external_recording: bool) -> bool {
    !external_recording
}

/// Whether a fresh capture launch should run as its OWN independent instance rather than
/// stepping aside for / signalling the existing one. Purely the "allow multiple
/// instances" setting today; a named seam so the launch decision is documented + tested
/// in one place.
pub fn wants_own_instance(allow_multiple: bool) -> bool {
    allow_multiple
}

/// Whether ANOTHER live instance currently has a recording in progress (its recording
/// marker exists and its pid is still alive). Consumed by the capture overlay to DISABLE
/// the video kind while a recording runs elsewhere. Sweeps markers left behind by dead
/// pids as it scans, so a crashed recorder can't wedge the toggle off forever.
pub(crate) fn any_other_recording() -> bool {
    let dir = crate::util::runtime_dir();
    let self_pid = std::process::id();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    let suffix = format!(".{RECORDING_MARKER}");
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
            // Stale marker from a crashed recorder — sweep it so the toggle re-enables.
            let _ = std::fs::remove_file(entry.path());
        }
    }
    found
}

/// Whether `pid` is a live process. Linux reads `/proc/<pid>`; other unix uses a
/// signal-0 probe (`ESRCH` = gone, `EPERM` = alive but not ours); Windows delegates to
/// the platform body. Best-effort — a false "live" only keeps the video toggle disabled
/// a little longer, never a hard failure.
#[cfg(target_os = "linux")]
fn pid_is_live(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}
#[cfg(all(unix, not(target_os = "linux")))]
fn pid_is_live(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 delivers nothing; it only probes existence/permission.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
#[cfg(windows)]
fn pid_is_live(pid: u32) -> bool {
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
/// DRAGON-322: with `allow_multiple` on, a RECORDING or PREVIEW sibling is also spared
/// (its per-pid state marker) so a recording session + a capture — including a
/// self-capture of the open preview — coexist. With it off, nothing extra is spared and
/// the historical single-instance collapse (a fresh capture replaces the old preview) is
/// byte-identical.
#[cfg(not(windows))]
pub fn close_other_instances(allow_multiple: bool) {
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
        // DRAGON-322: keep a live recording / preview sibling under allow-multiple.
        let recording =
            std::path::Path::new(&state_marker_path(pid, RECORDING_MARKER)).exists();
        let preview = std::path::Path::new(&state_marker_path(pid, PREVIEW_MARKER)).exists();
        if should_spare_sibling(allow_multiple, recording, preview) {
            log::info!(
                "DRAGON-322: sparing sibling pid {pid} (recording={recording} preview={preview})"
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

/// Windows (DRAGON-229): the Toolhelp sibling-capture sweep. Body under
/// `platform::windows::instance` (strict split) — matches siblings by full exe path,
/// spares the settings window (its recorded pid), and force-terminates the rest (the
/// analog of the Linux uncaught SIGTERM). Only matters with "allow multiple instances" on.
///
/// DRAGON-322: `allow_multiple` is threaded through so the Windows body can SPARE a
/// recording / preview sibling (its per-pid state marker) exactly like the unix body,
/// keeping a recording session + a concurrent capture alive.
#[cfg(windows)]
pub fn close_other_instances(allow_multiple: bool) {
    crate::platform::windows::instance::close_other_instances(allow_multiple);
}

/// DRAGON-322: whether `pid`'s per-pid RECORDING / PREVIEW marker exists. The predicate
/// the Windows sibling sweep uses (its pids come from a live Toolhelp snapshot, so no
/// liveness probe is needed here). Windows-only — the unix sweep inlines the same two
/// `Path::exists` checks against [`state_marker_path`].
#[cfg(windows)]
pub(crate) fn marker_flags(pid: u32) -> (bool, bool) {
    (
        std::path::Path::new(&state_marker_path(pid, RECORDING_MARKER)).exists(),
        std::path::Path::new(&state_marker_path(pid, PREVIEW_MARKER)).exists(),
    )
}

/// Whether `pid` is the resident daemon: the daemon-lock holder, or a process
/// launched with the literal `resident` argument (the autostart / toggle-on shape;
/// also covers a restarting daemon that hasn't won the lock yet). Never swept by
/// [`close_other_instances`]. `not(windows)`: the daemon-lock pid + `/proc` sweep it
/// serves are Linux/macOS-only.
#[cfg(not(windows))]
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
/// `not(windows)`: only the `/proc` sweep in [`close_other_instances`] calls it.
#[cfg(not(windows))]
fn is_settings_instance(pid: u32) -> bool {
    if settings_lock_pid() == Some(pid) {
        return true;
    }
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| b.split(|&c| c == 0).any(|a| a == b"--settings"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wants_own_instance_follows_the_setting() {
        // The launch decision (DRAGON-322): only "allow multiple instances" makes a
        // fresh capture run as its own process instead of stepping aside.
        assert!(!wants_own_instance(false));
        assert!(wants_own_instance(true));
    }

    #[test]
    fn spare_only_protected_siblings_under_allow_multiple() {
        // Setting OFF: never spare — the historical single-instance collapse (a fresh
        // capture replaces the old preview) is preserved for every sibling state.
        for &rec in &[false, true] {
            for &prev in &[false, true] {
                assert!(
                    !should_spare_sibling(false, rec, prev),
                    "allow_multiple off must never spare (rec={rec} prev={prev})"
                );
            }
        }
        // Setting ON: spare exactly the recording / preview siblings; a bare selector
        // sibling (neither marker) still collapses so a committed shot wins the screen.
        assert!(!should_spare_sibling(true, false, false));
        assert!(should_spare_sibling(true, true, false)); // recording session
        assert!(should_spare_sibling(true, false, true)); // open preview (self-capture)
        assert!(should_spare_sibling(true, true, true));
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
}
