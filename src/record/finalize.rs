//! Finalize pass: bake the live per-channel audio toggles into the recorded temp
//! file (amix + per-channel mute timeline) and mux the metadata into `out_path`.
//! Video is stream-copied so A/V sync is preserved exactly.
//!
//! DRAGON-127 retired the legacy wallclock+CFR+segments recorder, and with it the
//! per-channel A/V-sync delay-shift (`FinalizeDelays`, `shift_filter`) this pass
//! used to apply: the media-clock owned pipeline's FIFO output IS the final audio
//! (no wall-clock/segment-offset mapping needed), so finalize is now just
//! intervals → gate filters → amix → mux.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Produce `out_path` from the temp capture, gating each audio channel to the
/// intervals it was toggled on. Video is stream-copied (no re-encode → no sync
/// drift); only the (cheap) audio is re-encoded.
pub(crate) fn finalize_with_intervals(
    temp: &std::path::Path,
    out_path: &std::path::Path,
    mic_off: &[(f64, f64)],
    sys_off: &[(f64, f64)],
    is_hevc: bool,
    metadata: &str,
) -> Result<PathBuf, String> {
    let mic_f = chan_filter(mic_off);
    let sys_f = chan_filter(sys_off);
    let fc = format!(
        "[0:a:0]{mic_f}[m];[0:a:1]{sys_f}[s];\
         [m][s]amix=inputs=2:duration=longest:normalize=0[a]"
    );

    // Copying HEVC into mp4 needs the hvc1 tag for QuickTime/Apple players (h264
    // doesn't). We chose the encoder, so `is_hevc` is known from the plan — no
    // ffprobe needed.
    let mut cmd = Command::new(crate::util::ffmpeg_path());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(temp)
        .args(["-filter_complex", &fc])
        .args(["-map", "0:v:0", "-map", "[a]"])
        .args(["-c:v", "copy", "-c:a", "aac", "-b:a", "192k"]);
    if is_hevc {
        cmd.args(["-tag:v", "hvc1"]);
    }
    // Embed how this was recorded (read back by `--inspect`). The App wrote
    // `codec=auto` before the encode plan existed — annotate the outcome here,
    // where the plan's actual pick is known.
    let metadata = annotate_codec(metadata, is_hevc);
    cmd.args(["-metadata", &format!("comment={metadata}")])
        .args(["-metadata", "title=Cosmic Capture Kit"]);
    cmd.args(["-movflags", "+faststart"])
        .arg(out_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let status = cmd
        .status()
        .map_err(|e| format!("could not start ffmpeg finalize: {e}"))?;
    if status.success() {
        Ok(out_path.to_path_buf())
    } else {
        Err(format!("ffmpeg finalize exited with {status}"))
    }
}

/// One channel's finalize filter chain: gate it to its on-intervals. A no-op chain
/// is `anull` (a valid single filter for the `[0:a:N]…[x]` graph link).
fn chan_filter(off: &[(f64, f64)]) -> String {
    match vol_filter(off) {
        Some(f) => f,
        None => "anull".to_string(),
    }
}

/// The mute-gate stage for a channel given its off-intervals, or `None` (passthrough)
/// when it's always on. Full mute when always off, else a timeline `enable` that mutes
/// the gaps.
fn vol_filter(off: &[(f64, f64)]) -> Option<String> {
    if off.is_empty() {
        None
    } else if off.len() == 1 && off[0].0 <= 0.0 && off[0].1 >= 1.0e9 {
        Some("volume=0".to_string())
    } else {
        let expr = off
            .iter()
            .map(|(s, e)| format!("between(t,{s:.3},{e:.3})"))
            .collect::<Vec<_>>()
            .join("+");
        Some(format!("volume=0:enable='{expr}'"))
    }
}

/// Annotate a metadata line's `codec=auto` with the codec the encode plan
/// actually resolved to (decided only once the first frame fixed the dims) —
/// `codec=auto(H.264)` / `codec=auto(HEVC)`. Explicit codec choices pass
/// through untouched: they already say what they are.
fn annotate_codec(metadata: &str, is_hevc: bool) -> String {
    metadata.replace(
        "codec=auto",
        if is_hevc { "codec=auto(HEVC)" } else { "codec=auto(H.264)" },
    )
}

/// Per-channel "off" intervals from toggle points already mapped to seconds on the
/// audio stream's timeline (a channel is muted during these; elsewhere it passes
/// at full volume; `on_at_start` is the channel's state at t0; a trailing off
/// interval runs open-ended to EOF, encoded as `1e9`). The interval-building core:
/// `record::pump` (DRAGON-125) reuses this exact function to turn its accumulated
/// `TrackGain` automation (already in MEDIA seconds — no wall-clock mapping
/// needed) into the same interval shape, rather than re-deriving the same
/// on/off/trailing-open logic a second time.
pub(crate) fn off_intervals_from_pts(mut pts: Vec<(f64, bool)>, on_at_start: bool) -> Vec<(f64, f64)> {
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut intervals = Vec::new();
    let mut on = on_at_start;
    let mut off_start = if on_at_start { None } else { Some(0.0) };
    for (t, new_on) in pts {
        if new_on && !on {
            if let Some(s) = off_start.take() {
                intervals.push((s, t));
            }
        } else if !new_on && on {
            off_start = Some(t);
        }
        on = new_on;
    }
    if let Some(s) = off_start {
        intervals.push((s, 1.0e9)); // open-ended to EOF
    }
    intervals
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn annotate_codec_resolves_auto_and_leaves_explicit_choices() {
        let meta = "Cosmic Capture Kit | encoder=nvenc | codec=auto | fps=30";
        assert_eq!(
            annotate_codec(meta, false),
            "Cosmic Capture Kit | encoder=nvenc | codec=auto(H.264) | fps=30"
        );
        assert_eq!(
            annotate_codec(meta, true),
            "Cosmic Capture Kit | encoder=nvenc | codec=auto(HEVC) | fps=30"
        );
        // An explicit codec passes through untouched.
        let explicit = "Cosmic Capture Kit | codec=hevc | fps=30";
        assert_eq!(annotate_codec(explicit, true), explicit);
    }

    // ── off_intervals_from_pts: the interval-building core (media seconds) ────
    #[test]
    fn starts_on_with_no_events_is_never_muted() {
        assert_eq!(off_intervals_from_pts(vec![], true), Vec::<(f64, f64)>::new());
    }

    #[test]
    fn starts_off_with_no_events_is_muted_to_eof() {
        assert_eq!(off_intervals_from_pts(vec![], false), vec![(0.0, 1.0e9)]);
    }

    #[test]
    fn off_then_on_yields_one_bounded_interval() {
        let pts = vec![(1.0, false), (3.0, true)];
        assert_eq!(off_intervals_from_pts(pts, true), vec![(1.0, 3.0)]);
    }

    #[test]
    fn trailing_off_is_open_ended_to_eof() {
        let pts = vec![(2.0, false)];
        assert_eq!(off_intervals_from_pts(pts, true), vec![(2.0, 1.0e9)]);
    }

    #[test]
    fn starts_off_on_off_on_yields_two_intervals() {
        let pts = vec![(1.0, true), (2.0, false), (3.0, true)];
        assert_eq!(off_intervals_from_pts(pts, false), vec![(0.0, 1.0), (2.0, 3.0)]);
    }

    #[test]
    fn redundant_off_while_already_off_is_ignored() {
        // Already off at start; an "off" toggle changes nothing.
        let pts = vec![(1.0, false)];
        assert_eq!(off_intervals_from_pts(pts, false), vec![(0.0, 1.0e9)]);
    }

    #[test]
    fn out_of_order_events_are_sorted_by_time() {
        // Same toggles as `off_then_on` but supplied out of order.
        let pts = vec![(3.0, true), (1.0, false)];
        assert_eq!(off_intervals_from_pts(pts, true), vec![(1.0, 3.0)]);
    }

    // ── chan_filter: gate-only filtergraph shapes (no delay-shift any more) ───
    #[rstest]
    #[case(&[], "anull")]
    #[case(&[(0.0, 1.0e9)], "volume=0")]
    #[case(&[(1.0, 3.0)], "volume=0:enable='between(t,1.000,3.000)'")]
    fn chan_filter_shapes(#[case] off: &[(f64, f64)], #[case] want: &str) {
        assert_eq!(chan_filter(off), want);
    }
}
