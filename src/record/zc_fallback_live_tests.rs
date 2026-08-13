//! LIVE proof that ONE user action starts ONE recording session (DRAGON-422).
//!
//! ## What went wrong, and why the shape of this test is what it is
//!
//! A zero-copy recording attempt declined (its portal stream never delivered a dmabuf
//! frame), and the fallback answered by starting the session over on the CPU path —
//! after CLEARING the stop flag, which the user had set four seconds earlier when they
//! pressed stop. That second session was still recording a minute later: the preview
//! spinner was already up, so there was no stop button anywhere in the UI, and nothing
//! left that would ever set the flag again. And because it never finished,
//! `RecordHandle.done` was never filled, so the app's own failure path never ran either.
//!
//! Everything downstream of that is invisible to a unit test — but the thing that
//! actually went wrong is perfectly countable: how many recording sessions did one call
//! start? Each session begins with an audio pre-flight, and
//! [`super::owned::preflights_started`] counts them. Two is the bug, and it is the same
//! two that showed up in the field as two mic ffmpegs in a process list.
//!
//! So these tests drive the REAL entry point the app calls (`start_pipewire_recording`)
//! and count pre-flights. They are deliberately NOT written against durations, exit
//! codes or file contents: the failing session produced a perfectly plausible-looking
//! growing file, and the count is the only thing that says the recording was started
//! twice.
//!
//! ## Why a dead portal fd
//!
//! There is no live xdg-portal ScreenCast session available to a test (per this repo's
//! rule against tests needing a compositor), and the zero-copy path's decline is reached
//! through the portal fd. `/dev/null` is a real fd that `pipewire::consume_dmabuf`
//! rejects at `connect_fd` — the same "nothing was recorded" decline the field failure
//! took the long way round to, reached in milliseconds. What is under test is what the
//! CALLER does with a decline, which is identical either way.

use super::owned::preflights_started;
use super::{PipewireRecordParams, RecordSettings, start_pipewire_recording};
use std::os::fd::OwnedFd;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Serializes these tests against each other AND against `media_clock_e2e_tests`
/// (DRAGON-554: the crate-wide `super::recording_globals_lock`): the pre-flight
/// counter delta and the forced-failure env seam are process-global, so a
/// module-local lock could not stop that module's pre-flights from landing inside
/// this module's delta window, or this module's forced-failure env from failing
/// that module's real pre-flight mid-test.
fn test_lock() -> &'static Mutex<()> {
    super::recording_globals_lock()
}

/// Clears the forced-failure seam on drop, including during a panic's unwind.
struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: only constructed while holding `test_lock()`, held for this guard's
        // whole lifetime — no concurrent env access.
        unsafe {
            std::env::remove_var("CCK_TEST_FORCE_OWNED_FAILURE");
        }
    }
}

/// A real fd the PipeWire client will refuse to speak to (see the module doc).
fn dead_portal_fd() -> OwnedFd {
    OwnedFd::from(std::fs::File::open("/dev/null").expect("/dev/null is openable"))
}

fn settings(out: &std::path::Path) -> RecordSettings {
    RecordSettings {
        fps: 30,
        // Anything but "software", or the zero-copy attempt is skipped outright.
        preferred_encoder: "auto".to_string(),
        encoder_hint: None,
        presets: crate::encode::Presets::default(),
        zero_copy: true,
        mic: true,
        system_audio: true,
        bitrate_kbps: 8000,
        audio_offset_ms: 0,
        auto_device_compensation: false,
        max_res: (3840, 2160),
        metadata: String::new(),
        out_path: out.to_path_buf(),
        // DRAGON-673: a live test has no countdown, so media 0 is the settled pipeline
        // with no gate to hold for.
        start_gate: None,
    }
}

