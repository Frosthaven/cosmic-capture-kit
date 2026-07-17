//! End-to-end A/V-sync measurement for the `--make-sync-clip` / `--calibrate-sync`
//! calibration flow (DRAGON-119).
//!
//! Recordings on COSMIC carry a large "audio leads video" offset caused by
//! video-frame delivery lag the app cannot observe live (the compositor leaves the
//! SPA buffer pts unfilled and pw_delay under-reports), so the in-app
//! auto-calibration is honest but ~10x short of the truth. The fix is a one-time
//! measurement OUTSIDE the pipeline: generate a reference clip whose flash/beep
//! alignment is exact ([`write_sync_clip`]), have the user play + record it, then
//! measure the recording's flash-vs-beep offset ([`measure_av_offset`]).
//!
//! The ffmpeg decodes live in [`video_luma_series`] / [`audio_rms_series`]; all
//! analysis (onset detection, pairing, the median/spread math) is pure and
//! unit-tested below. Sign convention throughout: POSITIVE offset = the flash
//! appears LATER than its beep = audio leads video = the compensation to store in
//! `audio_sync_offset_ms` (which delays audio at finalize).

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

/// Both series are analysed on a 30 Hz grid: video is CFR-resampled to 30 fps and
/// audio RMS is windowed per 1/30 s, so one sample step is ~33 ms either side.
const SAMPLE_FPS: f64 = 30.0;
/// Audio decode rate for the RMS series (mono s16le).
const AUDIO_RATE: usize = 16_000;
/// Fixed analysis frame for the luma series. Mean luma doesn't care about aspect
/// distortion, and a fixed size means the rawvideo frame length is known without
/// probing the source's dimensions.
const LUMA_W: usize = 160;
const LUMA_H: usize = 90;
/// A rising threshold crossing only counts as an onset after this many consecutive
/// below-threshold samples (100 ms at 30 Hz) — so a bright static screen (or a
/// flicker inside a flash) yields nothing.
const DARK_RUN: usize = 3;
/// Max flash↔beep pairing distance. The clip's events are 1.5 s apart, so anything
/// farther than this belongs to a different event (or to none).
const MAX_PAIR_GAP_SECS: f64 = 1.5;
/// A pair-offset spread beyond this marks the measurement low-confidence (the
/// events disagree too much to trust the median).
const MAX_CONFIDENT_SPREAD_SECS: f64 = 0.120;
/// Minimum luma swing (0..255) for the series to contain flashes at all — below
/// this, midpoint "crossings" would just be noise on a static picture.
const MIN_LUMA_RANGE: f32 = 16.0;
/// Minimum RMS swing (0..1) for the series to contain beeps (~ -46 dBFS).
const MIN_RMS_RANGE: f32 = 0.005;

/// The filename `--make-sync-clip` writes by default.
pub(crate) const SYNC_CLIP_NAME: &str = "cck-sync-reference.mp4";

/// One flash/beep offset measurement over a whole recording.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SyncMeasurement {
    /// Median of (flash time − beep time) over the paired events, seconds.
    /// Positive = audio leads video = the value to feed `audio_sync_offset_ms`.
    pub offset_secs: f64,
    /// Max − min of the pair offsets, seconds (how much the events disagree).
    pub spread_secs: f64,
    /// How many flash/beep pairs the median is over.
    pub pairs: usize,
    /// Whether the spread is within [`MAX_CONFIDENT_SPREAD_SECS`]. The caller
    /// decides what a low-confidence measurement is worth.
    pub confident: bool,
}

// ---------------------------------------------------------------------------
// ffmpeg decodes (the impure edge)
// ---------------------------------------------------------------------------

/// Mean luma per frame of `path`'s video, CFR-resampled to `fps_hint` (falling
/// back to 30 when the hint is unusable). Times are anchored to the video
/// stream's container start time, so a stream that starts late keeps its offset.
pub(crate) fn video_luma_series(path: &Path, fps_hint: f64) -> Option<Vec<(f64, f32)>> {
    let fps = if fps_hint.is_finite() && fps_hint > 0.0 { fps_hint } else { SAMPLE_FPS };
    let vf = format!("scale={LUMA_W}:{LUMA_H},fps={fps},format=gray");
    let mut cmd = Command::new(crate::util::ffmpeg_path());
    cmd.args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vf", &vf, "-f", "rawvideo", "pipe:1"]);
    let raw = run_pipe(cmd)?;
    let frame = LUMA_W * LUMA_H;
    if raw.len() < frame {
        return None;
    }
    let t0 = stream_start(path, "v:0");
    Some(
        raw.chunks_exact(frame)
            .enumerate()
            .map(|(n, px)| {
                let sum: u64 = px.iter().map(|&b| b as u64).sum();
                (t0 + n as f64 / fps, sum as f32 / frame as f32)
            })
            .collect(),
    )
}

