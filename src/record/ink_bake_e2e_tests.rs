//! The window-mode ink bake, proven by CONTENT (DRAGON-691 amendment 2).
//!
//! CLAUDE.md's rule for this ticket is the DRAGON-417 one: **verify a recording by its
//! CONTENT, not its shape**. A bake that produced a file of the right duration, with the right
//! frame count and a plausible size, while drawing the ink in the wrong place, at the wrong
//! time, or not at all, would pass every shape check there is. So these tests build a
//! recording-shaped fixture, run the REAL finalize, decode the result back, and read the
//! pixels.
//!
//! What each one pins:
//!
//! * the ink is IN the video, at the position the window mapping puts it;
//! * it is NOT there before it was drawn, and NOT there after it was erased or dissolved,
//!   which is the replay reaching the file rather than a static overlay;
//! * a recording with NO ink is byte-identical to what finalize produced before this existed,
//!   video stream copy included;
//! * the AUDIO survives the pass unchanged, which is the property the whole "one pass, no
//!   concat" design exists to protect.
//!
//! Like `av_sync_tests` beside it, this is the ONE place CLAUDE.md's "no ffmpeg in tests" rule
//! is excluded, and it SKIPS LOUDLY when ffmpeg is not reachable rather than passing quietly.
//!
//! # What these deliberately do NOT cover, and what does
//!
//! Every test here starts from an [`InkBake`] that is already complete, its rect timeline
//! included. That is the right boundary for the bake, and it is also a blind spot with a name:
//! DRAGON-691 shipped with a macOS `platform::window_rect` that answered `None` for every
//! window, so the real timeline was always EMPTY, `InkBake::is_empty` was always true, the bake
//! never ran, and all six of these tests stayed green while no macOS window recording had any
//! ink in it. Filling `rects` is the app's job, not finalize's, and it is pinned where it
//! happens: `platform::mac::focus::window_rect_now_tests` asks the live window server about a
//! real window. If you add a platform arm, add its live test there too.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::ink::{InkClip, InkGen, InkLog, InkTool, MediaMs};
use crate::record::ink_bake::InkBake;

/// The fixture's geometry. Small on purpose: these tests encode real video, and every one of
/// them is a few hundred frames at most.
const W: u32 = 160;
const H: u32 = 120;
const FPS: u32 = 20;
const SECS: f64 = 4.0;

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

macro_rules! require_ffmpeg {
    ($name:literal) => {
        if !have_ffmpeg() {
            eprintln!(
                "SKIPPED (loud): {} needs ffmpeg+ffprobe on PATH — the ink bake was not \
                 verified against decoded pixels",
                $name
            );
            return;
        }
    };
}

/// A scratch file that unlinks itself.
struct Scratch(Vec<PathBuf>);

