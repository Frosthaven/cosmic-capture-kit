//! Headless end-to-end proof of the media-clock OWNED PipeWire pipeline
//! (DRAGON-125 chunk B1): drives the REAL [`super::pump`] engine,
//! [`crate::encode::spawn_ffmpeg_media_clock`], [`crate::audio::capture::MonitorCapture`],
//! [`crate::audio::clean_mic::setup_clean_mic_tap`], and
//! [`super::finalize::finalize_with_intervals`] together — synthetic video frames (a
//! flash pattern, paced by the real `VideoTicker`) + a real PulseAudio null-sink
//! standing in for "system audio" (no live PipeWire portal is available headlessly,
//! so this is the "owned path SHAPE" proof the ticket asks for, not a call through
//! `record_pipewire` itself).
//!
//! Unlike the rest of the suite, these tests SHELL OUT to real ffmpeg/ffprobe/pactl
//! and take several real wall-clock seconds: they LOUDLY skip (never a silent pass)
//! when those tools aren't usable, mirroring `av_sync_tests`/`monitor_capture_smoke`'s
//! convention. Both tests mutate PROCESS-GLOBAL state (an env var the owned path's
//! pre-flight check reads, the mic-source override, and real pactl modules with
//! fixed names) so they're serialized against each other (and any future test that
//! might touch the same globals) via [`test_lock`], and reset it all through a
//! `Drop` guard so a mid-test panic can't leave it dirty for whatever runs next.
//!
//! Sync measurement is DIFFERENTIAL (mirroring `av_sync_tests`'s own reasoning): the
//! flash/beep offset measured BEFORE the pause is compared against the offset
//! measured AFTER it, rather than against an assumed-zero cross-process alignment
//! between this test's own wall clock and the independently-started beep-player
//! ffmpeg's internal sample clock — a constant startup skew between those two
//! cancels in the differential comparison, which is what the pause-handling code
//! could actually get wrong.
//!
//! ## Coverage extension to the screencopy/zero-copy owned workers (DRAGON-127)
//!
//! `record::screencopy::record_screencopy_owned` and
//! `record::zero_copy::record_pipewire_zero_copy_owned` /
//! `record_screencopy_zero_copy_owned` extend the SAME media-clock model this
//! harness drives — they push their video ticks through the identical
//! `super::pump` engine, `finalize::finalize_with_intervals`, and (for the raw-frame
//! screencopy worker) the identical `VideoTicker`/`spawn_ffmpeg_media_clock` pair.
//! This harness's own video "capture" is ALREADY synthetic (frames generated on
//! demand and fed through `due_video_ticks`, exactly like screencopy's on-demand
//! grabs — it never drove a live PipeWire portal either), so its five scenarios
//! (continuity, actionable pre-flight failure, tone continuity, early long pause,
//! pause-without-mute) already exercise every worker's shared engine byte-for-byte.
//! What ISN'T covered
//! here — and can't be, headlessly, per this repo's convention against tests that
//! need a live compositor/GPU (see CLAUDE.md) — is each worker's OWN capture-side
//! glue: screencopy's real wayland grab loop + damage-skip decision, and either
//! zero-copy worker's real GPU encoder session. Those are covered by: the pure
//! `encode::command` argument-shape tests
//! (`media_clock_encoded_command_*`/`media_clock_command_*`), the
//! `record::zero_copy::tests::trailing_frames_needed_*` tests for the screencopy
//! zero-copy worker's trailing-coverage math, and manual verification against the
//! real compositor/GPU this environment can't provide.

use super::pump::PumpConfig;
use super::sync_probe::{audio_rms_series, beep_times, flash_times, pair_offset, video_luma_series};
use super::{AudioChannel, ToggleEvent};
use crate::mixer::clock::MediaClock;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Serializes the E2E tests (and guards against any future one) against each
/// other: they mutate the SAME process-global env vars
/// (`CCK_TEST_MONITOR_SOURCE`, `CCK_TEST_FORCE_OWNED_FAILURE`) and
/// `crate::audio::config`'s mic-source override, and claim fixed-name pactl
/// modules — none of which is safe to do from two threads at once (`cargo test`'s
/// default runner is multi-threaded). Since DRAGON-554 this is the CRATE-WIDE
/// recording-globals lock (`super::recording_globals_lock`), shared with
/// `zc_fallback_live_tests`: those tests read the pre-flight counter as a delta and
/// set the same forced-failure env, so a module-local lock here was no lock at all
/// against them.
fn test_lock() -> &'static Mutex<()> {
    super::recording_globals_lock()
}

/// Resets the process-global state these tests mutate (`CCK_TEST_MONITOR_SOURCE`,
/// `CCK_TEST_FORCE_OWNED_FAILURE`, the mic-source override) on drop — including
/// during a panic's unwind — so a failing assertion can never leave the next test
/// (or the user's own recordings) reading a stale test-only value.
struct GlobalStateGuard;
impl Drop for GlobalStateGuard {
    fn drop(&mut self) {
        // SAFETY: only ever constructed while holding `test_lock()`, which is held
        // for this guard's entire lifetime — no concurrent env access.
        unsafe {
            std::env::remove_var("CCK_TEST_MONITOR_SOURCE");
            std::env::remove_var("CCK_TEST_FORCE_OWNED_FAILURE");
        }
        crate::audio::config::set_mic_source("");
    }
}

const FPS: u32 = 30;
const W: u32 = 320;
const H: u32 = 180;

