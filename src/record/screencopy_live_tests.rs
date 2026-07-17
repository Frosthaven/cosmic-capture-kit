//! LIVE Linux recording anti-freeze proof (DRAGON-167), `#[ignore]`-gated so it never
//! runs in the normal suite — it needs a real COSMIC/Wayland session, a working
//! PulseAudio-compatible server, and ffmpeg/ffprobe/ffplay. Run explicitly on the box:
//!
//! ```text
//! WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 \
//!   cargo test -- --ignored --nocapture live_screencopy
//! ```
//!
//! These are the Linux twins of `sck_live_tests`'s DRAGON-164 freeze nets. They drive
//! the REAL public entry point `start_region_recording` (which on Linux runs the
//! screencopy CPU worker) through a short record → stop cycle WHILE a separate `ffplay`
//! process animates a full-motion test pattern on the session (guaranteed per-frame
//! pixel change), then freezedetect-assert the finished clip is frozen for only a small
//! fraction of its duration. A static-screen test cannot discriminate a stale-feed
//! freeze from a healthy static capture, so forced motion is essential.
//!
//! Coverage note: the screencopy CPU path is PULL-model (it grabs a fresh frame per tick
//! synchronously — see `record_screencopy_owned`), so it is IMMUNE to the DRAGON-167
//! stale-frame shape by construction; this test proves that immunity holds under real
//! encoder load on the live session. The PipeWire portal path (which HAS the fixed shape)
//! needs an interactive portal grant to obtain its stream fd and so cannot be driven
//! headlessly here — it is covered by the synthetic `latch_freshest` unit tests in
//! `record::pipewire` (which reproduce the exact backpressure staleness the fix removes).
//! See DRAGON-167's coverage matrix.

