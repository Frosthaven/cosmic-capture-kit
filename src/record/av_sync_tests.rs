//! End-to-end verification for the finalize pass.
//!
//! Unlike the rest of the suite, these tests SHELL OUT to real ffmpeg/ffprobe: they
//! build a recorder-shaped clip, run the ACTUAL [`super::finalize::finalize_with_intervals`]
//! on it, and re-measure the flash-vs-beep offset of the OUTPUT (via the same
//! [`super::sync_probe`] detectors the calibration flow uses) to prove the amix +
//! mute-gate filtergraph preserves A/V alignment exactly and mutes at the right
//! instant. The filter STRINGS are already unit-tested in `finalize.rs`; this checks
//! that those strings, fed to ffmpeg, actually produce the claimed output.
//!
//! ffmpeg is the ONE tool CLAUDE.md's "no ffmpeg in tests" rule excludes for this
//! module: every test LOUDLY skips — never silently passes — when ffmpeg/ffprobe
//! are absent, and RUNS wherever they're present (this repo's target machine).
//!
//! DRAGON-127 retired the legacy wallclock+CFR+segments recorder and, with it, the
//! per-channel A/V-sync delay-shift this pass used to apply (every production caller
//! now feeds already-correctly-timed audio straight from the media-clock pump) — the
//! delay-shift cases (`finalize_positive_delay_…`, `finalize_negative_delay_…`,
//! `finalize_per_channel_delays_…`) and the two-segment assembly E2E
//! (`assemble_two_segments_…`, which exercised `Segment`/`assemble_recording`, both
//! deleted) tested exactly that removed capability and are gone with it. The
//! two-segment assembly E2E's successors are the pause E2Es in
//! `media_clock_e2e_tests` (`media_clock_owned_session_continuity_e2e`,
//! `media_clock_user_shape_pause_no_mute_e2e`, `media_clock_early_long_pause_e2e`):
//! they cover the SAME pause-content semantics (a pause-free stretch's audio stays
//! aligned to its own video across a pause) end-to-end through the real owned
//! pipeline, rather than through a hand-built `Segment` list — strictly broader
//! coverage of the same property.

use super::finalize::finalize_with_intervals;
use super::sync_probe::{audio_rms_series, beep_times, measure_av_offset, SyncMeasurement};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;

/// Metadata line handed to finalize (no `codec=auto`, so it passes through untouched
/// — these tests measure audio timing, not the `--inspect` annotation).
const META: &str = "cck-av-sync-test | recorder=fixture";
/// Tolerance for the alignment assertion (output-vs-fixture). Priming and frame
/// quantisation cancel in the difference; measured drift is <2 ms, so ±40 ms is
/// generous.
const TOL: f64 = 0.040;
/// Tolerance for the one ABSOLUTE onset-position assertion (the surviving beep in
/// `mute_interval_lands_at_the_expected_time`). Unlike the differential alignment
/// test there is no fixture baseline to cancel priming (~21 ms) + the 33 ms
/// detection grid, so this gets the wider band the reviewer's own reasoning
/// prescribes for the non-cancelling case (observed drift there is ~1 ms).
const POS_TOL: f64 = 0.060;

