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
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Serializes the two E2E tests (and guards against any future one) against each
/// other: both mutate the SAME process-global env var
/// (`CCK_TEST_MONITOR_SOURCE`) and `crate::audio::config`'s mic-source override,
/// and both claim fixed-name pactl modules — none of which is safe to do from two
/// threads at once (`cargo test`'s default runner is multi-threaded).
fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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
        Command::new(tool)
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
        let child = Command::new(crate::util::ffmpeg_path())
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
    let out = Command::new(crate::util::ffprobe_path())
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
    let Ok(out) = Command::new(crate::util::ffprobe_path())
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
    let out = Command::new(crate::util::ffprobe_path())
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
    let super::owned::OwnedAudioStart { mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx } =
        owned;

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

    let t0 = Instant::now();
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
            let elapsed = t0.elapsed().as_secs_f64();
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
    let out = Command::new(crate::util::ffmpeg_path())
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
    let out = Command::new(crate::util::ffprobe_path())
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