use super::{start_region_recording, RecordSettings, RegionRecordParams};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn ffprobe_json(path: &std::path::Path) -> String {
    let out = std::process::Command::new(crate::util::ffprobe_path())
        .args([
            "-v", "error", "-show_entries",
            "stream=codec_type,codec_name,duration,width:format=duration", "-of", "json",
        ])
        .arg(path)
        .output()
        .expect("ffprobe failed to run (is it installed?)");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Pull the first `duration` value that appears AFTER the given `codec_type` marker in
/// the ffprobe JSON — crude but dependency-free (mirrors the SCK live test's parser).
fn duration_after(json: &str, marker: &str) -> Option<f64> {
    let idx = json.find(marker)?;
    let rest = &json[idx..];
    let d = rest.find("\"duration\"")?;
    let tail = &rest[d..];
    let colon = tail.find(':')?;
    let q1 = tail[colon..].find('"')? + colon + 1;
    let q2 = tail[q1..].find('"')? + q1;
    tail[q1..q2].parse().ok()
}

/// Whether ffmpeg/ffprobe are usable — the live tests LOUDLY skip (never a silent pass)
/// when they aren't, mirroring the suite convention.
fn have_ffmpeg() -> bool {
    std::process::Command::new(crate::util::ffmpeg_path())
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Locate `ffplay` next to the configured ffmpeg, else on `PATH`. `None` if absent.
fn which_ffplay() -> Option<std::path::PathBuf> {
    let ffmpeg = crate::util::ffmpeg_path();
    let cand = std::path::Path::new(&ffmpeg).with_file_name("ffplay");
    if cand.exists() {
        return Some(cand);
    }
    let probe = std::process::Command::new("ffplay")
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    matches!(probe, Ok(s) if s.success()).then(|| std::path::PathBuf::from("ffplay"))
}

/// Spawn a borderless full-motion `ffplay` window at logical `(x, y)` sized `w x h` (a
/// per-frame-changing `testsrc2`), returning the child so the caller kills it after the
/// capture. `None` if ffplay is unavailable. Gives the window a moment to map.
fn spawn_motion_window(x: i32, y: i32, w: u32, h: u32) -> Option<std::process::Child> {
    let ffplay = which_ffplay()?;
    let child = std::process::Command::new(ffplay)
        .args(["-hide_banner", "-loglevel", "error", "-noborder", "-alwaysontop"])
        .args(["-left", &x.to_string(), "-top", &y.to_string()])
        .args(["-x", &w.to_string(), "-y", &h.to_string()])
        .args(["-f", "lavfi", "-i", &format!("testsrc2=size={w}x{h}:rate=30")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn ffplay motion source");
    std::thread::sleep(Duration::from_millis(800));
    Some(child)
}

/// The first enumerated Wayland output's `(logical_pos, logical_size)`, or `None` when no
/// compositor/output is reachable (a bare SSH shell with no `WAYLAND_DISPLAY`).
fn first_output() -> Option<((i32, i32), (i32, i32))> {
    let (_conn, _queue, data) = crate::screencopy::connect(false)?;
    crate::screencopy::outputs(&data)
        .into_iter()
        .next()
        .map(|(_, _, pos, size)| (pos, size))
}

/// Standard live recording settings: the CPU software encoder (`x264`, so the test does
/// not depend on a VAAPI device), both audio channels, writing to `out_path`.
fn live_settings(out_path: std::path::PathBuf) -> RecordSettings {
    RecordSettings {
        fps: 30,
        preferred_encoder: "software".to_string(),
        presets: crate::encode::Presets::default(),
        zero_copy: false,
        mic: true,
        system_audio: true,
        bitrate_kbps: 8000,
        audio_offset_ms: 0,
        auto_device_compensation: true,
        max_res: (0, 0),
        metadata: String::new(),
        out_path,
    }
}

/// Drive a full record→stop cycle for `params`, recording ~`secs` of MEDIA time, and
/// return the finalized path — panicking (loudly) on any pre-flight/worker failure. Same
/// shape as `sck_live_tests::record_target`.
fn record_target(params: RegionRecordParams, secs: u64) -> std::path::PathBuf {
    let handle = start_region_recording(params);
    let ready = Instant::now() + Duration::from_secs(30);
    while handle.dims.lock().unwrap().is_none() {
        if let Some(Err(e)) = handle.done.lock().unwrap().clone() {
            panic!(
                "recording FAILED before first frame: {e}\n(needs a live COSMIC session + \
                 a PulseAudio-compatible server for the audio pre-flight)"
            );
        }
        assert!(Instant::now() < ready, "worker never captured a first frame");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_secs(secs));
    handle.stop.store(true, Ordering::Relaxed);

    let deadline = Instant::now() + Duration::from_secs(60);
    let result = loop {
        if let Some(r) = handle.done.lock().unwrap().clone() {
            break r;
        }
        assert!(Instant::now() < deadline, "recording never finished (worker hung?)");
        std::thread::sleep(Duration::from_millis(200));
    };
    result.unwrap_or_else(|e| panic!("recording FAILED: {e}"))
}

/// Run freezedetect over `path` and return `(total_frozen_seconds, video_duration)`. A
/// HEALTHY recording of a moving screen freezes for only a small fraction of its length;
/// a regressed one (stale-feed re-feeding one old frame) freezes for nearly all of it.
/// Dependency-free parse of ffmpeg's stderr freeze markers (mirrors the SCK live test).
fn freeze_profile(path: &std::path::Path, noise: &str, min_dur: f64) -> (f64, f64) {
    let out = std::process::Command::new(crate::util::ffmpeg_path())
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(path)
        .args([
            "-vf",
            &format!("freezedetect=n={noise}:d={min_dur}"),
            "-map", "0:v", "-f", "null", "-",
        ])
        .output()
        .expect("ffmpeg freezedetect failed to run");
    let log = String::from_utf8_lossy(&out.stderr);
    let mut frozen = 0.0f64;
    for line in log.lines() {
        if let Some(i) = line.find("freeze_duration:")
            && let Ok(d) = line[i + "freeze_duration:".len()..].trim().parse::<f64>()
        {
            frozen += d;
        }
    }
    let json = ffprobe_json(path);
    let vdur = duration_after(&json, "\"video\"")
        .or_else(|| duration_after(&json, "\"format\""))
        .unwrap_or(0.0);
    (frozen, vdur)
}

/// Record `params` for `secs`, then freezedetect the result and assert it was frozen for
/// well under half its duration. `label` names the path for the log line. Kills `motion`
/// after the capture.
fn assert_capture_not_frozen(
    label: &str,
    params: RegionRecordParams,
    secs: u64,
    mut motion: std::process::Child,
) {
    let out_path = params.settings.out_path.clone();
    let _ = std::fs::remove_file(&out_path);
    let path = record_target(params, secs);
    let _ = motion.kill();
    let _ = motion.wait();
    let (frozen, vdur) = freeze_profile(&path, "-60dB", 0.5);
    let frac = if vdur > 0.0 { frozen / vdur } else { 1.0 };
    eprintln!(
        "LIVE {label}: video={vdur:.2}s frozen={frozen:.2}s ({:.0}% frozen)",
        frac * 100.0
    );
    let _ = std::fs::remove_file(&path);
    assert!(vdur > 2.0, "{label}: video too short ({vdur:.2}s); did it start?");
    assert!(
        frac < 0.5,
        "{label}: frozen {:.0}% of {vdur:.2}s (video stopped tracking the screen — DRAGON-167)",
        frac * 100.0
    );
}

/// DRAGON-167 anti-freeze net for the screencopy REGION path: record a region of the
/// first output that overlaps an animating ffplay window, and assert the clip is not
/// frozen. Proves the pull-model screencopy worker keeps the video tracking the screen
/// under real encoder load (its documented immunity to the stale-frame shape). Skips
/// loudly when the compositor / ffplay / ffmpeg is absent.
#[test]
#[ignore = "live: needs a COSMIC session, a Pulse server, ffmpeg/ffplay/ffprobe"]
fn live_screencopy_region_moving_is_not_frozen() {
    if !have_ffmpeg() {
        eprintln!("SKIP (loud): ffmpeg/ffprobe unavailable — the DRAGON-167 live proof did not run");
        return;
    }
    let Some((pos, size)) = first_output() else {
        eprintln!(
            "SKIP (loud): no Wayland output reachable (set WAYLAND_DISPLAY + XDG_RUNTIME_DIR to \
             the live COSMIC session) — the DRAGON-167 live proof did not run"
        );
        return;
    };
    eprintln!("first output pos={pos:?} size={size:?}");

    // Animate a window near the output's top-left, then record a region covering it.
    let mx = pos.0 + 150;
    let my = pos.1 + 150;
    let Some(motion) = spawn_motion_window(mx, my, 900, 700) else {
        eprintln!("SKIP (loud): ffplay not found — cannot force motion");
        return;
    };
    let dir = std::env::temp_dir();
    let out_path = dir.join(format!("cck-live-scr-region-{}.mkv", std::process::id()));
    // A region wholly inside the output, overlapping the motion window.
    let rw = 1000u32.min((size.0 - 100).max(2) as u32);
    let rh = 800u32.min((size.1 - 100).max(2) as u32);
    let params = RegionRecordParams {
        x: pos.0 + 100,
        y: pos.1 + 100,
        w: rw,
        h: rh,
        cursor: true,
        settings: live_settings(out_path),
    };
    assert_capture_not_frozen("SCREENCOPY REGION MOTION", params, 5, motion);
}

/// DRAGON-167 anti-freeze net for the screencopy FULL-OUTPUT (monitor) path: record the
/// whole first output while an ffplay window animates on it, and assert the clip is not
/// frozen. A full-output region is the largest encoder load the CPU path faces (where a
/// stale-feed freeze would bite hardest), so this is the strongest live immunity check.
#[test]
#[ignore = "live: needs a COSMIC session, a Pulse server, ffmpeg/ffplay/ffprobe"]
fn live_screencopy_fulloutput_moving_is_not_frozen() {
    if !have_ffmpeg() {
        eprintln!("SKIP (loud): ffmpeg/ffprobe unavailable — the DRAGON-167 live proof did not run");
        return;
    }
    let Some((pos, size)) = first_output() else {
        eprintln!(
            "SKIP (loud): no Wayland output reachable (set WAYLAND_DISPLAY + XDG_RUNTIME_DIR) — \
             the DRAGON-167 live proof did not run"
        );
        return;
    };
    eprintln!("recording full output pos={pos:?} size={size:?}");
    let Some(motion) = spawn_motion_window(pos.0 + 120, pos.1 + 120, 1280, 960) else {
        eprintln!("SKIP (loud): ffplay not found — cannot force motion");
        return;
    };
    let dir = std::env::temp_dir();
    let out_path = dir.join(format!("cck-live-scr-full-{}.mkv", std::process::id()));
    let params = RegionRecordParams {
        x: pos.0,
        y: pos.1,
        w: size.0.max(1) as u32,
        h: size.1.max(1) as u32,
        cursor: true,
        settings: live_settings(out_path),
    };
    assert_capture_not_frozen("SCREENCOPY FULL MOTION", params, 6, motion);
}