/// RMS per 1/30 s window of `path`'s first audio stream (decoded mono s16le at
/// 16 kHz). Times are anchored to the audio stream's container start time —
/// that's what keeps a time-shifted stream's offset measurable (a raw sample
/// pipe otherwise starts at the first sample, silently dropping it).
pub(crate) fn audio_rms_series(path: &Path) -> Option<Vec<(f64, f32)>> {
    let mut cmd = Command::new(crate::util::ffmpeg_path());
    cmd.args(["-v", "error", "-i"])
        .arg(path)
        // a:0, not a: a raw sample pipe can only carry ONE stream.
        .args(["-map", "a:0", "-ac", "1", "-ar", "16000", "-f", "s16le", "pipe:1"]);
    let raw = run_pipe(cmd)?;
    let win = (AUDIO_RATE as f64 / SAMPLE_FPS) as usize; // 533 samples
    let (sample_bytes, _) = raw.as_chunks::<2>();
    let samples: Vec<i16> = sample_bytes.iter().map(|b| i16::from_le_bytes(*b)).collect();
    if samples.len() < win {
        return None;
    }
    let t0 = stream_start(path, "a:0");
    Some(
        samples
            .chunks_exact(win)
            .enumerate()
            .map(|(k, w)| {
                let energy: f64 = w
                    .iter()
                    .map(|&s| {
                        let f = s as f64 / 32768.0;
                        f * f
                    })
                    .sum();
                // Window time from the SAMPLE index (exact), not k/30 (533 · 30 ≠
                // 16000, so that would drift ~0.6 ms per second).
                (t0 + (k * win) as f64 / AUDIO_RATE as f64, (energy / win as f64).sqrt() as f32)
            })
            .collect(),
    )
}

/// Spawn `cmd` and read its stdout to EOF. `None` when it can't start or decodes
/// nothing; a truncated tail (non-zero exit after real output) is still usable.
fn run_pipe(mut cmd: Command) -> Option<Vec<u8>> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = Vec::new();
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    let _ = child.wait();
    (!out.is_empty()).then_some(out)
}