/// The worker's result, or `None` if it had not finished within `budget`.
fn wait_done(
    handle: &super::RecordHandle,
    budget: Duration,
) -> Option<Result<std::path::PathBuf, String>> {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if let Some(r) = handle.done.lock().ok().and_then(|g| g.clone()) {
            return Some(r);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// Everything a finished session should have taken with it and did not: its audio FIFOs
/// still sitting in the runtime dir, and any process still holding one open.
///
/// This is the field evidence, checked directly. The preserved process list for the wedge
/// showed both `/run/user/1000/cosmic-capture-kit.<pid>.<token>.{mic,sys}mix.pcm` still
/// present and a muxer ffmpeg still writing with those exact paths on its command line, a
/// minute after a two-second capture.
///
/// The names carry our pid AND `token`, the pre-flight number this session was given, so
/// this sees ONLY the session under test — which is what makes it usable in a suite that
/// runs tests in parallel, where a bare "are there ffmpeg children?" sweep picks up
/// whatever the `media_clock_e2e` tests happen to be running in this same process, and
/// even a pid-only match picks up their FIFOs. Their absence also says the teardown ran
/// to completion rather than being abandoned partway: the same `cleanup()` /
/// `PumpHandle::join` step that unlinks them is the one that reaps the mic capture.
fn session_leftovers(token: u32) -> Vec<String> {
    let me = std::process::id();
    let prefix = format!("cosmic-capture-kit.{me}.{token}.");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(crate::util::runtime_dir()) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(&prefix) && (n.ends_with(".micmix.pcm") || n.ends_with(".sysmix.pcm"))
            {
                out.push(format!("FIFO {n}"));
            }
        }
    }
    // And anything still running against them — the runaway muxer's own signature.
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
                continue; // exited between the readdir and here
            };
            let cmdline = String::from_utf8_lossy(&raw);
            if cmdline.contains(&prefix) {
                out.push(format!("process {pid}"));
            }
        }
    }
    out
}