/// Whether both `ffmpeg` and `ffprobe` respond to `-version`. The measurement decodes
/// and the fixture build both need them; either missing means the verification can't run.
fn have_ffmpeg() -> bool {
    let responds = |tool: PathBuf| {
        crate::util::quiet_command(tool)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    responds(crate::util::ffmpeg_path()) && responds(crate::util::ffprobe_path())
}

/// A LOUD skip: print why the test did not verify anything, then return. Never a silent
/// pass — an absent ffmpeg must be visible in the test log, not disguised as green.
macro_rules! require_ffmpeg {
    ($name:literal) => {
        if !have_ffmpeg() {
            eprintln!(
                "SKIPPED (loud): {} needs ffmpeg+ffprobe on PATH — A/V offset verification did not run",
                $name
            );
            return;
        }
    };
}

/// The shared recorder-shaped fixture: its path plus its OWN measured A/V offset (the
/// baseline every test differences against).
struct Fixture {
    path: PathBuf,
    offset_secs: f64,
}

/// Build (once) a temp `.mkv` shaped exactly like the recorder's capture temp: an
/// h264 video track plus TWO aac audio tracks titled `mic`/`system`. The video is
/// 320x180@30 black with a 3-frame white flash at t=1.5 s and t=3.0 s; both audio
/// tracks are the SAME sample-accurate 1 kHz beep at those instants (the `aevalsrc`
/// gate-in-the-expression trick from [`super::sync_probe::write_sync_clip`]). Panics
/// loudly if ffmpeg fails or the fixture isn't self-aligned — a broken baseline would
/// silently corrupt every downstream assertion.
fn build_fixture() -> Fixture {
    let path =
        std::env::temp_dir().join(format!("cck-av-sync-fixture-{}.mkv", std::process::id()));
    let video = "color=c=black:s=320x180:r=30:d=4,\
                 drawbox=c=white:t=fill:w=iw:h=ih:\
                 enable='between(t,1.5,1.6)+between(t,3,3.1)'";
    let audio = "aevalsrc='0.8*sin(2*PI*1000*t)*(between(t,1.5,1.58)+between(t,3,3.08))':\
                 s=48000:d=4";
    let status = crate::util::ffmpeg_command()
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", video])
        .args(["-f", "lavfi", "-i", audio])
        .args(["-map", "0:v", "-map", "1:a", "-map", "1:a"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "30", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac"])
        .args(["-metadata:s:a:0", "title=mic", "-metadata:s:a:1", "title=system"])
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .expect("ffmpeg should start to build the A/V-sync fixture");
    assert!(status.success(), "ffmpeg failed to build the A/V-sync fixture");
    let m = measure_av_offset(&path).expect("the freshly-built fixture must measure an A/V offset");
    assert!(
        m.offset_secs.abs() <= 0.030,
        "fixture must be self-aligned (measured {:.1} ms, expected ~0 within 30 ms)",
        m.offset_secs * 1000.0
    );
    Fixture { path, offset_secs: m.offset_secs }
}

/// The memoised fixture — built once per test process, reused by every test.
fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(build_fixture)
}

/// Temp paths created by a test, deleted on drop (even on panic). The memoised fixture
/// is deliberately NOT tracked here — it outlives every test.
struct Scratch {
    paths: Vec<PathBuf>,
}

impl Scratch {
    fn new() -> Self {
        Self { paths: Vec::new() }
    }

    /// A fresh, process-unique temp path `cck-av-sync-<stem>-<pid>.<ext>`, tracked for
    /// cleanup (and pre-cleared in case a prior aborted run left it behind).
    fn out(&mut self, stem: &str, ext: &str) -> PathBuf {
        let p = std::env::temp_dir()
            .join(format!("cck-av-sync-{stem}-{}.{ext}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        self.paths.push(p.clone());
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Measure `path`'s flash-vs-beep offset with the real detectors, panicking (loudly)
/// with the ffmpeg-side reason if it can't be measured.
fn measure(path: &Path) -> SyncMeasurement {
    measure_av_offset(path)
        .unwrap_or_else(|e| panic!("measuring {} failed: {e}", path.display()))
}

/// Decoded frame count of `path`'s video stream, via ffprobe. Container-agnostic proof
/// that a stream-copy preserved the video exactly (frames / fps = duration).
fn video_frame_count(path: &Path) -> u64 {
    let out = crate::util::ffprobe_command()
        .args([
            "-v", "error", "-select_streams", "v:0", "-count_frames",
            "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .expect("ffprobe should run to count frames");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or_else(|_| panic!("could not parse frame count for {}", path.display()))
}

/// Assert `got ≈ want` within `tol` seconds, reporting the miss in milliseconds.
fn assert_near(got: f64, want: f64, tol: f64, what: &str) {
    assert!(
        (got - want).abs() <= tol,
        "{what}: got {:.1} ms, want {:.1} ms (±{:.0} ms) — off by {:.1} ms",
        got * 1000.0,
        want * 1000.0,
        tol * 1000.0,
        (got - want).abs() * 1000.0,
    );
}

/// `finalize_with_intervals` with no mute intervals must reproduce the recorded
/// alignment byte-for-byte in timing (the output's offset equals the fixture's own)
/// AND stream-copy the video exactly (identical frame count).
#[test]
fn finalize_preserves_av_alignment_and_stream_copies_video() {
    require_ffmpeg!("finalize_preserves_av_alignment_and_stream_copies_video");
    let fix = fixture();
    let mut scratch = Scratch::new();
    let out = scratch.out("preserve", "mp4");
    finalize_with_intervals(&fix.path, &out, &[], &[], false, META)
        .expect("finalize with no mute intervals should succeed");
    let got = measure(&out).offset_secs;
    assert_near(got, fix.offset_secs, TOL, "finalize output must keep the fixture's alignment");
    assert_eq!(
        video_frame_count(&out),
        video_frame_count(&fix.path),
        "finalize must stream-copy the video, not re-encode or trim it"
    );
}

/// A muted mic must not affect the mixed result: with the mic fully off (an
/// always-off interval) and the system channel untouched, the output must still
/// read the fixture's own alignment (the system channel governs; amix's
/// `normalize=0` and the mic's silence don't shift it).
#[test]
fn finalize_a_fully_muted_channel_does_not_shift_the_mix() {
    require_ffmpeg!("finalize_a_fully_muted_channel_does_not_shift_the_mix");
    let fix = fixture();
    let mut scratch = Scratch::new();
    let out = scratch.out("muted-chan", "mp4");
    // Mic muted for the whole clip; system passes through untouched.
    finalize_with_intervals(&fix.path, &out, &[(0.0, 1.0e9)], &[], false, META)
        .expect("finalize with a fully-muted mic should succeed");
    let got = measure(&out).offset_secs;
    assert_near(
        got,
        fix.offset_secs,
        TOL,
        "the audible (system) channel alone must still read the fixture's alignment",
    );
}

/// The mute-interval end-to-end pin: a mid-recording OFF interval on the system
/// channel must silence exactly that window — the 3.0 s beep (inside [2.0, 4.0))
/// is gone, the 1.5 s beep (before it) survives. Isolates the mic (muted from t0)
/// so its untouched copy of the 3.0 s beep can't fill the gap the sys mute opens.
#[test]
fn mute_interval_lands_at_the_expected_time() {
    require_ffmpeg!("mute_interval_lands_at_the_expected_time");
    let fix = fixture();
    let mut scratch = Scratch::new();
    let out = scratch.out("mute-window", "mp4");
    finalize_with_intervals(&fix.path, &out, &[(0.0, 1.0e9)], &[(2.0, 4.0)], false, META)
        .expect("finalize with a mid-recording sys mute should succeed");
    let rms = audio_rms_series(&out).expect("decode the finalized audio");
    let beeps = beep_times(&rms);
    assert_eq!(
        beeps.len(),
        1,
        "exactly one beep must survive — the 1.5 s beep; found {beeps:?}"
    );
    assert_near(beeps[0], 1.5, POS_TOL, "the surviving beep must sit at its original 1.5 s");
}