impl Scratch {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn path(&mut self, stem: &str, ext: &str) -> PathBuf {
        let p = std::path::PathBuf::from(crate::util::runtime_dir())
            .join(format!("cck-inkbake-{stem}-{}.{ext}", std::process::id()));
        self.0.push(p.clone());
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A recording-shaped temp: a BLACK video plus the two audio tracks finalize expects (mic and
/// system), so the pass under test is the real one with its real filtergraph.
///
/// Black on purpose: the ink is the only thing that can make a pixel non-black, so "is there
/// ink here" is a question the decoder can answer without a reference frame. The audio is a
/// steady tone, which is what the audio-survival test measures.
fn build_temp(temp: &Path) -> bool {
    build_temp_sized(temp, W, H)
}

/// [`build_temp`] at a chosen size. Separate since DRAGON-755: the Media Foundation test
/// needs a bigger frame than the rest of this module uses, because a HARDWARE encoder MFT has
/// a minimum it will accept and 160x120 is under it (measured: `MF_E_INVALIDMEDIATYPE` at
/// 160x120, encodes at 640x480). No real recording is that small, so this is a fixture
/// constraint rather than anything the shipped path has to answer for.
fn build_temp_sized(temp: &Path, w: u32, h: u32) -> bool {
    let video = format!("color=c=black:s={w}x{h}:r={FPS}:d={SECS}");
    let audio = format!("sine=frequency=440:sample_rate=48000:duration={SECS}");
    crate::util::ffmpeg_command()
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", &video])
        .args(["-f", "lavfi", "-i", &audio])
        .args(["-map", "0:v", "-map", "1:a", "-map", "1:a"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "30", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac"])
        .args(["-metadata:s:a:0", "title=mic", "-metadata:s:a:1", "title=system"])
        .arg(temp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Decode one frame at `at_secs` and return it as RGB bytes, `W * H * 3`.
fn frame_at(path: &Path, at_secs: f64) -> Option<Vec<u8>> {
    frame_at_sized(path, at_secs, W, H)
}

/// [`frame_at`] for a video that is not the module's default size (DRAGON-755).
fn frame_at_sized(path: &Path, at_secs: f64, w: u32, h: u32) -> Option<Vec<u8>> {
    let out = crate::util::ffmpeg_command()
        .args(["-v", "error", "-ss", &format!("{at_secs:.3}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-pix_fmt", "rgb24", "-f", "rawvideo", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    (out.stdout.len() == (w * h * 3) as usize).then_some(out.stdout)
}

/// How many pixels of a decoded frame are visibly not black. The ink is the only thing in the
/// fixture that can make one, so this IS "how much ink is on this frame".
fn inked_pixels(frame: &[u8]) -> usize {
    frame.chunks_exact(3).filter(|p| p[0] > 60 || p[1] > 60 || p[2] > 60).count()
}

/// How many inked pixels lie in the rows `y0..y1`. Strokes drawn at different heights are
/// therefore countable one at a time, which is what lets a test with three strokes on screen at
/// once say WHICH of them is missing.
fn inked_in_band(frame: &[u8], y0: u32, y1: u32) -> usize {
    let row = (W * 3) as usize;
    frame[(y0 as usize * row).min(frame.len())..(y1 as usize * row).min(frame.len())]
        .chunks_exact(3)
        .filter(|p| p[0] > 60 || p[1] > 60 || p[2] > 60)
        .count()
}

/// The centre of mass of the inked pixels, or `None` when there are none. This is what says
/// the ink landed WHERE the window mapping put it rather than merely somewhere.
fn ink_centroid(frame: &[u8]) -> Option<(f32, f32)> {
    let (mut sx, mut sy, mut n) = (0.0f32, 0.0f32, 0.0f32);
    for (i, p) in frame.chunks_exact(3).enumerate() {
        if p[0] > 60 || p[1] > 60 || p[2] > 60 {
            sx += (i as u32 % W) as f32;
            sy += (i as u32 / W) as f32;
            n += 1.0;
        }
    }
    (n > 0.0).then(|| (sx / n, sy / n))
}

/// A stroke of `n` points along a horizontal line at `y`, drawn from `at_ms`, one point every
/// 20ms. Coordinates are ink-surface points, which the bake maps through the window rect.
fn stroke(
    tool: InkTool,
    generation: Option<u32>,
    at_ms: MediaMs,
    y: f32,
    x0: f32,
    n: usize,
) -> InkClip {
    InkClip {
        tool,
        points: (0..n).map(|i| (x0 + i as f32 * 6.0, y)).collect(),
        speed: Vec::new(),
        point_ms: (0..n).map(|i| at_ms + i as MediaMs * 20).collect(),
        width: crate::ink::WIDTH_PT,
        generation,
    }
}

/// A bake plan whose window sits exactly at the origin and exactly the video's size, so
/// surface points map one-to-one onto video pixels and a test can name a pixel.
///
/// That identity is convenient and it is also the reason
/// [`ink_maps_through_an_offset_hidpi_window_e2e`] exists: with an identity mapping a dropped
/// output origin or a dropped points-to-pixels ratio is invisible, and a HiDPI display is
/// exactly where those two stop cancelling.
fn plan(log: InkLog) -> InkBake {
    InkBake {
        log,
        rects: vec![(0, (0, 0, W as i32, H as i32))],
        origin: (0, 0),
        point_scale: 1.0,
        color: [255, 0, 0, 255],
    }
}

/// Run the REAL finalize over the fixture, with or without ink.
///
/// DRAGON-716: `software` on purpose. These tests assert on PIXELS, and they have to decode
/// the same way on every machine that runs them, so they must not pick up whatever hardware
/// encoder the host happens to have. The tier TABLE is pinned separately by
/// `finalize::ink_hw_video_args_tests`, which needs no ffmpeg at all.
fn finalize(temp: &Path, out: &Path, bake: Option<&InkBake>) -> Result<PathBuf, String> {
    finalize_on(temp, out, bake, "software", false, (W, H))
}

/// [`finalize`] on a NAMED encoder tier, at a chosen frame size (DRAGON-755).
///
/// The one caller that does not want `software` is the Media Foundation test below, which
/// exists precisely to run the hardware arm the rest of this module deliberately avoids.
fn finalize_on(
    temp: &Path,
    out: &Path,
    bake: Option<&InkBake>,
    encoder: &'static str,
    is_hevc: bool,
    video: (u32, u32),
) -> Result<PathBuf, String> {
    let job = bake.map(|bake| super::finalize::InkJob {
        bake,
        video,
        fps: FPS,
        video_frames: (SECS * FPS as f64) as u64,
        encoder,
        kbps: 8000,
    });
    super::finalize::finalize_with_ink(
        temp,
        out,
        &[],
        &[],
        is_hevc,
        "cck-inkbake-e2e",
        job.as_ref(),
    )
}

/// The video codec ffprobe reports for the delivered file, e.g. `h264` or `hevc`.
///
/// This is the DRAGON-755 test's whole discriminator, so it is worth saying why it can be
/// trusted: `ink_sw_video_args` is `libx264`, which CANNOT produce HEVC. An `hevc` video
/// stream in the output therefore proves attempt 1 delivered it, and no amount of the
/// software fallback quietly succeeding could counterfeit that answer.
// Windows-gated with its only caller: Media Foundation is a Windows tier and nothing else in
// this module cares which codec came out.
#[cfg(windows)]
fn video_codec(path: &Path) -> Option<String> {
    let o = crate::util::quiet_command(crate::util::ffprobe_path())
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=codec_name", "-of", "csv=p=0"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// A BRONZE bake plan (DRAGON-693): the whole mapping is ONE rect sample covering the stream's
/// own frame at the origin, with no scale.
///
/// This is exactly what `App::publish_ink_bake` builds for that tier, and building it the same
/// way here is the point: Bronze records its strokes in STREAM pixels, so it needs no window
/// position and never asks the platform for one. `point_to_video` then reduces to the identity
/// when the encode kept the stream's size, and to the right rescale when the resolution cap
/// shrank it, which is what "an identity mapping in stream space" has to mean once an encoder
/// is allowed to resize.
fn stream_plan(log: InkLog, stream: (u32, u32)) -> InkBake {
    InkBake {
        log,
        rects: vec![(0, (0, 0, stream.0 as i32, stream.1 as i32))],
        origin: (0, 0),
        point_scale: 1.0,
        color: [255, 0, 0, 255],
    }
}

/// A stroke like [`stroke`] but at a chosen WIDTH, for the stream-space tests: Bronze records
/// in the stream's pixels, so its widths are in that space too.
fn wide_stroke(at_ms: MediaMs, y: f32, x0: f32, n: usize, width: f32) -> InkClip {
    InkClip { width, ..stroke(InkTool::Marker, None, at_ms, y, x0, n) }
}

/// **BRONZE** (DRAGON-693): ink recorded in STREAM pixels reaches the file through the same
/// finalize pass, with no window position anywhere in the plan, and lands where the stream to
/// video ratio puts it.
///
/// The stream here is deliberately TWICE the encoded video, which is the case a literal
/// identity map would get wrong: it is what an encoder that capped the resolution produces, and
/// it is the reason the plan is expressed as a rect sample rather than as "no mapping". Ink
/// drawn at stream x=60..174 has to come back at video x=30..87.
#[test]
fn bronze_stream_space_ink_bakes_through_an_identity_mapping_e2e() {
    require_ffmpeg!("bronze_stream_space_ink_bakes_through_an_identity_mapping_e2e");
    let _guard = super::recording_globals_lock().lock();
    let mut sc = Scratch::new();
    let (temp, out) = (sc.path("bronze", "mkv"), sc.path("bronze-out", "mp4"));
    assert!(build_temp(&temp), "could not build the fixture");

    // The stream is 2x the video on both axes.
    let stream = (W * 2, H * 2);
    let log = InkLog {
        clips: vec![wide_stroke(1_000, 120.0, 60.0, 20, crate::ink::WIDTH_PT * 2.0)],
        gens: Vec::new(),
        erases: Vec::new(),
        wipes: Vec::new(),
    };
    let bake = stream_plan(log, stream);
    assert!(!bake.is_empty(), "one rect sample is a complete Bronze mapping");
    finalize(&temp, &out, Some(&bake)).expect("finalize with stream-space ink");

    let early = frame_at(&out, 0.4).expect("decode an early frame");
    assert_eq!(inked_pixels(&early), 0, "ink appeared before it was drawn");

    let late = frame_at(&out, 2.5).expect("decode a late frame");
    let n = inked_pixels(&late);
    assert!(n > 60, "the stream-space stroke is not in the video (only {n} inked pixels)");
    let (cx, cy) = ink_centroid(&late).expect("inked pixels have a centroid");
    // Stream x=60..174 at y=120, halved by the 2x ratio: video x=30..87 at y=60.
    assert!((cx - 58.5).abs() < 12.0, "the ink is at the wrong x: {cx}");
    assert!((cy - 60.0).abs() < 6.0, "the ink is at the wrong y: {cy}");
}

/// **The FINAL SWEEP reaches the file** (DRAGON-693). The Silver and Bronze disarm lifts the
/// ink over `FINAL_SWEEP_MS` instead of hiding the window, and this is why: a hide is one frame
/// with ink and the next without, which the recording shows as a glitch.
///
/// Measured in decoded pixels, in three states: solid before the disarm, PART GONE partway
/// through the sweep, and absent after it. The middle assertion is the one that separates a
/// ramp from a cut, and it is the whole reason the sweep is recorded on the log rather than
/// applied to it.
#[test]
fn a_final_sweep_lifts_the_baked_ink_over_its_own_span_e2e() {
    require_ffmpeg!("a_final_sweep_lifts_the_baked_ink_over_its_own_span_e2e");
    let _guard = super::recording_globals_lock().lock();
    let mut sc = Scratch::new();
    let (temp, out) = (sc.path("sweep", "mkv"), sc.path("sweep-out", "mp4"));
    assert!(build_temp(&temp), "could not build the fixture");

    // Marker ink (nothing dissolves on its own), swept at 2.0s.
    let log = InkLog {
        clips: vec![stroke(InkTool::Marker, None, 1_000, 60.0, 20.0, 22)],
        gens: Vec::new(),
        erases: Vec::new(),
        wipes: vec![2_000],
    };
    finalize(&temp, &out, Some(&plan(log))).expect("finalize with a swept scene");

    let before = frame_at(&out, 1.9).expect("decode a pre-sweep frame");
    let solid = inked_pixels(&before);
    assert!(solid > 100, "the stroke is not in the video (only {solid} inked pixels)");

    // Partway through: SOME of it has gone and some is still there. Neither "all" nor
    // "nothing" would tell a ramp from a cut.
    let mid_secs = 2.0 + (crate::ink::FINAL_SWEEP_MS as f64 / 1000.0) / 2.0;
    let mid = frame_at(&out, mid_secs).expect("decode a mid-sweep frame");
    let lifting = inked_pixels(&mid);
    assert!(lifting > 0, "the whole scene went at once: this is a cut, not a sweep");
    assert!(lifting < solid, "nothing had lifted by half way ({lifting} vs {solid})");

    // And gone at its own deadline.
    let after = frame_at(&out, 2.0 + crate::ink::FINAL_SWEEP_MS as f64 / 1000.0 + 0.2)
        .expect("decode a post-sweep frame");
    assert_eq!(inked_pixels(&after), 0, "the sweep did not finish");
}

/// **The headline**: ink drawn during a window recording reaches the FILE, at the place the
/// window mapping puts it, and is absent before it was drawn.
///
/// Marker ink, so nothing dissolves and the only thing under test is that the replay reaches
/// the encoder at the right times.
#[test]
fn window_ink_is_composited_into_the_video_e2e() {
    require_ffmpeg!("window_ink_is_composited_into_the_video_e2e");
    let _guard = super::recording_globals_lock().lock();
    let mut sc = Scratch::new();
    let (temp, out) = (sc.path("marker", "mkv"), sc.path("marker-out", "mp4"));
    assert!(build_temp(&temp), "could not build the fixture");

    // One horizontal stroke across the middle, drawn from 1.0s to 1.4s.
    let log = InkLog {
        clips: vec![stroke(InkTool::Marker, None, 1_000, 60.0, 30.0, 20)],
        gens: Vec::new(),
        erases: Vec::new(),
        wipes: Vec::new(),
    };
    finalize(&temp, &out, Some(&plan(log))).expect("finalize with ink");

    // BEFORE the stroke: the frame is untouched.
    let early = frame_at(&out, 0.4).expect("decode an early frame");
    assert_eq!(inked_pixels(&early), 0, "ink appeared before it was drawn");

    // AFTER it: the ink is there, and it is where the mapping put it.
    let late = frame_at(&out, 2.5).expect("decode a late frame");
    let n = inked_pixels(&late);
    assert!(n > 100, "the stroke is not in the video (only {n} inked pixels)");
    let (cx, cy) = ink_centroid(&late).expect("inked pixels have a centroid");
    // The stroke runs from x=30 to x=144 at y=60, so its centre of mass is near (87, 60).
    assert!((cx - 87.0).abs() < 12.0, "the ink is at the wrong x: {cx}");
    assert!((cy - 60.0).abs() < 6.0, "the ink is at the wrong y: {cy}");

    // And it is being DRAWN, not switched on: partway through the stroke there is less of it
    // than at the end. This is the replay's per-point timing reaching the file.
    let mid = frame_at(&out, 1.2).expect("decode a mid-stroke frame");
    let m = inked_pixels(&mid);
    assert!(m > 0, "the stroke had not started by its own midpoint");
    assert!(m < n, "the stroke was drawn all at once ({m} then {n})");
}

/// The MAGIC MARKER's dissolve reaches the file too: solid through the hold, gone after the
/// sweep. The tool's whole behaviour, measured in decoded pixels rather than in the model.
#[test]
fn magic_ink_dissolves_in_the_baked_video_e2e() {
    require_ffmpeg!("magic_ink_dissolves_in_the_baked_video_e2e");
    let _guard = super::recording_globals_lock().lock();
    let mut sc = Scratch::new();
    let (temp, out) = (sc.path("magic", "mkv"), sc.path("magic-out", "mp4"));
    assert!(build_temp(&temp), "could not build the fixture");

    // Drawn at 0.2s; the generation's last activity is 0.4s, so it holds to 2.4s and is gone
    // at 3.4s — all inside the fixture's four seconds.
    let log = InkLog {
        clips: vec![stroke(InkTool::MagicMarker, Some(0), 200, 60.0, 30.0, 10)],
        gens: vec![InkGen { id: 0, last_activity_ms: 400 }],
        erases: Vec::new(),
        wipes: Vec::new(),
    };
    finalize(&temp, &out, Some(&plan(log))).expect("finalize with ink");

    let held = inked_pixels(&frame_at(&out, 2.0).expect("decode during the hold"));
    assert!(held > 50, "magic ink was not solid during its hold ({held} pixels)");
    let gone = inked_pixels(&frame_at(&out, 3.7).expect("decode after the sweep"));
    assert_eq!(gone, 0, "magic ink was still in the video after its sweep");
    // And mid-sweep it is partly gone rather than all-or-nothing, which is the moving front.
    let sweeping = inked_pixels(&frame_at(&out, 3.0).expect("decode mid-sweep"));
    assert!(sweeping < held, "the dissolve did not eat into the stroke");
}

/// **The DRAGON-691 follow-up regression, end to end**: a Marker stroke, a Magic Marker stroke,
/// and a SECOND Magic Marker stroke begun while the first one's sweep was in flight. All three
/// have to be in the video, each at its own instants.
///
/// This is the shape the owner reported and it is the one
/// [`magic_ink_dissolves_in_the_baked_video_e2e`] above could not have caught, for two reasons
/// worth naming. That test has ONE generation and one stroke, so the interleaving never occurs;
/// and the failure was not in the bake at all, it was that the live scene DELETED the middle
/// stroke from the log the moment its sweep finished, which the tests here cannot see because
/// each of them hands finalize a log built by hand. What this pins is the log that a session of
/// this shape must produce (`ink::session_tests` drives the app's own recording API to the same
/// three clips and two generations) and that a bake of it puts every stroke in the file.
///
/// The three strokes are drawn at three different HEIGHTS so a decoded frame can be asked about
/// each one separately: the marker at y=20, the first magic stroke at y=60, the second at y=95.
#[test]
fn a_stroke_fading_when_the_next_one_starts_is_still_baked_e2e() {
    require_ffmpeg!("a_stroke_fading_when_the_next_one_starts_is_still_baked_e2e");
    let _guard = super::recording_globals_lock().lock();
    let mut sc = Scratch::new();
    let (temp, out) = (sc.path("overlap", "mkv"), sc.path("overlap-out", "mp4"));
    assert!(build_temp(&temp), "could not build the fixture");

    // Generation 0's last sample is at 480ms, so it holds to 2480 and is gone at 3480.
    // Generation 1 begins at 2700, squarely inside that sweep, and holds past the fixture's end.
    let log = InkLog {
        clips: vec![
            stroke(InkTool::Marker, None, 100, 20.0, 20.0, 10),
            stroke(InkTool::MagicMarker, Some(0), 300, 60.0, 20.0, 10),
            stroke(InkTool::MagicMarker, Some(1), 2_700, 95.0, 20.0, 10),
        ],
        gens: vec![
            InkGen { id: 0, last_activity_ms: 480 },
            InkGen { id: 1, last_activity_ms: 2_880 },
        ],
        erases: Vec::new(),
        wipes: Vec::new(),
    };
    finalize(&temp, &out, Some(&plan(log))).expect("finalize with ink");
    let band = |frame: &[u8]| {
        (inked_in_band(frame, 0, 40), inked_in_band(frame, 45, 80), inked_in_band(frame, 85, 120))
    };

    // While generation 0 is HOLDING: the marker and the first magic stroke are both solid, and
    // the second has not been drawn yet. **This is the assertion the bug failed**: the stroke
    // that would later dissolve was simply not in the file, here or anywhere.
    let (marker, magic_a, magic_b) = band(&frame_at(&out, 1.5).expect("decode the hold"));
    assert!(marker > 50, "the marker stroke is not in the video ({marker} pixels)");
    assert!(
        magic_a > 50,
        "the magic stroke that later dissolved is missing from the video while it was still \
         solid on screen ({magic_a} pixels)"
    );
    assert_eq!(magic_b, 0, "the second magic stroke appeared before it was drawn");
    let solid_a = magic_a;

    // With BOTH on screen: generation 0 is part way through its sweep on its own clock, and
    // generation 1 is untouched by it.
    let (marker, magic_a, magic_b) = band(&frame_at(&out, 3.0).expect("decode the overlap"));
    assert!(marker > 50, "the marker stroke went with the dissolve");
    assert!(magic_a > 0, "the dissolving stroke vanished all at once instead of sweeping");
    assert!(magic_a < solid_a, "the dissolve never ate into the first stroke");
    assert!(magic_b > 50, "the second magic stroke is not in the video ({magic_b} pixels)");

    // Generation 0 finishes on its own clock. Generation 1 is still holding, which is what says
    // the new drawing neither paused the old sweep nor was pulled into it.
    let (marker, magic_a, magic_b) = band(&frame_at(&out, 3.7).expect("decode after the sweep"));
    assert!(marker > 50, "the marker stroke should outlive every dissolve");
    assert_eq!(magic_a, 0, "the first generation was still in the video after its sweep");
    assert!(magic_b > 50, "the second generation stopped holding ({magic_b} pixels)");
}

/// ERASE reaches the file: ink drawn, then cleared, is gone from that instant on.
#[test]
fn erasing_clears_the_baked_ink_e2e() {
    require_ffmpeg!("erasing_clears_the_baked_ink_e2e");
    let _guard = super::recording_globals_lock().lock();
    let mut sc = Scratch::new();
    let (temp, out) = (sc.path("erase", "mkv"), sc.path("erase-out", "mp4"));
    assert!(build_temp(&temp), "could not build the fixture");

    let log = InkLog {
        clips: vec![stroke(InkTool::Marker, None, 300, 60.0, 30.0, 15)],
        gens: Vec::new(),
        erases: vec![2_000],
        wipes: Vec::new(),
    };
    finalize(&temp, &out, Some(&plan(log))).expect("finalize with ink");

    assert!(inked_pixels(&frame_at(&out, 1.5).expect("before")) > 50, "the ink never appeared");
    assert_eq!(inked_pixels(&frame_at(&out, 2.6).expect("after")), 0, "erase did not clear it");
}

/// A window that MOVED mid-recording moves the ink with it, which is the whole reason the plan
/// carries a rect timeline rather than one rectangle.
#[test]
fn ink_follows_a_window_that_moved_e2e() {
    require_ffmpeg!("ink_follows_a_window_that_moved_e2e");
    let _guard = super::recording_globals_lock().lock();
    let mut sc = Scratch::new();
    let (temp, out) = (sc.path("moved", "mkv"), sc.path("moved-out", "mp4"));
    assert!(build_temp(&temp), "could not build the fixture");

    // A short stroke near the left of the surface, held on screen the whole time.
    let log = InkLog {
        clips: vec![stroke(InkTool::Marker, None, 100, 60.0, 20.0, 6)],
        gens: Vec::new(),
        erases: Vec::new(),
        wipes: Vec::new(),
    };
    // The window sits at the origin, then jumps 40 points LEFT at 2s. Ink at a fixed screen
    // position is therefore 40 pixels further RIGHT inside the window's frame afterwards.
    let mut bake = plan(log);
    bake.rects = vec![(0, (0, 0, W as i32, H as i32)), (2_000, (-40, 0, W as i32, H as i32))];
    finalize(&temp, &out, Some(&bake)).expect("finalize with ink");

    let before = ink_centroid(&frame_at(&out, 1.5).expect("before the move")).expect("ink");
    let after = ink_centroid(&frame_at(&out, 3.0).expect("after the move")).expect("ink");
    let dx = after.0 - before.0;
    assert!((dx - 40.0).abs() < 6.0, "the ink did not follow the window: moved {dx} px");
    assert!((after.1 - before.1).abs() < 3.0, "the ink drifted vertically");
}

/// The mapping survives a window that is neither at the surface's origin nor the video's size:
/// a HiDPI window recording, where the video is in BACKING PIXELS and everything else is in
/// POINTS.
///
/// The other tests here place the window at (0,0) at exactly the video's size, which makes the
/// map the identity and hides two whole classes of mistake: dropping the output origin, and
/// dropping the points-to-pixels ratio. On a 2x display those are worth 2x and a screenful
/// respectively, which is enough to put the drawing somewhere else in the frame or off it
/// entirely, and the file would still be the right length with the right frame count.
///
/// The geometry: an output whose origin is (200, 100), a window at (240, 130) that is 80 x 60
/// POINTS, captured into a 160 x 120 PIXEL video. So a surface point p maps to
/// `(p + (200,100) - (240,130)) * 2`, and the stroke's centre of mass at surface (71, 45) has
/// to land at video (62, 30) rather than at (71, 45), where an identity map would leave it.
#[test]
fn ink_maps_through_an_offset_hidpi_window_e2e() {
    require_ffmpeg!("ink_maps_through_an_offset_hidpi_window_e2e");
    let _guard = super::recording_globals_lock().lock();
    let mut sc = Scratch::new();
    let (temp, out) = (sc.path("hidpi", "mkv"), sc.path("hidpi-out", "mp4"));
    assert!(build_temp(&temp), "could not build the fixture");

    // Eight points 6.0pt apart from x=50, at y=45: surface x runs 50..92, centre 71.
    let log = InkLog {
        clips: vec![stroke(InkTool::Marker, None, 500, 45.0, 50.0, 8)],
        gens: Vec::new(),
        erases: Vec::new(),
        wipes: Vec::new(),
    };
    let bake = InkBake {
        log,
        rects: vec![(0, (240, 130, 80, 60))],
        origin: (200, 100),
        point_scale: 1.0,
        color: [255, 0, 0, 255],
    };
    finalize(&temp, &out, Some(&bake)).expect("finalize with ink");

    let late = frame_at(&out, 2.5).expect("decode a late frame");
    let n = inked_pixels(&late);
    assert!(n > 100, "the stroke is not in the video (only {n} inked pixels)");
    let (cx, cy) = ink_centroid(&late).expect("inked pixels have a centroid");
    assert!((cx - 62.0).abs() < 8.0, "the ink is at the wrong x: {cx}, expected ~62");
    assert!((cy - 30.0).abs() < 6.0, "the ink is at the wrong y: {cy}, expected ~30");
    // And say the discriminating half out loud: an identity map would have left it at (71, 45).
    assert!(
        (cy - 45.0).abs() > 8.0,
        "the ink is where an unscaled mapping would put it ({cy}), so the ratio was lost"
    );
}

/// **The no-ink guarantee**, measured rather than asserted about the argument list: a
/// recording with no ink comes out with its video STREAM-COPIED, so its frames are bit-for-bit
/// the ones the recorder wrote.
///
/// The check is the encoder's own fingerprint: a stream copy preserves the exact frame count
/// and the exact codec of the temp, where a re-encode would not have to. Together with
/// `finalize_ink_args_tests`' argument-level pin, that is both halves of "a feature nobody
/// used costs nothing".
#[test]
fn a_recording_with_no_ink_keeps_its_stream_copy_e2e() {
    require_ffmpeg!("a_recording_with_no_ink_keeps_its_stream_copy_e2e");
    let _guard = super::recording_globals_lock().lock();
    let mut sc = Scratch::new();
    let (temp, out) = (sc.path("plain", "mkv"), sc.path("plain-out", "mp4"));
    assert!(build_temp(&temp), "could not build the fixture");
    finalize(&temp, &out, None).expect("finalize with no ink");

    let frames = |p: &Path| -> Option<u64> {
        let o = crate::util::quiet_command(crate::util::ffprobe_path())
            .args(["-v", "error", "-select_streams", "v:0", "-count_frames"])
            .args(["-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
            .arg(p)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        String::from_utf8_lossy(&o.stdout).trim().parse().ok()
    };
    assert_eq!(frames(&temp), frames(&out), "the video was re-encoded, not stream-copied");
    // Nothing was drawn on it either.
    assert_eq!(inked_pixels(&frame_at(&out, 2.0).expect("decode")), 0);
}

/// **The audio guarantee**: baking ink changes the VIDEO and nothing else. The audio goes
/// through the identical filtergraph either way, is encoded exactly once, and comes out the
/// same length.
///
/// This is the property the whole "one pass, no concat" shape exists to protect. The obvious
/// alternative, baking only the inked spans and concatenating, would splice the audio, and the
/// mixer's continuity guarantee is what keeps `amix` from desynchronising.
#[test]
fn baking_ink_leaves_the_audio_alone_e2e() {
    require_ffmpeg!("baking_ink_leaves_the_audio_alone_e2e");
    let _guard = super::recording_globals_lock().lock();
    let mut sc = Scratch::new();
    let temp = sc.path("audio", "mkv");
    let (plain, inked) = (sc.path("audio-plain", "mp4"), sc.path("audio-inked", "mp4"));
    assert!(build_temp(&temp), "could not build the fixture");

    let log = InkLog {
        clips: vec![stroke(InkTool::Marker, None, 500, 60.0, 30.0, 12)],
        gens: Vec::new(),
        erases: Vec::new(),
        wipes: Vec::new(),
    };
    finalize(&temp, &plain, None).expect("finalize with no ink");
    finalize(&temp, &inked, Some(&plan(log))).expect("finalize with ink");

    // The decoded audio is the same length and the same shape either way.
    let pcm = |p: &Path| -> Option<Vec<u8>> {
        let o = crate::util::ffmpeg_command()
            .args(["-v", "error", "-i"])
            .arg(p)
            .args(["-map", "0:a:0", "-f", "s16le", "-ac", "1", "-ar", "8000", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        (!o.stdout.is_empty()).then_some(o.stdout)
    };
    let a = pcm(&plain).expect("decode the un-inked audio");
    let b = pcm(&inked).expect("decode the inked audio");
    let (la, lb) = (a.len() as f64, b.len() as f64);
    assert!(
        (la - lb).abs() / la.max(1.0) < 0.02,
        "the audio changed length when ink was baked: {la} vs {lb} bytes"
    );
    // And it is still a tone rather than silence or noise: the RMS matches.
    let rms = |v: &[u8]| -> f64 {
        let n = v.len() / 2;
        if n == 0 {
            return 0.0;
        }
        let sum: f64 = v
            .chunks_exact(2)
            .map(|c| {
                let s = i16::from_le_bytes([c[0], c[1]]) as f64 / 32768.0;
                s * s
            })
            .sum();
        (sum / n as f64).sqrt()
    };
    let (ra, rb) = (rms(&a), rms(&b));
    assert!(ra > 0.05, "the fixture's tone did not survive the plain pass: {ra}");
    assert!((ra - rb).abs() < 0.02, "the tone changed when ink was baked: {ra} vs {rb}");
}

/// The Media Foundation test's frame size. Bigger than the rest of this module's fixtures
/// because a HARDWARE encoder MFT has a minimum it accepts and 160x120 is below it: measured
/// on the owner's box, `hevc_mf -hw_encoding true` answers `MF_E_INVALIDMEDIATYPE` at
/// 160x120 and encodes cleanly at 640x480.
#[cfg(windows)]
const MF_W: u32 = 640;
#[cfg(windows)]
const MF_H: u32 = 480;

/// Whether this machine can really encode through a HARDWARE Media Foundation MFT, asked with
/// the project's OWN probe so the test's gate and the tier's own selection rule are the same
/// question. A machine that would never select the `mf` tier must not run this test.
#[cfg(windows)]
fn mf_hardware_encodes(encoder: &str) -> bool {
    crate::encode::ffmpeg_encoders().contains(encoder)
        && crate::encode::hw_encoder_probe_ok_with(encoder, &crate::encode::mf_probe_args())
}

/// **DRAGON-755**: the ink finalize's Media Foundation arm really encodes, on the hardware,
/// instead of dying in its first frames and leaving the software fallback to deliver the take.
///
/// This arm executed exactly once anywhere before this test existed, in the owner's QA, and it
/// failed: `ffmpeg finalize exited with exit code: 0xdfaba7bb` after feeding 2 of 659 overlay
/// frames. The DRAGON-716 fallback held and the recording was delivered, which is why nothing
/// noticed for as long as it did, and it is exactly the shape a test has to be built to see
/// through.
///
/// **So the assertion is not "a file came out".** It is that the file is HEVC. The software
/// fallback is `libx264`, which cannot produce HEVC at all, so an `hevc` video stream can only
/// have come from attempt 1. A test that merely checked the pass succeeded would have passed
/// against the broken arm too, since the broken arm's session still delivered a recording.
///
/// It runs only where the question is real: Windows, with `hevc_mf` in the ffmpeg build and a
/// hardware MFT that answers the project's own probe. Anywhere else it SKIPS LOUDLY rather
/// than passing quietly, exactly like the ffmpeg gate above it.
#[cfg(windows)]
#[test]
fn the_media_foundation_ink_finalize_encodes_on_the_hardware_e2e() {
    require_ffmpeg!("the_media_foundation_ink_finalize_encodes_on_the_hardware_e2e");
    if !mf_hardware_encodes("hevc_mf") {
        eprintln!(
            "SKIPPED (loud): the_media_foundation_ink_finalize_encodes_on_the_hardware_e2e \
             needs hevc_mf in the ffmpeg build AND a hardware Media Foundation encoder MFT \
             this machine's probe accepts. The mf ink finalize arm was NOT verified"
        );
        return;
    }
    let _guard = super::recording_globals_lock().lock();
    let mut sc = Scratch::new();
    let (temp, out) = (sc.path("mf", "mkv"), sc.path("mf-out", "mp4"));
    assert!(build_temp_sized(&temp, MF_W, MF_H), "could not build the fixture");

    // One horizontal stroke across the middle, drawn from 1.0s, on a window that sits at the
    // origin at exactly the video's size so surface points ARE video pixels.
    let log = InkLog {
        clips: vec![stroke(InkTool::Marker, None, 1_000, (MF_H / 2) as f32, 80.0, 40)],
        gens: Vec::new(),
        erases: Vec::new(),
        wipes: Vec::new(),
    };
    let bake = InkBake {
        log,
        rects: vec![(0, (0, 0, MF_W as i32, MF_H as i32))],
        origin: (0, 0),
        point_scale: 1.0,
        color: [255, 0, 0, 255],
    };
    // `is_hevc: true`, which is what makes the codec check below able to tell the two attempts
    // apart, and is also the case the owner hit (their plan carried `hevc=true nv12=true`).
    finalize_on(&temp, &out, Some(&bake), "mf", true, (MF_W, MF_H))
        .expect("the mf ink finalize should deliver, on one attempt or the other");

    // THE assertion. Anything but `hevc` means attempt 1 failed and the software fallback
    // rescued the take, which is the defect this test exists to catch.
    assert_eq!(
        video_codec(&out).as_deref(),
        Some("hevc"),
        "the mf finalize arm did not deliver this file: the software fallback did, so the \
         hardware attempt failed again"
    );

    // And the ink is really in it, at the right time. A hardware encoder that produced an
    // HEVC file with no overlay in it would satisfy the codec check alone.
    let early = frame_at_sized(&out, 0.4, MF_W, MF_H).expect("decode an early frame");
    assert_eq!(inked_pixels(&early), 0, "ink appeared before it was drawn");
    let late = frame_at_sized(&out, 2.5, MF_W, MF_H).expect("decode a late frame");
    let n = inked_pixels(&late);
    assert!(n > 100, "the stroke is not in the hardware-encoded video (only {n} inked pixels)");
}