/// Whether a PulseAudio-compatible server is reachable — the pre-flight the second test
/// needs to actually SUCCEED. Loud skip (never a silent pass) when it is not, mirroring
/// `media_clock_e2e_tests`' convention.
fn pulse_available() -> bool {
    std::process::Command::new("pactl")
        .arg("info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A recording whose VIDEO side can never come up must not start any audio at all
/// (DRAGON-658).
///
/// This is the ordering itself, counted. Every worker now brings its capture up first,
/// confirms a genuinely real frame, and only then runs the audio pre-flight, so a portal
/// stream that will never deliver one (here, an fd the PipeWire client refuses outright)
/// must cost ZERO pre-flights: no mic ffmpeg, no pulse client, no FIFOs, on either the
/// zero-copy attempt or the CPU path it falls back to. Before the reordering this same
/// call started one on each of them.
///
/// Needs no sound server precisely BECAUSE nothing audio-side should be touched, which is
/// what makes it a usable everyday net rather than a live test.
#[test]
fn a_recording_that_never_captures_starts_no_audio() {
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());

    let dir = std::env::temp_dir().join(format!("cck-d658-order-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let before = preflights_started();
    let handle = start_pipewire_recording(PipewireRecordParams {
        fd: dead_portal_fd(),
        node_id: 0,
        crop: None,
        settings: settings(&dir.join("out.mp4")),
    });

    let result = wait_done(&handle, Duration::from_secs(60))
        .expect("a recording that cannot capture must report, not hang");
    let started = preflights_started() - before;

    assert!(result.is_err(), "nothing was captured, so this must not report success: {result:?}");
    assert_eq!(
        started, 0,
        "the video side never delivered a frame, so no audio pre-flight may have run; \
         {started} did, which means a worker still starts audio before it knows it can capture"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A failed audio pre-flight is the RECORDING's failure, not an invitation to try the
/// whole thing again.
///
/// The CPU path's first act used to be the very same pre-flight, against the very same
/// sound server, so falling back to it could only fail the same way, one whole recording
/// later, with a second set of capture processes started and torn down on the way. Needs
/// nothing but the forced-failure seam, so it runs everywhere.
///
/// Since DRAGON-658 the pre-flight runs only AFTER a real frame is confirmed, so on this
/// dead-fd fixture it is never reached at all and the honest count is 0 rather than 1 (the
/// sibling test above pins that directly). The claim this one makes is the DRAGON-422 one
/// and it is unchanged: one user action starts AT MOST one recording session, whatever
/// fails and wherever it fails.
#[test]
fn a_failed_audio_preflight_does_not_start_a_second_session() {
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: the lock above is held for this test's whole body.
    unsafe {
        std::env::set_var("CCK_TEST_FORCE_OWNED_FAILURE", "1");
    }
    let _env = EnvGuard;

    let dir = std::env::temp_dir().join(format!("cck-d422-preflight-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let before = preflights_started();
    let handle = start_pipewire_recording(PipewireRecordParams {
        fd: dead_portal_fd(),
        node_id: 0,
        crop: None,
        settings: settings(&dir.join("out.mp4")),
    });

    let result = wait_done(&handle, Duration::from_secs(60))
        .expect("a recording that cannot start its audio must report, not hang");
    let started = preflights_started() - before;

    assert!(result.is_err(), "the recording must fail, not appear to succeed: {result:?}");
    assert!(
        started <= 1,
        "one user action must start AT MOST one recording session; {started} audio pre-flights \
         ran, which means the failed attempt was answered by starting the whole recording again"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stop requested while the zero-copy attempt is running is FINAL: the fallback must
/// not start a second recording behind it.
///
/// This is the field failure itself. The stop is set the instant the handle comes back,
/// which is while the worker is still in its pre-flight — exactly where the user's stop
/// landed live (they pressed it four seconds in, long before the attempt gave up). The
/// attempt then declines, and the only correct answer is to report that the recording
/// did not happen. Starting another one records something nobody asked for and hands
/// back a result nobody is waiting for any more.
///
/// WHICH pre-flight the stop lands in now depends on the box (DRAGON-425): the node
/// pre-flight runs first and can decline before any audio capture is started, so the
/// count this test reads is 0 or 1 rather than always 1. See the assertion below.
#[test]
fn a_stop_during_a_declined_attempt_is_not_answered_with_a_second_recording() {
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    if !pulse_available() {
        eprintln!(
            "SKIP a_stop_during_a_declined_attempt_is_not_answered_with_a_second_recording: \
             no PulseAudio-compatible server (pactl info failed); the pre-flight this test \
             needs to SUCCEED cannot run here"
        );
        return;
    }

    let dir = std::env::temp_dir().join(format!("cck-d422-stop-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let before = preflights_started();
    let handle = start_pipewire_recording(PipewireRecordParams {
        fd: dead_portal_fd(),
        node_id: 0,
        crop: None,
        settings: settings(&dir.join("out.mp4")),
    });
    // What `App::stop_recording` does, at the point the user did it: set the flag and
    // put up the preview's spinner. From here on, nothing in the UI can stop anything.
    handle.stop.store(true, Ordering::Relaxed);

    let result = wait_done(&handle, Duration::from_secs(90));
    let started = preflights_started() - before;

    // AT MOST one. The claim is that the fallback did not start a second recording, and
    // both readings satisfy it; which one a box produces is a property of its hardware
    // (DRAGON-425). Where the node pre-flight can `Use` a node, the attempt starts its
    // audio pre-flight and is then stopped -> 1. Where it `Decline`s, because the session
    // renders on a GPU no VAAPI node here can import, the attempt returns before any audio
    // exists and the stop keeps the CPU path from starting one -> 0. Pinning exactly 1
    // pinned the pre-DRAGON-425 ordering, in which every attempt paid for real audio
    // capture before it was allowed to discover it could never encode at all.
    assert!(
        started <= 1,
        "a stop was requested during the attempt, so the fallback must not start a second \
         recording; {started} audio pre-flights ran"
    );
    let result = result.expect(
        "the recording must report a result so the app can tell the user; a session that \
         never finishes is what left the preview spinning forever",
    );
    let reason = result.expect_err("nothing was recorded, so this must not report success");
    assert!(
        reason.contains("stopped"),
        "the reason reaches the user verbatim, so it must say what happened: {reason}"
    );
    // And the session cleaned up after itself. The field evidence for this bug was a
    // process list and a runtime dir, not a log (see `session_leftovers`).
    // `before` is the pre-flight number this session was handed (see `session_leftovers`).
    let stray = session_leftovers(before);
    assert!(
        stray.is_empty(),
        "the failed session left its own artifacts behind, owned by nobody: {stray:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