/// Whether ffmpeg/ffprobe/pactl all respond — mirrors `av_sync_tests::have_ffmpeg`,
/// plus a pactl reachability check this suite additionally needs.
fn have_e2e_tools() -> bool {
    let responds_version = |tool: std::path::PathBuf| {
        crate::util::quiet_command(tool)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    let pactl_ok = Command::new("pactl")
        .arg("info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    responds_version(crate::util::ffmpeg_path()) && responds_version(crate::util::ffprobe_path()) && pactl_ok
}

macro_rules! require_e2e_tools {
    ($name:literal) => {
        if !have_e2e_tools() {
            eprintln!(
                "SKIPPED (loud): {} needs ffmpeg+ffprobe+pactl reachable — the media-clock \
                 E2E proof did not run",
                $name
            );
            return;
        }
    };
}

/// A pactl `module-null-sink`, unloaded on drop — a disposable virtual audio device
/// this suite plays into / captures from, never touching the user's real devices.
struct NullSink {
    module_id: String,
    name: String,
}

impl NullSink {
    fn load(name: &str) -> Option<Self> {
        let out = Command::new("pactl")
            .args([
                "load-module",
                "module-null-sink",
                &format!("sink_name={name}"),
                &format!("sink_properties=device.description={name}"),
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let module_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if module_id.is_empty() {
            return None;
        }
        Some(Self { module_id, name: name.to_string() })
    }

    fn monitor_source(&self) -> String {
        format!("{}.monitor", self.name)
    }
}

impl Drop for NullSink {
    fn drop(&mut self) {
        let _ = Command::new("pactl")
            .args(["unload-module", &self.module_id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Spawn an ffmpeg process playing a synthesized tone + scheduled beeps into
/// `sink`'s input for `duration_secs`, killed on drop. `beep_times_secs` are ITS OWN
/// internal `aevalsrc` clock seconds (see the module doc for why this test never
/// assumes that clock is wall-clock-aligned with the caller's).
struct BeepPlayer(Child);
impl BeepPlayer {
    fn start(sink: &str, duration_secs: f64, beep_times_secs: &[f64]) -> Option<Self> {
        let beeps: String = beep_times_secs
            .iter()
            .map(|t| format!("between(t,{t},{})", t + 0.08))
            .collect::<Vec<_>>()
            .join("+");
        // A continuous low tone (always audible) PLUS louder beeps at the scheduled
        // instants — the continuous tone is what makes a MUTE window's edges show
        // up cleanly in the RMS series (a beeps-only signal would already read as
        // "silent" between beeps regardless of whether muting works). An empty
        // schedule yields the bare tone (the continuity E2E measures its sample-level
        // smoothness, so it must carry no legitimate large sample steps of its own).
        let expr = if beeps.is_empty() {
            "0.05*sin(2*PI*300*t)".to_string()
        } else {
            format!("0.05*sin(2*PI*300*t) + 0.8*sin(2*PI*1000*t)*({beeps})")
        };
        let child = crate::util::ffmpeg_command()
            .args(["-hide_banner", "-loglevel", "error"])
            .args(["-f", "lavfi", "-i", &format!("aevalsrc='{expr}':s=48000:d={duration_secs}")])
            .args(["-f", "pulse", "-device", sink, "cck-e2e-beep"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .ok()?;
        Some(Self(child))
    }
}
impl Drop for BeepPlayer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One raw video frame's bytes for the software (RGBA) encode path: solid black or
/// solid white (a "flash"), full alpha.
fn frame_rgba(bright: bool, w: u32, h: u32) -> Vec<u8> {
    let px = if bright { 0xFF } else { 0x00 };
    vec![px; (w * h * 4) as usize]
}

/// The falling edge (last `>= threshold` sample immediately followed by a
/// `< threshold` one) nearest `expected`, searched within `+-window`.
fn falling_edge_near(series: &[(f64, f32)], threshold: f32, expected: f64, window: f64) -> Option<f64> {
    let mut prev_high: Option<bool> = None;
    for &(t, v) in series {
        if t < expected - window {
            prev_high = Some(v >= threshold);
            continue;
        }
        if t > expected + window {
            break;
        }
        if prev_high == Some(true) && v < threshold {
            return Some(t);
        }
        prev_high = Some(v >= threshold);
    }
    None
}

/// The rising-edge mirror of [`falling_edge_near`].
fn rising_edge_near(series: &[(f64, f32)], threshold: f32, expected: f64, window: f64) -> Option<f64> {
    let mut prev_low: Option<bool> = None;
    for &(t, v) in series {
        if t < expected - window {
            prev_low = Some(v < threshold);
            continue;
        }
        if t > expected + window {
            break;
        }
        if prev_low == Some(true) && v >= threshold {
            return Some(t);
        }
        prev_low = Some(v < threshold);
    }
    None
}

/// ffprobe: total packet count on `stream` (`"v:0"`, `"a:0"`, ...).
fn probe_packet_count(path: &std::path::Path, stream: &str) -> Option<usize> {
    let out = crate::util::ffprobe_command()
        .args(["-v", "error", "-select_streams", stream])
        .args(["-count_packets", "-show_entries", "stream=nb_read_packets", "-of", "csv=p=0"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// ffprobe: every video packet's PTS (seconds), in container order.
fn probe_video_pts(path: &std::path::Path) -> Vec<f64> {
    let Ok(out) = crate::util::ffprobe_command()
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "packet=pts_time", "-of", "csv=p=0"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<f64>().ok())
        .collect()
}

/// ffprobe: container duration (seconds).
fn probe_duration(path: &std::path::Path) -> Option<f64> {
    let out = crate::util::ffprobe_command()
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Instants (this process's clock) of each control action taken during a session —
/// used to compute EXPECTED media positions via a throwaway [`MediaClock`] fed the
/// identical history, rather than assuming perfectly-timed sleeps.
struct SessionTimeline {
    t0: Instant,
    pause_at: Instant,
    resume_at: Instant,
    mute_at: Instant,
    unmute_at: Instant,
    session_end: Instant,
}

/// Drive one owned-shaped session: two null sinks (`{prefix}mic` / `{prefix}sys`), a
/// beep player on the system sink, the real `try_start_owned_audio` pre-flight
/// check, and — if that succeeds — the real pump + a synthetic video loop for
/// `total_secs`, pausing `[pause_at_secs, pause_at_secs+pause_len_secs]` and muting
/// the system channel `[mute_at_secs, mute_at_secs+mute_len_secs]` (all real
/// seconds since session start). `beep_times_secs` schedules BOTH the beeps (real
/// seconds on the beep player's own clock) AND the video flashes (real seconds on
/// this process's clock) — kept identical so every flash has a matching beep
/// (unless it falls inside the mute window, which the caller should avoid putting
/// in this schedule; the mute correctness is instead verified via the continuous
/// tone's own edges). Returns `None` if the pre-flight check itself failed.
#[allow(clippy::too_many_arguments)]
fn run_owned_shaped_session(
    prefix: &str,
    total_secs: f64,
    pause_at_secs: f64,
    pause_len_secs: f64,
    mute_at_secs: f64,
    mute_len_secs: f64,
    beep_times_secs: &[f64],
) -> Option<(std::path::PathBuf, SessionTimeline)> {
    let mic_sink = NullSink::load(&format!("{prefix}mic"))?;
    let sys_sink = NullSink::load(&format!("{prefix}sys"))?;
    crate::audio::config::set_mic_source(&mic_sink.monitor_source());
    // SAFETY: the caller holds `test_lock()` for this whole call.
    unsafe {
        std::env::set_var("CCK_TEST_MONITOR_SOURCE", sys_sink.monitor_source());
    }
    let _beep = BeepPlayer::start(&sys_sink.name, total_secs + 1.0, beep_times_secs)?;
    // Give the beep player a moment to actually start producing audio before the
    // pre-flight smoke check looks for it (mirrors the ~300ms settle used
    // elsewhere in this codebase's pulse code).
    std::thread::sleep(Duration::from_millis(300));

    let owned = super::owned::try_start_owned_audio().ok()?;
    let super::owned::OwnedAudioStart {
        capture_start, mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx,
    } = owned;

    let out_path = std::env::temp_dir().join(format!("cck-e2e-{prefix}{}.mp4", std::process::id()));
    let temp_path = super::recording_temp_path(&out_path);
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&temp_path);

    let presets = crate::encode::Presets::default();
    let plan = crate::encode::EncodePlan::resolve("software", W, H, &presets);
    let Ok(mut child) = crate::encode::spawn_ffmpeg_media_clock(
        W, H, W, H, FPS, &plan, 4000, &temp_path, &mic_fifo_path, &sys_fifo_path,
    ) else {
        drop(mic_tap);
        let _ = monitor.stop();
        let _ = std::fs::remove_file(&mic_fifo_path);
        let _ = std::fs::remove_file(&sys_fifo_path);
        return None;
    };
    let mut stdin = child.stdin.take().expect("piped stdin");

    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let events: Mutex<Vec<ToggleEvent>> = Mutex::new(Vec::new());

    // Media time 0 is the pre-flight instant, exactly as every production worker
    // anchors it (DRAGON-417) — the session's own pacing below still runs off its
    // own `loop_t0`, so the scenario timings are unchanged.
    let t0 = capture_start;
    let loop_t0 = Instant::now();
    let cfg = PumpConfig {
        fps: FPS,
        audio_offset_ms: 0,
        auto_device_compensation: false,
        mic_on0: true,
        sys_on0: true,
        duck_system: false,
    };
    let mut timeline =
        SessionTimeline { t0, pause_at: t0, resume_at: t0, mute_at: t0, unmute_at: t0, session_end: t0 };

    let final_path = std::thread::scope(|scope| {
        let (pump_handle, mut ticker) = super::pump::spawn(
            scope, t0, cfg, mic_fifo_path.clone(), sys_fifo_path.clone(), mic_tap, mic_rx, monitor,
            sys_rx, &stop, &paused, &events,
        )
        .expect("pump spawn must succeed in the E2E harness");

        let (mut paused_on, mut paused_off, mut muted_on, mut muted_off) = (false, false, false, false);
        // A deliberate mid-session child process (stands in for the app's level
        // meter / capture helpers — the DRAGON-125 field failure's leak vector):
        // spawned while the pump's FIFO write ends are open and kept alive across
        // the whole stop tail. If those fds ever leak into children again (a
        // missing O_CLOEXEC), THIS process holds the FIFOs write-open, ffmpeg's
        // audio inputs never see EOF, and the teardown assertion below trips at
        // the reap bound — exactly the field wedge, reproduced headlessly.
        let mut leak_probe: Option<Child> = None;
        loop {
            let elapsed = loop_t0.elapsed().as_secs_f64();
            if elapsed >= 1.0 && leak_probe.is_none() {
                leak_probe = Command::new("sleep")
                    .arg("120")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok();
            }
            if elapsed >= pause_at_secs && !paused_on {
                paused.store(true, Ordering::Relaxed);
                timeline.pause_at = Instant::now();
                paused_on = true;
            }
            if elapsed >= pause_at_secs + pause_len_secs && !paused_off {
                paused.store(false, Ordering::Relaxed);
                timeline.resume_at = Instant::now();
                paused_off = true;
            }
            if elapsed >= mute_at_secs && !muted_on {
                let at = Instant::now();
                events.lock().unwrap().push((at, AudioChannel::Sys, false));
                timeline.mute_at = at;
                muted_on = true;
            }
            if elapsed >= mute_at_secs + mute_len_secs && !muted_off {
                let at = Instant::now();
                events.lock().unwrap().push((at, AudioChannel::Sys, true));
                timeline.unmute_at = at;
                muted_off = true;
            }
            if elapsed >= total_secs {
                break;
            }
            let bright = beep_times_secs.iter().any(|&f| elapsed >= f && elapsed < f + 0.1);
            let frame = frame_rgba(bright, W, H);
            let due = ticker.due_video_ticks(Instant::now());
            for _ in 0..due {
                let _ = stdin.write_all(&frame);
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        stop.store(true, Ordering::Relaxed);
        timeline.session_end = Instant::now();
        let pump_out = pump_handle.join();
        let more = ticker.ticks_to_cover(pump_out.final_media);
        let last_frame = frame_rgba(false, W, H);
        for _ in 0..more {
            let _ = stdin.write_all(&last_frame);
        }
        drop(stdin);
        // The teardown bound, asserted: with every input EOF'd (video stdin above,
        // the audio FIFOs by the pump's writer), a healthy ffmpeg flushes and
        // exits in ~a second. The DRAGON-125 field wedge (leaked FIFO write fds
        // kept EOF from ever arriving; demuxers blocked in read() until the 30s
        // kill) sat at exactly this line — the close-on-exec fix is what makes
        // this pass.
        let eof_at = Instant::now();
        let reaped = super::wait_or_kill(&mut child, Duration::from_secs(30));
        let teardown = eof_at.elapsed();
        assert!(
            matches!(&reaped, Ok(s) if s.success()),
            "capture ffmpeg must exit cleanly after its inputs close (got {reaped:?})"
        );
        assert!(
            teardown < Duration::from_secs(10),
            "capture ffmpeg must exit promptly after EOF, not ride out the reap bound \
             (took {teardown:?}; the leaked-write-fd wedge took the full 30s)"
        );
        if let Some(mut probe) = leak_probe.take() {
            let _ = probe.kill();
            let _ = probe.wait();
        }

        super::finalize::finalize_with_intervals(
            &temp_path,
            &out_path,
            &pump_out.mic_off,
            &pump_out.sys_off,
            plan.is_hevc(),
            "cck-e2e-test",
        )
        .ok()
    });
    let _ = std::fs::remove_file(&temp_path);
    final_path.map(|p| (p, timeline))
}

// ---------------------------------------------------------------------------

/// E2E-1 (continuity): a 10s owned-shaped session, PAUSE 3..5, sys-MUTE 6..7.
/// Asserts total duration, video packet count, flash/beep alignment (differential,
/// before vs after the pause), the muted window's edges, and PTS monotonicity.
#[test]
fn media_clock_owned_session_continuity_e2e() {
    require_e2e_tools!("media_clock_owned_session_continuity_e2e");
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _guard = GlobalStateGuard;

    // Flashes/beeps before the pause (2) and after both the pause and the mute
    // (3) — none scheduled INSIDE the mute window: the mute's correctness is
    // verified via the continuous tone's own edges instead (see the module doc),
    // so a flash with no matching beep never risks a bogus pairing.
    let beeps = [1.0, 2.0, 5.5, 8.0, 9.0];
    let Some((out_path, timeline)) =
        run_owned_shaped_session("cck_e2e1_", 10.0, 3.0, 2.0, 6.0, 1.0, &beeps)
    else {
        panic!(
            "the owned-shaped session's pre-flight/session setup failed — this is the \
             harness itself, not the thing under test; check pactl/ffmpeg are reachable \
             and no stale cck_e2e1_* pactl modules are lingering"
        );
    };

    // ---- Expected media positions, from a throwaway clock fed the SAME history ----
    let mut expect = MediaClock::new(timeline.t0);
    expect.pause(timeline.pause_at);
    expect.resume(timeline.resume_at);
    let expected_final_media = expect.media_at(timeline.session_end);
    let expected_mute_start = expect.media_at(timeline.mute_at);
    let expected_mute_end = expect.media_at(timeline.unmute_at);

    // ---- Duration / packet count / PTS monotonicity ----
    let duration = probe_duration(&out_path).expect("ffprobe duration");
    let packets = probe_packet_count(&out_path, "v:0").expect("ffprobe video packet count");
    let pts = probe_video_pts(&out_path);
    eprintln!(
        "E2E-1: expected_final_media={expected_final_media:.3}s duration={duration:.3}s \
         packets={packets} (fps*media={:.1}) expected_mute=[{expected_mute_start:.3},\
         {expected_mute_end:.3}]",
        expected_final_media * FPS as f64,
    );
    assert!(
        (duration - expected_final_media).abs() < 0.2,
        "duration {duration:.3}s must be within 0.2s of the expected media length \
         {expected_final_media:.3}s (~8s: 10s wall − 2s paused)"
    );
    // PER-STREAM durations, not just the container's (= the LONGEST stream): the
    // DRAGON-125 field regression shipped a file whose container duration looked
    // plausible while video (8.13s of media) and audio (10s of wall) disagreed by
    // exactly the pause — an assertion on `format=duration` alone can never catch
    // that class again.
    let vdur = probe_stream_duration(&out_path, "v:0").expect("ffprobe video duration");
    let adur = probe_stream_duration(&out_path, "a:0").expect("ffprobe audio duration");
    eprintln!("E2E-1: per-stream durations video={vdur:.3}s audio={adur:.3}s");
    assert!(
        (vdur - expected_final_media).abs() < 0.2,
        "VIDEO stream duration {vdur:.3}s must be within 0.2s of media {expected_final_media:.3}s"
    );
    assert!(
        (adur - expected_final_media).abs() < 0.2,
        "AUDIO stream duration {adur:.3}s must be within 0.2s of media {expected_final_media:.3}s \
         (an audio track on WALL time — pause not excluded — is the DRAGON-125 regression)"
    );
    let expected_packets = (expected_final_media * FPS as f64).round() as i64;
    assert!(
        (packets as i64 - expected_packets).abs() <= 2,
        "video packet count {packets} must be within 2 of fps*media = {expected_packets}"
    );
    assert!(pts.windows(2).all(|w| w[1] > w[0]), "video PTS must be strictly increasing: {pts:?}");
    let steps: Vec<f64> = pts.windows(2).map(|w| w[1] - w[0]).collect();
    let expected_step = 1.0 / FPS as f64;
    assert!(
        steps.iter().all(|&s| (s - expected_step).abs() < expected_step * 0.5),
        "no timestamp gap: every inter-packet step must be ~1/fps ({expected_step:.4}s); \
         got {steps:?}"
    );

    // ---- Flash/beep alignment: differential, before vs after the pause ----
    let luma = video_luma_series(&out_path, FPS as f64).expect("decode video luma");
    let rms = audio_rms_series(&out_path).expect("decode audio rms");
    let flashes = flash_times(&luma);
    let beep_onsets = beep_times(&rms);
    let split = |v: &[f64], before: bool| -> Vec<f64> {
        v.iter().copied().filter(|&t| (t < 3.5) == before).collect()
    };
    let (flashes_before, flashes_after) = (split(&flashes, true), split(&flashes, false));
    let (beeps_before, beeps_after) = (split(&beep_onsets, true), split(&beep_onsets, false));
    let before = pair_offset(&flashes_before, &beeps_before)
        .expect("need >=2 flash/beep pairs BEFORE the pause");
    let after = pair_offset(&flashes_after, &beeps_after)
        .expect("need >=2 flash/beep pairs AFTER the pause");
    eprintln!(
        "E2E-1: sync offset before={:.1}ms (spread {:.1}ms, {} pairs) after={:.1}ms \
         (spread {:.1}ms, {} pairs)",
        before.offset_secs * 1000.0, before.spread_secs * 1000.0, before.pairs,
        after.offset_secs * 1000.0, after.spread_secs * 1000.0, after.pairs,
    );
    assert!(
        (before.offset_secs - after.offset_secs).abs() < 0.060,
        "flash/beep alignment before ({:.1}ms) vs after ({:.1}ms) the pause must agree \
         within ±60ms — the pause must not introduce extra A/V skew",
        before.offset_secs * 1000.0, after.offset_secs * 1000.0,
    );

    // ---- Muted window: silent, edges within ±60ms of the expected positions ----
    const TONE_THRESHOLD: f32 = 0.02; // between silence (~0) and the continuous tone (~0.035 RMS)
    let mid_mute = (expected_mute_start + expected_mute_end) / 2.0;
    let mid_rms = rms
        .iter()
        .filter(|(t, _)| (*t - mid_mute).abs() < 0.1)
        .map(|(_, v)| *v)
        .fold(0.0f32, f32::max);
    assert!(mid_rms < TONE_THRESHOLD, "the muted window's midpoint must be silent (rms={mid_rms})");
    let start_edge = falling_edge_near(&rms, TONE_THRESHOLD, expected_mute_start, 0.3)
        .expect("must find the mute's falling edge near the expected start");
    let end_edge = rising_edge_near(&rms, TONE_THRESHOLD, expected_mute_end, 0.3)
        .expect("must find the mute's rising edge near the expected end");
    eprintln!(
        "E2E-1: mute edges measured=[{start_edge:.3},{end_edge:.3}] expected=\
         [{expected_mute_start:.3},{expected_mute_end:.3}]"
    );
    assert!(
        (start_edge - expected_mute_start).abs() < 0.060,
        "mute start edge {start_edge:.3}s must be within ±60ms of expected {expected_mute_start:.3}s"
    );
    assert!(
        (end_edge - expected_mute_end).abs() < 0.060,
        "mute end edge {end_edge:.3}s must be within ±60ms of expected {expected_mute_end:.3}s"
    );

    let _ = std::fs::remove_file(&out_path);
}

/// E2E-2 (actionable failure, no fallback): force the owned path's pre-flight
/// check to fail and verify `try_start_owned_audio` reports a named `Err` — the
/// exact signal `record_pipewire`'s caller now turns straight into the recording's
/// failure (DRAGON-127 retired the legacy recorder this used to fall back to).
///
/// The ticket's suggested mechanism (point `MonitorCapture` at a nonexistent
/// source name) does NOT reliably force a failure on THIS platform: measured live,
/// pointing ffmpeg's/`MonitorCapture`'s pulse client at an unrecognized source
/// name (with or without a `.monitor` suffix) silently falls back to the DEFAULT
/// source instead of erroring (PipeWire's pulse-compat behavior; a stricter
/// PulseAudio server may differ) — `pa_stream_connect_record` still succeeds. This
/// test instead uses the honest, documented env-guard alternative the ticket's own
/// mechanism choice allows for: `CCK_TEST_FORCE_OWNED_FAILURE`
/// (`test_force_owned_failure` in `pipewire.rs`), which short-circuits
/// `try_start_owned_audio` to `Err` directly — observably identical, from every
/// caller's perspective, to a genuine pre-flight failure.
///
/// A live PipeWire portal isn't available headlessly, so the FULL entry point
/// (which needs a real `fd`) can't be driven end-to-end here; what's proven
/// instead is the precise mechanism recording failure depends on, plus — by
/// construction, visible directly in `try_start_owned_audio`'s signature — that
/// the portal `fd` is never even a parameter of this check, so it cannot have
/// been touched regardless of the outcome.
#[test]
fn media_clock_owned_path_reports_an_actionable_error_on_forced_failure() {
    require_e2e_tools!("media_clock_owned_path_reports_an_actionable_error_on_forced_failure");
    let _ = env_logger::try_init();
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _guard = GlobalStateGuard;

    let mic_sink = NullSink::load("cck_e2e2_mic").expect("load the mic null sink");
    crate::audio::config::set_mic_source(&mic_sink.monitor_source());
    // SAFETY: `test_lock()` is held for this whole test.
    unsafe {
        std::env::set_var("CCK_TEST_FORCE_OWNED_FAILURE", "1");
    }

    let start = Instant::now();
    let result = super::owned::try_start_owned_audio();
    let elapsed = start.elapsed();
    eprintln!(
        "E2E-2: try_start_owned_audio() with CCK_TEST_FORCE_OWNED_FAILURE=1 -> {} (took {elapsed:?})",
        match &result {
            Ok(_) => "Ok (unexpected)".to_string(),
            Err(e) => format!("Err({e:?})"),
        }
    );
    match result {
        Ok(owned) => {
            owned.cleanup();
            panic!("the forced-failure seam must make the pre-flight check fail");
        }
        Err(reason) => {
            assert_eq!(
                reason, "forced failure (test seam)",
                "the reported reason must name what failed"
            );
        }
    }
    assert!(
        elapsed < Duration::from_secs(1),
        "a forced failure must fail IMMEDIATELY (it's checked before anything else \
         starts), not ride out any bound (took {elapsed:?})"
    );

    // Sanity check the SAME mechanism the other direction: with the seam OFF, the
    // pre-flight check must succeed against a real (if unconventional) source —
    // proving the `Err` above was really about the forced failure, not some
    // unrelated harness problem (e.g. a missing FIFO permission).
    // SAFETY: `test_lock()` is held for this whole test.
    unsafe {
        std::env::remove_var("CCK_TEST_FORCE_OWNED_FAILURE");
        std::env::set_var("CCK_TEST_MONITOR_SOURCE", mic_sink.monitor_source());
    }
    let good = super::owned::try_start_owned_audio();
    let good_came_up = good.is_ok();
    if let Ok(owned) = good {
        owned.cleanup();
    }
    assert!(good_came_up, "the SAME check must succeed against a real (if unconventional) source");
}

/// Decode `path`'s first audio stream to mono f32 @ 48kHz — the continuity E2E's
/// sample-level view (the RMS series is far too coarse to see placement seams).
fn audio_mono_f32(path: &std::path::Path) -> Option<Vec<f32>> {
    let out = crate::util::ffmpeg_command()
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "a:0", "-ac", "1", "-ar", "48000", "-f", "f32le", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let (chunks, _) = out.stdout.as_chunks::<4>();
    Some(chunks.iter().map(|b| f32::from_le_bytes(*b)).collect())
}

/// Count sample-level discontinuities in `[from_secs, to_secs)` of a decoded mono
/// 48kHz signal: adjacent-sample steps larger than `threshold`. A clean 300Hz tone
/// at 0.05 amplitude has a maximum legitimate step of 2π·300/48000 · 0.05 ≈ 0.002 —
/// a placement seam (silence-fill edge or truncation splice) steps up to the full
/// waveform range, orders of magnitude above it.
fn discontinuity_count(samples: &[f32], from_secs: f64, to_secs: f64, threshold: f32) -> usize {
    let lo = ((from_secs * 48_000.0) as usize).min(samples.len());
    let hi = ((to_secs * 48_000.0) as usize).min(samples.len());
    samples[lo..hi].windows(2).filter(|w| (w[1] - w[0]).abs() > threshold).count()
}

/// E2E-4 (audio continuity, the DRAGON-122-integration garbling regression): an 8s
/// unpaused, unmuted session capturing a bare continuous 300Hz tone. The tone must
/// come out sample-continuous: per-chunk wall-clock anchoring jitter (each 10-25ms
/// chunk independently placed at `arrival − duration`, so scheduler jitter turns
/// into ~40-100 micro-truncations/silence-fills per second) is audible as garbling
/// while every coarse assertion (durations, RMS edges, sync pairing) still passes.
/// The threshold (0.02) sits ~10× above the tone's own maximum step and far above
/// AAC's coding noise at this amplitude; the analysis window skips the first 1s
/// (capture spin-up / encoder priming) and the last 0.5s (finalize tail).
#[test]
fn media_clock_unpaused_tone_continuity_e2e() {
    require_e2e_tools!("media_clock_unpaused_tone_continuity_e2e");
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _guard = GlobalStateGuard;

    // No pause (pause_at=100 > total), no mute, no beeps: one continuous tone.
    let Some((out_path, timeline)) =
        run_owned_shaped_session("cck_e2e4_", 8.0, 100.0, 0.0, 100.0, 0.0, &[])
    else {
        panic!(
            "the owned-shaped session's pre-flight/session setup failed — this is the \
             harness itself, not the thing under test; check pactl/ffmpeg are reachable \
             and no stale cck_e2e4_* pactl modules are lingering"
        );
    };

    let expected_media = timeline.session_end.duration_since(timeline.t0).as_secs_f64();
    let samples = audio_mono_f32(&out_path).expect("decode the output audio to PCM");
    let dur = samples.len() as f64 / 48_000.0;
    assert!(
        (dur - expected_media).abs() < 0.3,
        "decoded audio length {dur:.3}s must be near the session length {expected_media:.3}s"
    );
    let seams = discontinuity_count(&samples, 1.0, dur - 0.5, 0.02);
    eprintln!(
        "E2E-4: {seams} sample discontinuities (>0.02 step) in [1.0,{:.1}]s of a pure tone",
        dur - 0.5
    );
    assert!(
        seams <= 2,
        "an unpaused tone capture must be sample-continuous — {seams} discontinuities \
         found (per-chunk wall-anchor jitter chops the stream ~40×/s; the contiguous \
         placement model keeps this at ~0)"
    );

    let _ = std::fs::remove_file(&out_path);
}

/// E2E-5 (early long pause — the pause-starvation shape): pause 1s in, for 6s, in a
/// 10s session. During a pause NOTHING is fed to ffmpeg on any of its three inputs
/// (video ticks are 0, the mixer's render is frozen), so this holds a barely-started
/// ffmpeg starved for longer than the muxer-liveness/watchdog budget (12s from spawn
/// covers pause end at ~7s… the point is the STARVATION itself plus the recording
/// surviving it end-to-end with correct media durations: 10s wall − 6s pause = 4s).
#[test]
fn media_clock_early_long_pause_e2e() {
    require_e2e_tools!("media_clock_early_long_pause_e2e");
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _guard = GlobalStateGuard;

    let beeps = [8.0, 9.0];
    let Some((out_path, timeline)) =
        run_owned_shaped_session("cck_e2e5_", 10.0, 1.0, 6.0, 100.0, 0.0, &beeps)
    else {
        panic!(
            "the owned-shaped session's pre-flight/session setup failed — this is the \
             harness itself, not the thing under test; check pactl/ffmpeg are reachable \
             and no stale cck_e2e5_* pactl modules are lingering"
        );
    };

    let mut expect = MediaClock::new(timeline.t0);
    expect.pause(timeline.pause_at);
    expect.resume(timeline.resume_at);
    let expected_media = expect.media_at(timeline.session_end);

    let vdur = probe_stream_duration(&out_path, "v:0").expect("ffprobe video duration");
    let adur = probe_stream_duration(&out_path, "a:0").expect("ffprobe audio duration");
    eprintln!("E2E-5: expected_media={expected_media:.3}s video={vdur:.3}s audio={adur:.3}s");
    assert!(
        (vdur - expected_media).abs() < 0.2,
        "VIDEO stream duration {vdur:.3}s must be within 0.2s of media {expected_media:.3}s"
    );
    assert!(
        (adur - expected_media).abs() < 0.2,
        "AUDIO stream duration {adur:.3}s must be within 0.2s of media {expected_media:.3}s"
    );

    let _ = std::fs::remove_file(&out_path);
}

/// ffprobe: one stream's duration (seconds).
fn probe_stream_duration(path: &std::path::Path, stream: &str) -> Option<f64> {
    let out = crate::util::ffprobe_command()
        .args(["-v", "error", "-select_streams", stream])
        .args(["-show_entries", "stream=duration", "-of", "csv=p=0"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// E2E-3 (the field failure's shape, DRAGON-125): pause ~2s with NO mute toggle in
/// a 10s session — the exact user scenario whose recording shipped with video on
/// media time (pause excluded, 8.13s) but audio on WALL time (pause included,
/// 10s). Asserts the per-stream durations agree with the expected media length —
/// the assertion class E2E-1 historically lacked (it checked only the container
/// duration, which is just the LONGEST stream) — plus packet counts and PTS
/// continuity. The harness's teardown assertions (prompt ffmpeg exit after EOF
/// with a live mid-session child — see `run_owned_shaped_session`) cover the
/// second half of the field failure, the leaked-fd stop wedge.
#[test]
fn media_clock_user_shape_pause_no_mute_e2e() {
    require_e2e_tools!("media_clock_user_shape_pause_no_mute_e2e");
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _guard = GlobalStateGuard;

    let beeps = [1.0, 2.0, 3.0, 7.0, 8.0, 9.0];
    // Total 10s, pause 4..6 (2s); mute_at=100 => no toggle ever fires (the field
    // session had none), so the whole session is one ungated stretch.
    let Some((out_path, timeline)) =
        run_owned_shaped_session("cck_e2e3_", 10.0, 4.0, 2.0, 100.0, 0.0, &beeps)
    else {
        panic!(
            "the owned-shaped session's pre-flight/session setup failed — this is the \
             harness itself, not the thing under test; check pactl/ffmpeg are reachable \
             and no stale cck_e2e3_* pactl modules are lingering"
        );
    };

    let mut expect = MediaClock::new(timeline.t0);
    expect.pause(timeline.pause_at);
    expect.resume(timeline.resume_at);
    let expected_final_media = expect.media_at(timeline.session_end);

    let vdur = probe_stream_duration(&out_path, "v:0").expect("ffprobe video duration");
    let adur = probe_stream_duration(&out_path, "a:0").expect("ffprobe audio duration");
    let packets = probe_packet_count(&out_path, "v:0").expect("ffprobe video packet count");
    let fdur = probe_duration(&out_path).expect("ffprobe container duration");
    eprintln!(
        "E2E-3: expected_media={expected_final_media:.3}s video={vdur:.3}s audio={adur:.3}s \
         container={fdur:.3}s packets={packets}"
    );
    assert!(
        (vdur - expected_final_media).abs() < 0.2,
        "VIDEO stream duration {vdur:.3}s must be within 0.2s of media {expected_final_media:.3}s"
    );
    assert!(
        (adur - expected_final_media).abs() < 0.2,
        "AUDIO stream duration {adur:.3}s must be within 0.2s of media {expected_final_media:.3}s \
         — the field file carried WALL-length audio (~media + the 2s pause)"
    );
    assert!(
        (adur - vdur).abs() < 0.2,
        "audio ({adur:.3}s) and video ({vdur:.3}s) must agree — they diverged by the pause \
         length in the field failure"
    );
    assert!(
        (fdur - expected_final_media).abs() < 0.2,
        "container duration {fdur:.3}s must match the media length {expected_final_media:.3}s"
    );
    let expected_packets = (expected_final_media * FPS as f64).round() as i64;
    assert!(
        (packets as i64 - expected_packets).abs() <= 2,
        "video packet count {packets} must be within 2 of fps*media = {expected_packets}"
    );
    let pts = probe_video_pts(&out_path);
    assert!(pts.windows(2).all(|w| w[1] > w[0]), "video PTS must be strictly increasing");

    let _ = std::fs::remove_file(&out_path);
}


// ===========================================================================
// Content-identified sessions (DRAGON-417): markers that carry their own
// capture time, in BOTH streams
// ===========================================================================
//
// Everything above this line measures a recording by its SHAPE — durations, packet
// counts, edge positions, RMS envelopes. That whole class of assertion passed while
// recordings were silently throwing away their opening seconds, and passed again
// while pause was (wrongly) suspected of eating content: a waveform cannot tell
// "paused" from "nobody was talking", and a duration cannot tell "8 seconds of the
// right content" from "8 seconds starting 6 seconds late". The only way to answer
// "is the content that was in front of the recorder at wall time T actually in the
// file at media position m?" is to record content that IDENTIFIES ITS OWN CAPTURE
// TIME, and decode it back out.
//
// Two markers, one per stream, both self-timestamping:
//
// - AUDIO: a linear chirp played into the captured sink. Its instantaneous FREQUENCY
//   is its own source time ([`CHIRP_F0`] + [`CHIRP_RATE`]·t), recovered per window by
//   a Goertzel scan. Frequency, not amplitude — a silent stretch is not mistaken for
//   anything, it simply yields no peak.
// - VIDEO: each generated frame is a flat grey whose LEVEL encodes the wall instant
//   the frame was written ([`video_level`]). A still test pattern is what made a
//   frozen video indistinguishable from a live one; this one cannot be.
//
// Both decode back to THIS process's clock (the chirp through a measured onset —
// see [`calibrate_chirp_onset`]), so every assertion below is absolute, not a fit.

/// The marker chirp's start frequency (Hz) at its own source time 0.
const CHIRP_F0: f64 = 300.0;
/// The marker chirp's sweep rate (Hz per second of source time). Chosen so a session
/// of tens of seconds stays well inside the AAC-preserved band while one 2048-sample
/// analysis window (~43ms) covers only ~17Hz of sweep — narrower than the scan's own
/// resolution, so the peak stays sharp.
const CHIRP_RATE: f64 = 400.0;

/// Plays the marker chirp into `sink` until dropped. `sin(2π(F0·t + (RATE/2)·t²))`
/// has instantaneous frequency `F0 + RATE·t`, i.e. the source time is readable
/// straight off the spectrum.
struct ChirpPlayer(Child);
impl ChirpPlayer {
    fn start(sink: &str, duration_secs: f64) -> Option<Self> {
        let expr = format!(
            "0.6*sin(2*PI*({CHIRP_F0}*t+{}*t*t))",
            CHIRP_RATE / 2.0
        );
        let child = crate::util::ffmpeg_command()
            .args(["-hide_banner", "-loglevel", "error"])
            .args(["-f", "lavfi", "-i", &format!("aevalsrc='{expr}':s=48000:d={duration_secs}")])
            .args(["-f", "pulse", "-device", sink, "cck-e2e-chirp"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .ok()?;
        Some(Self(child))
    }
}
impl Drop for ChirpPlayer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Goertzel power of `x` (Hann-windowed) at `f`, sample rate `sr`.
fn goertzel_power(x: &[f32], hann: &[f64], f: f64, sr: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * f / sr;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for (i, &v) in x.iter().enumerate() {
        let s0 = coeff * s1 - s2 + v as f64 * hann[i];
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

/// The number of samples one chirp analysis window spans (~43ms at 48kHz).
const CHIRP_WIN: usize = 2048;

/// Recover the marker chirp's own SOURCE time from the recorded audio around media
/// position `at_media`. `None` when the window falls outside the decoded signal or
/// carries no dominant tone (silence, or content that isn't the chirp) — never a
/// guess. The candidate scan runs to `max_source_secs` of sweep plus margin.
fn chirp_source_time(samples: &[f32], at_media: f64, max_source_secs: f64) -> Option<f64> {
    const SR: f64 = 48_000.0;
    let center = (at_media * SR) as i64;
    let lo = center - (CHIRP_WIN as i64) / 2;
    if lo < 0 || (lo as usize) + CHIRP_WIN > samples.len() {
        return None;
    }
    let win = &samples[lo as usize..lo as usize + CHIRP_WIN];
    let hann: Vec<f64> = (0..CHIRP_WIN)
        .map(|i| {
            0.5 - 0.5
                * (2.0 * std::f64::consts::PI * i as f64 / (CHIRP_WIN - 1) as f64).cos()
        })
        .collect();
    let step = 20.0;
    let f_lo = CHIRP_F0 - 100.0;
    let f_hi = CHIRP_F0 + CHIRP_RATE * max_source_secs + 200.0;
    let mut powers: Vec<(f64, f64)> = Vec::new();
    let mut f = f_lo;
    while f <= f_hi {
        powers.push((f, goertzel_power(win, &hann, f, SR)));
        f += step;
    }
    let (best_i, &(best_f, best_p)) = powers
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.1.total_cmp(&b.1.1))?;
    // A real tone towers over the scan's median bin; silence or noise does not.
    let mut sorted: Vec<f64> = powers.iter().map(|p| p.1).collect();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    if best_p < median * 50.0 {
        return None;
    }
    // Parabolic refinement across the peak's neighbours (sub-bin frequency, so the
    // recovered source time beats the 20Hz scan step).
    let refined = if best_i > 0 && best_i + 1 < powers.len() {
        let (l, c, r) = (powers[best_i - 1].1, best_p, powers[best_i + 1].1);
        let denom = l - 2.0 * c + r;
        if denom.abs() > f64::EPSILON {
            best_f + step * 0.5 * (l - r) / denom
        } else {
            best_f
        }
    } else {
        best_f
    };
    Some((refined - CHIRP_F0) / CHIRP_RATE)
}

/// The wall instant at which the marker chirp's own source time 0 reached the sink —
/// MEASURED, because every absolute audio assertion needs the chirp's clock tied to
/// this process's, and an ffmpeg spawn + pulse connect is worth a few hundred
/// unpredictable milliseconds. Listens on `monitor_source` with the app's own
/// [`MonitorCapture`] and takes the first chunk carrying signal.
///
/// This is the ONE place amplitude is used, and legitimately: the chirp is a signal
/// this test fully controls, silent before its source 0 and at full level from it, so
/// "first chunk above the floor" is unambiguous. Nothing downstream infers content
/// from level.
fn calibrate_chirp_onset(
    monitor_source: &str,
    sink: &str,
    duration_secs: f64,
) -> Option<(Instant, ChirpPlayer)> {
    let (monitor, rx) = crate::audio::capture::MonitorCapture::start(Some(monitor_source.to_string()), None)?;
    // Let the capture settle so the onset isn't attributed to its own spin-up.
    std::thread::sleep(Duration::from_millis(400));
    while rx.try_recv().is_ok() {}
    let player = ChirpPlayer::start(sink, duration_secs)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut onset = None;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                let peak = chunk.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                if peak > 0.05 {
                    onset = Some(chunk.capture_wall);
                    break;
                }
            }
            Err(_) => continue,
        }
    }
    let _ = monitor.stop();
    onset.map(|t| (t, player))
}

/// Grey level encoding `dt` (seconds since the session's media 0) into a generated
/// video frame. Linear and monotone across a session of tens of seconds, with enough
/// spacing per step that h264's intra coding of a flat field can't confuse two.
fn video_level(dt_secs: f64) -> u8 {
    (24.0 + dt_secs * 15.0).clamp(24.0, 250.0) as u8
}

/// A flat RGBA frame at `level` — the video marker's carrier.
fn frame_level(level: u8, w: u32, h: u32) -> Vec<u8> {
    let mut f = vec![level; (w * h * 4) as usize];
    for px in f.as_chunks_mut::<4>().0 {
        px[3] = 0xFF;
    }
    f
}

/// One decoded byte per output frame: the frame's mean luma (the frames are flat, so
/// this IS the marker level, up to the encoder's range conversion — which the two
/// anchors in [`MarkedSession::frame_wall`] calibrate out exactly).
fn video_frame_levels(path: &std::path::Path) -> Option<Vec<u8>> {
    let out = crate::util::ffmpeg_command()
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "v:0", "-vf", "scale=1:1:flags=area", "-pix_fmt", "gray"])
        .args(["-f", "rawvideo", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some(out.stdout)
}

/// One content-identified session's recording plus everything needed to decode its
/// markers back onto THIS process's clock.
struct MarkedSession {
    path: std::path::PathBuf,
    /// Media time 0: the instant the audio pre-flight started, which is also the
    /// instant a real app marks itself "recording" (see `OwnedAudioStart::capture_start`).
    capture_start: Instant,
    /// Wall instant of the marker chirp's own source time 0 (measured).
    chirp_zero_at: Instant,
    /// `(output frame index, wall instant)` of every main-loop video write batch —
    /// exactly-known points that calibrate the decoded grey level back to wall
    /// seconds, whatever range conversion the encoder applied. The first and the last
    /// one still present in the finished file are the two anchors used.
    writes: Vec<(usize, Instant)>,
    /// Measured `(pause, resume)` instants, in order.
    pauses: Vec<(Instant, Instant)>,
    session_end: Instant,
    mic_stats: crate::mixer::track::MixerStats,
    sys_stats: crate::mixer::track::MixerStats,
    final_media: f64,
    levels: Vec<u8>,
    audio: Vec<f32>,
}

impl MarkedSession {
    /// Seconds since [`capture_start`](Self::capture_start).
    fn since_start(&self, t: Instant) -> f64 {
        t.saturating_duration_since(self.capture_start).as_secs_f64()
    }

    /// The WALL position (seconds since media 0) a given MEDIA position corresponds
    /// to: media time plus every pause that has already been frozen out by then.
    fn wall_at_media(&self, media: f64) -> f64 {
        let mut wall = media;
        for (p, r) in &self.pauses {
            let p_media = self.since_start(*p) - (wall - media);
            if p_media <= media {
                wall += r.saturating_duration_since(*p).as_secs_f64();
            }
        }
        wall
    }

    /// The opening span: media 0 up to here is covered by copies of the FIRST frame
    /// the worker ever captured (audio was already being recorded; video wasn't ready
    /// yet), so a frame in this span identifies the video side's ready instant, not
    /// its own media position.
    fn opening_span(&self) -> f64 {
        self.since_start(self.writes[0].1)
    }

    /// The two calibration points: the first video write, and the last one that
    /// actually survived into the file.
    fn anchors(&self) -> Option<((usize, Instant), (usize, Instant))> {
        let a = *self.writes.first()?;
        let b = *self.writes.iter().rev().find(|(k, _)| *k + 1 < self.levels.len())?;
        (b.0 > a.0).then_some((a, b))
    }

    /// Wall position (seconds since media 0) that output video frame `k` was
    /// captured at, decoded from the frame's own marker level.
    fn frame_wall(&self, k: usize) -> Option<f64> {
        let ((ka, ta), (kb, tb)) = self.anchors()?;
        let (la, lb) = (*self.levels.get(ka)? as f64, *self.levels.get(kb)? as f64);
        if (lb - la).abs() < 1.0 {
            return None;
        }
        let (wa, wb) = (self.since_start(ta), self.since_start(tb));
        let l = *self.levels.get(k)? as f64;
        Some(wa + (l - la) * (wb - wa) / (lb - la))
    }

    /// Wall position (seconds since media 0) of the AUDIO sitting at media position
    /// `media`, decoded from the chirp's own frequency.
    fn audio_wall(&self, media: f64, max_source: f64) -> Option<f64> {
        let s = chirp_source_time(&self.audio, media, max_source)?;
        Some(s + self.since_start(self.chirp_zero_at))
    }
}

/// Drive one owned-shaped session whose BOTH streams carry self-timestamping content
/// (see this section's header). `startup_delay_secs` is an artificial stall between
/// the audio pre-flight and the moment the video side starts feeding — the real
/// worker's own startup (capture handshake, first frame, ffmpeg spawn, and on the
/// portal path a bounded wait for the stream's first frame) made visible and
/// controllable, because that span is exactly what DRAGON-417 threw away.
/// `pauses` are `(offset from the video loop's start, length)` in real seconds.
fn run_marked_session(
    prefix: &str,
    total_secs: f64,
    startup_delay_secs: f64,
    pauses: &[(f64, f64)],
) -> Option<MarkedSession> {
    let mic_sink = NullSink::load(&format!("{prefix}mic"))?;
    let sys_sink = NullSink::load(&format!("{prefix}sys"))?;
    crate::audio::config::set_mic_source(&mic_sink.monitor_source());
    // SAFETY: the caller holds `test_lock()` for this whole call.
    unsafe {
        std::env::set_var("CCK_TEST_MONITOR_SOURCE", sys_sink.monitor_source());
    }
    let chirp_len = total_secs + startup_delay_secs + 20.0;
    let (chirp_zero_at, _chirp) =
        calibrate_chirp_onset(&sys_sink.monitor_source(), &sys_sink.name, chirp_len)?;

    // === the instant a real app marks itself "recording" ===
    let owned = super::owned::try_start_owned_audio().ok()?;
    let super::owned::OwnedAudioStart {
        capture_start, mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx,
    } = owned;

    let out_path = std::env::temp_dir().join(format!("cck-e2e-{prefix}{}.mp4", std::process::id()));
    let temp_path = super::recording_temp_path(&out_path);
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&temp_path);

    let presets = crate::encode::Presets::default();
    let plan = crate::encode::EncodePlan::resolve("software", W, H, &presets);
    let Ok(mut child) = crate::encode::spawn_ffmpeg_media_clock(
        W, H, W, H, FPS, &plan, 4000, &temp_path, &mic_fifo_path, &sys_fifo_path,
    ) else {
        drop(mic_tap);
        let _ = monitor.stop();
        let _ = std::fs::remove_file(&mic_fifo_path);
        let _ = std::fs::remove_file(&sys_fifo_path);
        return None;
    };
    let mut stdin = child.stdin.take().expect("piped stdin");

    // The worker's video-side startup, made explicit.
    let already = capture_start.elapsed().as_secs_f64();
    if startup_delay_secs > already {
        std::thread::sleep(Duration::from_secs_f64(startup_delay_secs - already));
    }

    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let events: Mutex<Vec<ToggleEvent>> = Mutex::new(Vec::new());
    let cfg = PumpConfig {
        fps: FPS,
        audio_offset_ms: 0,
        auto_device_compensation: false,
        mic_on0: true,
        sys_on0: true,
        duck_system: false,
    };

    let mut measured_pauses: Vec<(Instant, Instant)> = Vec::new();
    let mut writes: Vec<(usize, Instant)> = Vec::new();
    let mut session_end = capture_start;
    let mut stats: Option<(crate::mixer::track::MixerStats, crate::mixer::track::MixerStats, f64)> =
        None;

    let finished = std::thread::scope(|scope| {
        let (pump_handle, mut ticker) = super::pump::spawn(
            scope, capture_start, cfg, mic_fifo_path.clone(), sys_fifo_path.clone(), mic_tap,
            mic_rx, monitor, sys_rx, &stop, &paused, &events,
        )
        .expect("pump spawn must succeed in the E2E harness");

        let loop_t0 = Instant::now();
        let mut written: usize = 0;
        let mut pause_state: Vec<(bool, bool)> = vec![(false, false); pauses.len()];
        let mut last_frame = frame_level(video_level(0.0), W, H);
        loop {
            let elapsed = loop_t0.elapsed().as_secs_f64();
            for (i, &(at, len)) in pauses.iter().enumerate() {
                if elapsed >= at && !pause_state[i].0 {
                    paused.store(true, Ordering::Relaxed);
                    measured_pauses.push((Instant::now(), Instant::now()));
                    pause_state[i].0 = true;
                }
                if elapsed >= at + len && !pause_state[i].1 {
                    paused.store(false, Ordering::Relaxed);
                    if let Some(last) = measured_pauses.last_mut() {
                        last.1 = Instant::now();
                    }
                    pause_state[i].1 = true;
                }
            }
            if elapsed >= total_secs {
                break;
            }
            let now = Instant::now();
            let due = ticker.due_video_ticks(now);
            if due > 0 {
                last_frame = frame_level(video_level(capture_start.elapsed().as_secs_f64()), W, H);
                writes.push((written, now));
                for _ in 0..due {
                    let _ = stdin.write_all(&last_frame);
                    written += 1;
                }
            }
            std::thread::sleep(Duration::from_millis(8));
        }

        stop.store(true, Ordering::Relaxed);
        session_end = Instant::now();
        let pump_out = pump_handle.join();
        stats = Some((pump_out.mic_stats, pump_out.sys_stats, pump_out.final_media));
        // The stop tail re-feeds the LAST frame (marker unchanged), exactly like the
        // production workers do — so the anchors stay meaningful.
        for _ in 0..ticker.ticks_to_cover(pump_out.final_media) {
            let _ = stdin.write_all(&last_frame);
        }
        drop(stdin);
        let reaped = super::wait_or_kill(&mut child, Duration::from_secs(30));
        assert!(
            matches!(&reaped, Ok(s) if s.success()),
            "capture ffmpeg must exit cleanly after its inputs close (got {reaped:?})"
        );
        super::finalize::finalize_with_intervals(
            &temp_path,
            &out_path,
            &pump_out.mic_off,
            &pump_out.sys_off,
            plan.is_hevc(),
            "cck-e2e-marked",
        )
        .ok()
    })?;
    let _ = std::fs::remove_file(&temp_path);

    let (mic_stats, sys_stats, final_media) = stats?;
    let levels = video_frame_levels(&finished)?;
    let audio = audio_mono_f32(&finished)?;
    Some(MarkedSession {
        path: finished,
        capture_start,
        chirp_zero_at,
        writes,
        pauses: measured_pauses,
        session_end,
        mic_stats,
        sys_stats,
        final_media,
        levels,
        audio,
    })
}

/// E2E-6 (DRAGON-417 — the opening of a recording survives a slow start): the app
/// marks itself "recording" the instant it spawns the capture worker, and the worker
/// starts capturing audio immediately; its VIDEO side then takes a while to come up
/// (capture handshake, first frame, ffmpeg spawn — and on the portal path a bounded
/// wait for the stream's first frame). This session makes that span 1.5s and asserts
/// that everything audible during it is in the file, at the right place.
///
/// The bug it pins: media time 0 used to be stamped when the VIDEO side was ready, so
/// every audio chunk captured before that landed at a negative media position and was
/// discarded — silently, with no error and nothing in any log. A user speaking the
/// moment the indicator went live lost the opening of their take and could only find
/// out by listening back. Measured in the field as ~6 seconds of a 10-second count
/// missing, with the file itself 6 seconds short.
///
/// Every assertion here is on decoded CONTENT (the chirp's own frequency, the frames'
/// own marker levels) or on the mixer's own discard counters — never on duration
/// alone, which is exactly the class of check that passed throughout this bug's life.
#[test]
fn media_clock_recording_opening_survives_a_slow_start_e2e() {
    require_e2e_tools!("media_clock_recording_opening_survives_a_slow_start_e2e");
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _guard = GlobalStateGuard;

    const STARTUP: f64 = 1.5;
    const TOTAL: f64 = 8.0;
    let Some(s) = run_marked_session("cck_e2e6_", TOTAL, STARTUP, &[]) else {
        panic!(
            "the marked session's pre-flight/chirp calibration failed — this is the \
             harness itself, not the thing under test; check pactl/ffmpeg are reachable \
             and no stale cck_e2e6_* pactl modules are lingering"
        );
    };
    let recorded_for = s.since_start(s.session_end);
    eprintln!(
        "E2E-6: app was 'recording' for {recorded_for:.3}s (video side ready {STARTUP:.1}s in); \
         media={:.3}s mic(late={} gap={}) sys(late={} gap={})",
        s.final_media, s.mic_stats.late_chunks, s.mic_stats.gap_samples,
        s.sys_stats.late_chunks, s.sys_stats.gap_samples,
    );

    // ---- Nothing captured after the app went live was thrown away ----
    assert_eq!(
        s.mic_stats.late_chunks, 0,
        "no mic chunk may be discarded as late: every one of them was captured after \
         the app said it was recording (this counter was 28 on the broken build)"
    );
    assert_eq!(
        s.sys_stats.late_chunks, 0,
        "no system chunk may be discarded as late (13 on the broken build)"
    );

    // ---- The recording is as long as the app claimed to be recording ----
    let adur = probe_stream_duration(&s.path, "a:0").expect("ffprobe audio duration");
    assert!(
        (adur - recorded_for).abs() < 0.35,
        "audio stream {adur:.3}s must cover the whole time the app was recording \
         ({recorded_for:.3}s) — the broken build came out short by the startup span"
    );

    // ---- AUDIO CONTENT: media m carries the sound that was playing at wall m ----
    // The chirp's own frequency says which instant of the source each window is; the
    // calibrated onset ties that to this process's clock. On the broken build every
    // one of these reads STARTUP seconds late.
    let max_source = recorded_for + STARTUP + 10.0;
    let mut checked = 0;
    let mut worst: f64 = 0.0;
    let mut m = 0.4;
    while m < recorded_for - 0.6 {
        let w = s
            .audio_wall(m, max_source)
            .unwrap_or_else(|| panic!("no chirp tone at media {m:.2}s — audio is missing there"));
        worst = worst.max((w - m).abs());
        assert!(
            (w - m).abs() < 0.35,
            "the audio at media {m:.2}s was captured at wall {w:.2}s; it must be the \
             sound that was playing at wall {m:.2}s. A shift of ~{STARTUP:.1}s here IS \
             the bug: the opening was dropped and everything else slid down to fill it."
        );
        checked += 1;
        m += 0.4;
    }
    assert!(checked >= 10, "expected a decent number of audio sample points, got {checked}");
    eprintln!("E2E-6: {checked} audio points, worst wall error {worst:.3}s");

    // ---- The very start of the file is real captured audio, not a hole ----
    let earliest = {
        let mut t = 0.05;
        loop {
            if t > 1.2 {
                panic!("no chirp tone anywhere in the first 1.2s — the opening is empty");
            }
            if s.audio_wall(t, max_source).is_some() {
                break t;
            }
            t += 0.05;
        }
    };
    eprintln!("E2E-6: audio first decodable at media {earliest:.2}s");
    assert!(
        earliest < 0.45,
        "the recording must start with captured sound, not silence: first decodable \
         audio at media {earliest:.2}s (only the capture's own spin-up may be missing)"
    );

    // ---- VIDEO CONTENT: the opening span is the first frame held, then live ----
    let opening_frames = (STARTUP * FPS as f64) as usize;
    for k in [0usize, opening_frames / 3, opening_frames.saturating_sub(3)] {
        let w = s.frame_wall(k).expect("decode frame marker");
        assert!(
            (w - STARTUP).abs() < 0.25,
            "video frame {k} (media {:.2}s) must still be the first frame the worker \
             ever captured (wall ~{STARTUP:.2}s), not a later one: decoded {w:.2}s",
            k as f64 / FPS as f64
        );
    }
    let mut k = opening_frames + FPS as usize / 2;
    while k + 2 < s.levels.len() {
        let media = k as f64 / FPS as f64;
        let w = s.frame_wall(k).expect("decode frame marker");
        assert!(
            (w - media).abs() < 0.25,
            "past the opening span, video frame {k} (media {media:.2}s) must be the \
             frame captured at wall {media:.2}s; decoded {w:.2}s"
        );
        k += FPS as usize;
    }

    let _ = std::fs::remove_file(&s.path);
}

/// Locate a jump of at least `min_jump` in a `(media, wall)` series — the media
/// position where the recorded content skips forward, i.e. a seam. Returns the
/// midpoint of the largest such step, or `None` if the content runs continuously.
fn seam_position(series: &[(f64, f64)], min_jump: f64) -> Option<f64> {
    let mut best: Option<(f64, f64)> = None; // (jump, media midpoint)
    for w in series.windows(2) {
        let jump = w[1].1 - w[0].1;
        if jump >= min_jump && best.is_none_or(|(b, _)| jump > b) {
            best = Some((jump, (w[0].0 + w[1].0) / 2.0));
        }
    }
    best.map(|(_, m)| m)
}

/// E2E-7 (pause cuts BOTH streams, at the SAME place, every time): three pause/resume
/// cycles in one session, verified by decoding what each stream actually contains.
///
/// What the older pause tests could not establish, and why it mattered: they assert
/// pause ARITHMETIC (durations and packet counts against a replayed `MediaClock`) on a
/// STATIC test pattern. On a still picture a frozen video and a live one are
/// byte-identical, so "video honoured the pause" was never actually observed — and an
/// energy view of the audio cannot tell a pause from a quiet moment either. When pause
/// was accused of eating a recording, nothing in the suite could clear it; that took
/// hand-transcribing a real take. This test replaces both blind spots with content:
/// every frame's grey level says which wall instant it was captured at, and the audio
/// chirp's frequency says the same for the sound.
///
/// It asserts, per stream: no content from inside any pause survived; the content that
/// did survive sits at the media position the media clock says it should; and the seam
/// in the audio lands at the same media position as the seam in the video, for every
/// cycle — a freeze that took hold slightly earlier in one stream than the other would
/// desync everything downstream and no per-stream test could see it.
#[test]
fn media_clock_pause_jump_cuts_both_streams_e2e() {
    require_e2e_tools!("media_clock_pause_jump_cuts_both_streams_e2e");
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _guard = GlobalStateGuard;

    const TOTAL: f64 = 13.0;
    let schedule = [(2.0, 1.8), (6.0, 2.2), (10.0, 1.6)];
    let Some(s) = run_marked_session("cck_e2e7_", TOTAL, 0.4, &schedule) else {
        panic!(
            "the marked session's pre-flight/chirp calibration failed — this is the \
             harness itself, not the thing under test; check pactl/ffmpeg are reachable \
             and no stale cck_e2e7_* pactl modules are lingering"
        );
    };
    assert_eq!(s.pauses.len(), 3, "all three pause cycles must have been driven");

    // Expected media position of each pause, from a clock fed the SAME history.
    let mut expect = MediaClock::new(s.capture_start);
    for (p, r) in &s.pauses {
        expect.pause(*p);
        expect.resume(*r);
    }
    let seams: Vec<f64> = s.pauses.iter().map(|(p, _)| expect.media_at(*p)).collect();
    let paused_walls: Vec<(f64, f64)> =
        s.pauses.iter().map(|(p, r)| (s.since_start(*p), s.since_start(*r))).collect();
    let recorded_media = expect.media_at(s.session_end);
    eprintln!(
        "E2E-7: media={:.3}s (expected {recorded_media:.3}s); pauses at media {seams:?}; \
         paused walls {paused_walls:?}",
        s.final_media
    );

    let max_source = s.since_start(s.session_end) + 15.0;
    let inside_a_pause = |w: f64, margin: f64| {
        paused_walls.iter().any(|&(p, r)| w > p + margin && w < r - margin)
    };

    // ---- VIDEO: no frame from inside a pause; every frame where the clock says ----
    let mut video_series: Vec<(f64, f64)> = Vec::new();
    // Skip the opening span (see `opening_span`): those frames are copies of the first
    // captured frame by design, not evidence about pause.
    let from_frame = ((s.opening_span() + 0.15) * FPS as f64).ceil() as usize;
    for k in from_frame..s.levels.len() {
        let media = k as f64 / FPS as f64;
        let Some(w) = s.frame_wall(k) else { continue };
        video_series.push((media, w));
        assert!(
            !inside_a_pause(w, 0.12),
            "video frame {k} (media {media:.2}s) was captured at wall {w:.2}s, which is \
             INSIDE a pause — paused time must not be recorded"
        );
        // The frame slot that STRADDLES a seam legitimately carries post-resume
        // content (its slot spans the freeze), so it is not evidence either way.
        if seams.iter().any(|&c| (media - c).abs() < 2.0 / FPS as f64) {
            continue;
        }
        assert!(
            (w - s.wall_at_media(media)).abs() < 0.30,
            "video frame {k} at media {media:.2}s should be the frame captured at wall \
             {:.2}s; decoded {w:.2}s",
            s.wall_at_media(media)
        );
    }
    assert!(video_series.len() > 100, "expected a full video track, got {} frames", video_series.len());

    // ---- AUDIO: same two questions, asked of the chirp ----
    let mut audio_series: Vec<(f64, f64)> = Vec::new();
    let mut m = 0.3;
    while m < s.final_media - 0.3 {
        if let Some(w) = s.audio_wall(m, max_source) {
            audio_series.push((m, w));
            assert!(
                seams.iter().any(|&c| (m - c).abs() < 0.1) || !inside_a_pause(w, 0.25),
                "the audio at media {m:.2}s was captured at wall {w:.2}s, INSIDE a pause"
            );
            // An analysis window straddling a seam holds both sides at once; its
            // reading is meaningless, and the seam checks below cover that boundary.
            if !seams.iter().any(|&c| (m - c).abs() < 0.1) {
                assert!(
                    (w - s.wall_at_media(m)).abs() < 0.40,
                    "the audio at media {m:.2}s should be the sound playing at wall \
                     {:.2}s; decoded {w:.2}s",
                    s.wall_at_media(m)
                );
            }
        }
        m += 0.05;
    }
    assert!(audio_series.len() > 100, "expected a decodable audio track, got {} points", audio_series.len());

    // ---- The two streams cut at the SAME media position, on every cycle ----
    for (i, &seam) in seams.iter().enumerate() {
        let len = paused_walls[i].1 - paused_walls[i].0;
        let near = |v: &[(f64, f64)]| -> Vec<(f64, f64)> {
            v.iter().copied().filter(|(m, _)| (*m - seam).abs() < 0.8).collect()
        };
        let vseam = seam_position(&near(&video_series), len * 0.5)
            .unwrap_or_else(|| panic!("no video seam near media {seam:.2}s (cycle {i})"));
        let aseam = seam_position(&near(&audio_series), len * 0.5)
            .unwrap_or_else(|| panic!("no audio seam near media {seam:.2}s (cycle {i})"));
        eprintln!(
            "E2E-7: cycle {i}: expected seam at media {seam:.3}s — video {vseam:.3}s, \
             audio {aseam:.3}s (pause was {len:.2}s)"
        );
        assert!(
            (vseam - seam).abs() < 0.25,
            "cycle {i}: the video's jump cut must land at media {seam:.3}s, not {vseam:.3}s"
        );
        assert!(
            (aseam - seam).abs() < 0.30,
            "cycle {i}: the audio's jump cut must land at media {seam:.3}s, not {aseam:.3}s"
        );
        assert!(
            (vseam - aseam).abs() < 0.25,
            "cycle {i}: audio and video must cut at the SAME media position (video \
             {vseam:.3}s vs audio {aseam:.3}s) — a stream that freezes early or late \
             desyncs everything after it"
        );
    }

    let _ = std::fs::remove_file(&s.path);
}

/// E2E-8 (DRAGON-554 — the opening prime reaches the muxer's side of both FIFOs
/// promptly): ffmpeg 7.x will not finish OPENING an f32le FIFO input until it has read
/// [`super::pump::FFMPEG7_INPUT_OPEN_BYTES`] from it (measured against the Flatpak
/// runtime's 7.1.3; see the constant's doc), and while an input hangs in open ffmpeg
/// reads no video from stdin either — so if the FIRST FIFO byte only exists once the
/// render horizon reaches media 0 (wall ~1.5s), every ffmpeg-7.x recording opens on
/// ~1.1s of frozen, then jumping, video (the DRAGON-554 evidence file). The pump's
/// opening prime renders that many bytes right after its startup catch-up drain
/// instead.
///
/// This test stands in for ffmpeg on the READ side of both FIFOs (no ffmpeg needed for
/// the property under test) and measures the wall time from `pump::spawn` until each
/// FIFO has delivered the ffmpeg-7.x open threshold. Without the prime the mic side
/// measures ~1.5s+ (the render horizon); with it, well under a second — the bound
/// asserted here splits those cleanly with CI slack.
#[test]
fn media_clock_opening_prime_reaches_the_muxer_promptly_e2e() {
    require_e2e_tools!("media_clock_opening_prime_reaches_the_muxer_promptly_e2e");
    let _ = env_logger::try_init();
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _guard = GlobalStateGuard;

    let mic_sink = NullSink::load("cck_e2e8_mic").expect("load the mic null sink");
    let sys_sink = NullSink::load("cck_e2e8_sys").expect("load the sys null sink");
    crate::audio::config::set_mic_source(&mic_sink.monitor_source());
    // SAFETY: `test_lock()` is held for this whole test.
    unsafe {
        std::env::set_var("CCK_TEST_MONITOR_SOURCE", sys_sink.monitor_source());
    }

    let owned = super::owned::try_start_owned_audio().expect("the pre-flight must come up");
    let super::owned::OwnedAudioStart {
        capture_start: _, mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx,
    } = owned;

    // The ffmpeg stand-ins: open each FIFO's read end, record how long the pump takes
    // to deliver the 7.x open threshold, then keep draining so the session stays
    // healthy until the test ends it.
    let spawn_reader = |path: std::path::PathBuf,
                        slot: Arc<Mutex<Option<Duration>>>,
                        spawned_at: Instant,
                        stop: Arc<AtomicBool>| {
        std::thread::spawn(move || {
            use std::io::Read;
            let Ok(mut f) = std::fs::File::open(&path) else { return };
            let mut got = 0usize;
            let mut buf = [0u8; 8192];
            loop {
                match f.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        got += n;
                        if got >= super::pump::FFMPEG7_INPUT_OPEN_BYTES
                            && let Ok(mut g) = slot.lock()
                        {
                            g.get_or_insert(spawned_at.elapsed());
                        }
                    }
                }
                if stop.load(Ordering::Relaxed) && got >= super::pump::FFMPEG7_INPUT_OPEN_BYTES {
                    break;
                }
            }
        })
    };

    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let events: Mutex<Vec<ToggleEvent>> = Mutex::new(Vec::new());
    let cfg = PumpConfig {
        fps: FPS,
        audio_offset_ms: 0,
        auto_device_compensation: false,
        mic_on0: true,
        sys_on0: true,
        duck_system: false,
    };
    let mic_time: Arc<Mutex<Option<Duration>>> = Arc::new(Mutex::new(None));
    let sys_time: Arc<Mutex<Option<Duration>>> = Arc::new(Mutex::new(None));

    let spawned_at = Instant::now();
    let mic_reader =
        spawn_reader(mic_fifo_path.clone(), mic_time.clone(), spawned_at, stop.clone());
    let sys_reader =
        spawn_reader(sys_fifo_path.clone(), sys_time.clone(), spawned_at, stop.clone());

    std::thread::scope(|scope| {
        let (pump_handle, _ticker) = super::pump::spawn(
            scope, spawned_at, cfg, mic_fifo_path.clone(), sys_fifo_path.clone(), mic_tap,
            mic_rx, monitor, sys_rx, &stop, &paused, &events,
        )
        .expect("pump spawn must succeed in the E2E harness");

        // Wait (bounded) for both measurements, then end the session.
        let deadline = Instant::now() + Duration::from_secs(4);
        let read = |s: &Arc<Mutex<Option<Duration>>>| s.lock().ok().and_then(|g| *g);
        while (read(&mic_time).is_none() || read(&sys_time).is_none())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Relaxed);
        let _ = pump_handle.join();
    });
    let _ = mic_reader.join();
    let _ = sys_reader.join();

    let mic_t = mic_time.lock().ok().and_then(|g| *g);
    let sys_t = sys_time.lock().ok().and_then(|g| *g);
    eprintln!(
        "E2E-8: ffmpeg-7.x open threshold ({} bytes) delivered — mic {:?}, sys {:?}",
        super::pump::FFMPEG7_INPUT_OPEN_BYTES,
        mic_t,
        sys_t
    );
    let bound = Duration::from_millis(1000);
    for (name, t) in [("mic", mic_t), ("sys", sys_t)] {
        let t = t.unwrap_or_else(|| {
            panic!(
                "the {name} FIFO never delivered the ffmpeg-7.x open threshold — without \
                 the opening prime this is exactly the DRAGON-554 park (first byte at \
                 wall ~1.5s, video frozen meanwhile)"
            )
        });
        assert!(
            t < bound,
            "the {name} FIFO took {t:?} to deliver ffmpeg 7.x's input-open threshold; \
             it must arrive well before the render horizon's ~1.5s (the opening prime, \
             DRAGON-554) or every 7.x session opens on frozen video"
        );
    }
}

/// DRAGON-554: the [`super::owned::AudioPreflight`] seam both the PipeWire and the SCK
/// workers overlap their video bring-up with. A forced pre-flight failure must come
/// back through `join()` with the same named, actionable reason the inline call
/// reports, promptly (the seam adds no wait of its own), and `abandon()` on a failed
/// pre-flight must be a quiet no-op — the video-side failure paths call it blind.
#[test]
fn audio_preflight_thread_reports_the_named_failure() {
    let _lock = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _guard = GlobalStateGuard;
    // SAFETY: `test_lock()` is held for this whole test.
    unsafe {
        std::env::set_var("CCK_TEST_FORCE_OWNED_FAILURE", "1");
    }

    let started = Instant::now();
    let result = super::owned::AudioPreflight::start().join();
    let elapsed = started.elapsed();
    match result {
        Ok(owned) => {
            owned.cleanup();
            panic!("the forced-failure seam must fail through the threaded pre-flight too");
        }
        Err(reason) => assert_eq!(
            reason, "forced failure (test seam)",
            "the threaded pre-flight must carry the same named reason the inline call reports"
        ),
    }
    assert!(
        elapsed < Duration::from_secs(2),
        "the seam must add no wait of its own (took {elapsed:?})"
    );

    // The blind-teardown path the video-side failures use.
    super::owned::AudioPreflight::start().abandon();
}


/// DRAGON-647: the capture children carry `PR_SET_PDEATHSIG(SIGKILL)` as orphan
/// protection, and Linux binds that signal to the spawning THREAD, not the process.
/// The audio pre-flight runs on a short-lived worker thread (`AudioPreflight`), so a
/// mic child spawned from it was SIGKILLed ~120ms into every Linux recording the
/// moment that thread returned — silently (SIGKILL writes no stderr), leaving an
/// honest all-silence mic track and a "source stopped delivering" pump summary. The
/// fix spawns the captures on the session-long READER thread instead.
///
/// This drives the real tap from a spawner thread that exits immediately — the exact
/// production shape — and asserts the capture SURVIVES its spawner by well over the
/// ~120ms the bug allowed. A null sink stands in for the mic so the test is
/// deterministic on any box with pactl; like its siblings it skips loudly without one.
#[test]
fn mic_tap_survives_its_spawning_threads_exit() {
    let _guard = test_lock().lock().unwrap();
    let _state = GlobalStateGuard;
    let Some(mic_sink) = NullSink::load("cckpdsigmic") else {
        eprintln!("SKIPPED: pactl/null-sink unavailable");
        return;
    };
    crate::audio::config::set_mic_source(&mic_sink.monitor_source());
    let cfg = crate::audio::InputConfig {
        noise_suppression: false,
        echo_cancellation: false,
        auto_gain: false,
        gate: false,
        gate_auto: false,
        gate_threshold: 0.5,
        advanced_vad: false,
    };
    // The production shape: the tap is set up on a thread that dies right away.
    let setup = std::thread::spawn(move || {
        crate::audio::clean_mic::setup_clean_mic_tap(cfg, "", None)
    })
    .join()
    .expect("spawner thread panicked");
    let Some((mut handle, rx)) = setup else {
        eprintln!("SKIPPED: mic tap did not start (no ffmpeg?)");
        return;
    };
    // The spawner is gone. Before the fix the ffmpeg child died with it, the reader
    // saw EOF within ~120ms, and this loop would collect a handful of frames at
    // most. 1.5s of wall time and a full second of delivered audio is comfortably
    // beyond any startup jitter while keeping the suite fast.
    let t0 = Instant::now();
    let mut samples = 0usize;
    while t0.elapsed() < Duration::from_millis(1500) {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(tap) => samples += tap.samples.len(),
            Err(_) => break,
        }
    }
    handle.drain();
    assert!(
        samples >= 48_000,
        "the mic capture died with its spawning thread (DRAGON-647 regressed): only          {samples} samples (~{:.2}s) arrived",
        samples as f64 / 48000.0
    );
}