/// A stream's container start time (seconds) via ffprobe; 0 when absent/unparseable.
fn stream_start(path: &Path, selector: &str) -> f64 {
    let out = Command::new(crate::util::ffprobe_path())
        .args([
            "-v", "error",
            "-select_streams", selector,
            "-show_entries", "stream=start_time",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output();
    let Ok(out) = out else { return 0.0 };
    let s = String::from_utf8_lossy(&out.stdout);
    let t = s.lines().next().and_then(|l| l.trim().parse::<f64>().ok()).unwrap_or(0.0);
    if t.is_finite() { t } else { 0.0 }
}

// ---------------------------------------------------------------------------
// Pure analysis
// ---------------------------------------------------------------------------

/// Times where the video flashes: rising crossings of the luma midpoint, each
/// preceded by a [`DARK_RUN`] of below-threshold samples. The clip is black with
/// white flashes, so threshold = midpoint of the series' own min/max; a bright
/// static screen (no swing, or no dark run) yields nothing.
pub(crate) fn flash_times(series: &[(f64, f32)]) -> Vec<f64> {
    onset_times(series, MIN_LUMA_RANGE)
}

/// Times where the audio beeps: RMS onset crossings, same algorithm as
/// [`flash_times`] on the RMS scale.
pub(crate) fn beep_times(series: &[(f64, f32)]) -> Vec<f64> {
    onset_times(series, MIN_RMS_RANGE)
}

/// Rising crossings of the series' min/max midpoint that follow at least
/// [`DARK_RUN`] below-threshold samples. `min_range` gates out flat/noisy series
/// (a static picture, digital silence) where the midpoint would split noise.
fn onset_times(series: &[(f64, f32)], min_range: f32) -> Vec<f64> {
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for &(_, v) in series {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if series.len() <= DARK_RUN || hi - lo < min_range {
        return Vec::new();
    }
    let thr = lo + (hi - lo) / 2.0;
    let mut out = Vec::new();
    for i in DARK_RUN..series.len() {
        let rising = series[i].1 >= thr && series[i - 1].1 < thr;
        if rising && (1..=DARK_RUN).all(|d| series[i - d].1 < thr) {
            out.push(series[i].0);
        }
    }
    out
}

/// Pair each flash with its nearest beep within ±[`MAX_PAIR_GAP_SECS`] and take
/// the median of (flash − beep). `None` below 2 pairs; a spread past
/// [`MAX_CONFIDENT_SPREAD_SECS`] returns `confident: false` instead of rejecting.
pub(crate) fn pair_offset(flashes: &[f64], beeps: &[f64]) -> Option<SyncMeasurement> {
    let mut offsets: Vec<f64> = Vec::new();
    for &f in flashes {
        let nearest = beeps
            .iter()
            .copied()
            .min_by(|a, b| (f - a).abs().partial_cmp(&(f - b).abs()).unwrap());
        if let Some(b) = nearest
            && (f - b).abs() <= MAX_PAIR_GAP_SECS
        {
            offsets.push(f - b);
        }
    }
    if offsets.len() < 2 {
        return None;
    }
    offsets.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = offsets.len();
    let offset_secs = if n % 2 == 1 {
        offsets[n / 2]
    } else {
        (offsets[n / 2 - 1] + offsets[n / 2]) / 2.0
    };
    let spread_secs = offsets[n - 1] - offsets[0];
    Some(SyncMeasurement {
        offset_secs,
        spread_secs,
        pairs: n,
        confident: spread_secs <= MAX_CONFIDENT_SPREAD_SECS,
    })
}

/// Measure a recording of the sync clip: decode both series, find the flashes and
/// beeps, and pair them. Every failure names what's missing so the user can fix
/// the re-recording (not the tool).
pub(crate) fn measure_av_offset(path: &Path) -> Result<SyncMeasurement, String> {
    let luma = video_luma_series(path, SAMPLE_FPS)
        .ok_or("could not decode video; is ffmpeg installed and the file a video?")?;
    let rms = audio_rms_series(path)
        .ok_or("could not decode audio; was system audio ON during the recording?")?;
    let flashes = flash_times(&luma);
    if flashes.is_empty() {
        return Err("no flashes found; is this a recording of the sync clip?".into());
    }
    let beeps = beep_times(&rms);
    if beeps.is_empty() {
        return Err("no beeps found; was system audio audible during the recording?".into());
    }
    pair_offset(&flashes, &beeps).ok_or_else(|| {
        "couldn't pair the flashes with beeps (need at least 2 pairs within 1.5 s); \
         record the full clip from before the first flash to after the last"
            .into()
    })
}

// ---------------------------------------------------------------------------
// Reference clip generation
// ---------------------------------------------------------------------------

/// Write the reference clip: ~6 s of 640x360@30 black with four 3-frame white
/// flashes (t = 1.0, 2.5, 4.0, 5.5) and an 80 ms 1 kHz beep at the SAME instants.
/// The beeps come from `aevalsrc` with the gate inside the expression — evaluated
/// per SAMPLE, so the beep edges are sample-accurate (a `volume=0:enable=` gate
/// only flips per audio frame, ~21 ms of quantisation).
pub(crate) fn write_sync_clip(out: &Path) -> Result<(), String> {
    let video = "color=c=black:s=640x360:r=30:d=6,\
                 drawbox=c=white:t=fill:w=iw:h=ih:\
                 enable='between(t,1,1.1)+between(t,2.5,2.6)+between(t,4,4.1)+between(t,5.5,5.6)'";
    let audio = "aevalsrc='0.8*sin(2*PI*1000*t)*(between(t,1,1.08)+between(t,2.5,2.58)\
                 +between(t,4,4.08)+between(t,5.5,5.58))':s=48000:d=6";
    let status = Command::new(crate::util::ffmpeg_path())
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", video, "-f", "lavfi", "-i", audio])
        .args(["-c:v", "libx264", "-preset", "fast", "-crf", "20", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart", "-shortest"])
        .arg(out)
        .status()
        .map_err(|e| format!("ffmpeg failed to start: {e}"))?;
    if !status.success() {
        return Err(format!("ffmpeg exited with {status} while writing the clip"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Series on the 30 Hz grid from bare values (luma or RMS scale).
    fn series(values: &[f32]) -> Vec<(f64, f32)> {
        values.iter().enumerate().map(|(i, &v)| (i as f64 / 30.0, v)).collect()
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn flash_times_finds_each_rising_edge_after_a_dark_run() {
        // Dark, flash at i=5..7, dark, flash at i=12.
        let mut v = vec![0.0f32; 20];
        for i in [5, 6, 7, 12] {
            v[i] = 255.0;
        }
        let t = flash_times(&series(&v));
        assert_eq!(t.len(), 2);
        assert!(approx(t[0], 5.0 / 30.0));
        assert!(approx(t[1], 12.0 / 30.0));
    }

    #[test]
    fn flash_times_static_bright_screen_yields_nothing() {
        // No swing at all: min == max, range 0 < MIN_LUMA_RANGE.
        assert!(flash_times(&series(&[200.0; 40])).is_empty());
    }

    #[test]
    fn flash_times_low_contrast_noise_yields_nothing() {
        // ±5 luma of noise on a static picture — under the 16-luma range gate.
        let v: Vec<f32> = (0..40).map(|i| 100.0 + if i % 2 == 0 { 5.0 } else { -5.0 }).collect();
        assert!(flash_times(&series(&v)).is_empty());
    }

    #[test]
    fn flash_times_monotonic_ramp_is_a_single_onset() {
        // A slow 0→255 ramp crosses the midpoint exactly once (with a dark run
        // before it), so it reads as ONE flash — documented behaviour.
        let v: Vec<f32> = (0..32).map(|i| i as f32 * 255.0 / 31.0).collect();
        assert_eq!(flash_times(&series(&v)).len(), 1);
    }

    #[test]
    fn flash_times_requires_the_full_dark_run() {
        // A 1-sample dip inside a bright stretch is not a new flash.
        let v = [0.0, 0.0, 0.0, 0.0, 255.0, 255.0, 0.0, 255.0, 255.0];
        let t = flash_times(&series(&v));
        assert_eq!(t.len(), 1, "only the first rise counts");
        assert!(approx(t[0], 4.0 / 30.0));
    }

    #[test]
    fn beep_times_finds_rms_onsets_and_ignores_silence() {
        let mut v = vec![0.001f32; 30];
        for i in [10, 11, 20, 21] {
            v[i] = 0.5;
        }
        let t = beep_times(&series(&v));
        assert_eq!(t.len(), 2);
        assert!(approx(t[0], 10.0 / 30.0));
        assert!(approx(t[1], 20.0 / 30.0));
        // Digital silence alone (swing under MIN_RMS_RANGE) yields nothing.
        assert!(beep_times(&series(&[0.0005; 30])).is_empty());
    }

    #[test]
    fn pair_offset_positive_when_flash_trails_beep() {
        // Flash 100 ms after each beep = audio leads video = POSITIVE.
        let beeps = [1.0, 2.5, 4.0, 5.5];
        let flashes: Vec<f64> = beeps.iter().map(|b| b + 0.1).collect();
        let m = pair_offset(&flashes, &beeps).unwrap();
        assert!(approx(m.offset_secs, 0.1));
        assert!(m.spread_secs < 1e-9);
        assert_eq!(m.pairs, 4);
        assert!(m.confident);
    }

    #[test]
    fn pair_offset_negative_when_flash_leads_beep() {
        let flashes = [1.0, 2.5, 4.0, 5.5];
        let beeps: Vec<f64> = flashes.iter().map(|f| f + 0.3).collect();
        let m = pair_offset(&flashes, &beeps).unwrap();
        assert!(approx(m.offset_secs, -0.3));
        assert!(m.confident);
    }

    #[test]
    fn pair_offset_needs_at_least_two_pairs() {
        assert!(pair_offset(&[1.0], &[1.05]).is_none());
        assert!(pair_offset(&[], &[1.0, 2.5]).is_none());
        // Two flashes but only one within pairing range of any beep.
        assert!(pair_offset(&[1.0, 30.0], &[1.02]).is_none());
    }

    #[test]
    fn pair_offset_ignores_unpairable_events() {
        // A flash with no beep within 1.5 s (and a far stray beep) drops out.
        let m = pair_offset(&[1.0, 2.5, 30.0], &[1.02, 2.52, 90.0]).unwrap();
        assert_eq!(m.pairs, 2);
        assert!(approx(m.offset_secs, -0.02));
    }

    #[test]
    fn pair_offset_wide_spread_is_low_confidence_not_rejected() {
        // Offsets 0.0 and 0.2 → spread 0.2 > 0.120: still measured, not confident.
        let m = pair_offset(&[1.0, 2.7], &[1.0, 2.5]).unwrap();
        assert!(approx(m.spread_secs, 0.2));
        assert!(!m.confident);
        assert!(approx(m.offset_secs, 0.1), "median of the two offsets");
    }

    #[test]
    fn pair_offset_even_pair_count_averages_the_middle_two() {
        let beeps = [1.0, 2.5, 4.0, 5.5];
        let flashes = [1.0, 2.52, 4.04, 5.6];
        let m = pair_offset(&flashes, &beeps).unwrap();
        assert!(approx(m.offset_secs, 0.03), "median of [0, .02, .04, .1]");
        assert!(approx(m.spread_secs, 0.1));
        assert!(m.confident);
    }

    #[test]
    fn onsets_pair_into_the_expected_measurement_end_to_end() {
        // Synthetic clip-shaped series: flashes one 33 ms sample after each beep.
        let mut luma = vec![16.0f32; 200];
        let mut rms = vec![0.0f32; 200];
        for start in [31usize, 76, 121, 166] {
            for d in 0..3 {
                luma[start + d] = 235.0;
            }
        }
        for start in [30usize, 75, 120, 165] {
            for d in 0..3 {
                rms[start + d] = 0.5;
            }
        }
        let m = pair_offset(&flash_times(&series(&luma)), &beep_times(&series(&rms))).unwrap();
        assert_eq!(m.pairs, 4);
        assert!(approx(m.offset_secs, 1.0 / 30.0));
        assert!(m.confident);
    }
}
