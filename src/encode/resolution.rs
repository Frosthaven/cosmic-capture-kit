//! Resolution fitting: clamp every capture within the encoder's hard max side and
//! the chosen codec's side limit, aspect-preserved with even dimensions.

/// NVENC's max encodable side (HEVC/AV1 on Turing+ GPUs). We cap EVERY encoder's
/// input to this, rescaling an oversized capture down (aspect preserved), so an
/// extreme multi-monitor span can never error the encoder whatever backend we use.
pub const ENCODER_MAX_SIDE: u32 = 8192;

/// The largest side a codec choice allows, so the capture is downscaled to keep the
/// chosen codec rather than silently switching: H.264 tops out at 4096, HEVC (and
/// `auto`) at the encoder hard max.
pub fn codec_max_side(codec: &str) -> u32 {
    match codec {
        "h264" => 4096,
        _ => ENCODER_MAX_SIDE,
    }
}

/// The recording's max-resolution box (`(max_w, max_h)`, `0` = no user limit on that
/// axis) tightened by the chosen codec's side limit — so forcing H.264 downscales an
/// over-4096 capture to keep H.264 instead of switching codec. Feed the result to
/// [`fit_within`].
pub fn codec_capped_resolution(max_res: (u32, u32), codec: &str) -> (u32, u32) {
    let cap = codec_max_side(codec);
    let ax = |u: u32| if u == 0 { cap } else { u.min(cap) };
    (ax(max_res.0), ax(max_res.1))
}

/// The largest frame side the SOFTWARE (libx264) encoder can encode in real time at
/// `fps` on a typical machine (DRAGON-162). At 30 fps, x264 `-preset fast` sustains
/// full-motion 3200×1800 with headroom on an M1 Pro but only ~0.5× real time at
/// 5120×2880 and ~0.9× at 3840×2160, so a 5K/4K display recorded through the software
/// path backpressures the pipe and the CFR re-feed produces frozen/stuttering video.
/// The sustainable pixel throughput is roughly constant, so the side cap scales with
/// `sqrt(30/fps)`: 3200 at 30 fps, ~2263 at 60 fps. A hardware encoder (VideoToolbox
/// HEVC) has no such limit and is NOT capped here — only the CPU fallback is.
pub const SOFTWARE_REALTIME_BASE_SIDE: u32 = 3200;

/// The software-encoder real-time side cap at `fps` (see [`SOFTWARE_REALTIME_BASE_SIDE`]),
/// never below a sane floor so a very high fps can't shrink the capture to nothing.
pub fn software_realtime_max_side(fps: u32) -> u32 {
    let fps = fps.max(1) as f64;
    let scaled = (SOFTWARE_REALTIME_BASE_SIDE as f64) * (30.0 / fps).sqrt();
    (scaled.round() as u32).clamp(1280, ENCODER_MAX_SIDE)
}

/// The effective max-resolution box for a recording, tightened by BOTH the chosen
/// codec's side limit AND — when the chosen `encoder` is the CPU software fallback —
/// the software real-time side cap for `fps` (DRAGON-162). The software cap keeps a
/// 5K/4K capture from freezing on the CPU path by downscaling it to a size x264 can
/// actually sustain at the configured frame rate; hardware encoders are unaffected.
/// Feed the result to [`fit_within`].
pub fn encoder_capped_resolution(
    max_res: (u32, u32),
    codec: &str,
    encoder: &str,
    fps: u32,
) -> (u32, u32) {
    let (cw, ch) = codec_capped_resolution(max_res, codec);
    if encoder == "software" {
        let s = software_realtime_max_side(fps);
        (cw.min(s), ch.min(s))
    } else {
        (cw, ch)
    }
}

/// Fit `w`×`h` within `ENCODER_MAX_SIDE` on both axes, preserving aspect ratio and
/// keeping even dimensions (codecs need even). Returns the input unchanged when it
/// already fits.
pub fn fit_within_encoder_max(w: u32, h: u32) -> (u32, u32) {
    fit_within(w, h, 0, 0)
}

/// The FINAL encode dimensions for a capture of physical footprint `cap_w`×`cap_h` under
/// the user's `max_res` box, `codec`, chosen `encoder`, and `fps` — the single source of
/// truth every stage must agree on (DRAGON-168). It composes the two existing helpers:
/// [`encoder_capped_resolution`] (user max-res ∩ codec side limit ∩, for the software
/// path, the real-time side cap) then [`fit_within`] (aspect-preserved, even-aligned, no
/// upscale). On macOS this is what the SCK stream is configured to DELIVER (GPU scaling),
/// so the pipe carries frames already at the encode size instead of the full capture —
/// no per-frame CPU resize and no full-res rawvideo transport. `encoder` is the resolved
/// encoder id (`software` / a hardware id), NOT the user's `preferred` string.
pub fn encode_dims(
    cap_w: u32,
    cap_h: u32,
    max_res: (u32, u32),
    codec: &str,
    encoder: &str,
    fps: u32,
) -> (u32, u32) {
    let (mw, mh) = encoder_capped_resolution(max_res, codec, encoder, fps.max(1));
    fit_within(cap_w, cap_h, mw, mh)
}

/// Fit `w`×`h` within a `max_w`×`max_h` box (0 on an axis = no user limit, just the
/// hard encoder max), preserving aspect ratio, never upscaling, even dimensions.
pub fn fit_within(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let cap = |user: u32| {
        if user == 0 {
            ENCODER_MAX_SIDE
        } else {
            user.min(ENCODER_MAX_SIDE)
        }
    };
    let (mw, mh) = (cap(max_w), cap(max_h));
    if w == 0 || h == 0 {
        return (w, h);
    }
    // Scale so both axes fit; never upscale.
    let scale = (mw as f64 / w as f64)
        .min(mh as f64 / h as f64)
        .min(1.0);
    if scale >= 1.0 {
        return (w, h);
    }
    let even = |v: f64| (v.round() as u32).max(2) & !1;
    (even(w as f64 * scale), even(h as f64 * scale))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(1920, 1080, 0, 0, (1920, 1080))] // already within the encoder max -> unchanged
    #[case(640, 480, 1920, 1080, (640, 480))] // smaller than the box -> never upscaled
    #[case(1920, 1080, 1280, 0, (1280, 720))] // width-capped, aspect preserved
    #[case(10_000, 5_000, 0, 0, (8192, 4096))] // clamped to ENCODER_MAX_SIDE on the long axis
    fn fit_within_cases(
        #[case] w: u32,
        #[case] h: u32,
        #[case] max_w: u32,
        #[case] max_h: u32,
        #[case] want: (u32, u32),
    ) {
        assert_eq!(fit_within(w, h, max_w, max_h), want);
    }

    #[test]
    fn fit_within_keeps_even_dims_when_scaling() {
        let (w, h) = fit_within(1921, 1083, 1000, 0);
        assert_eq!(w % 2, 0, "width must be even for the codec");
        assert_eq!(h % 2, 0, "height must be even for the codec");
        assert!(w <= 1000);
    }

    #[test]
    fn codec_cap_forces_h264_under_its_side_limit() {
        assert_eq!(codec_capped_resolution((0, 0), "h264"), (4096, 4096));
        assert_eq!(codec_capped_resolution((10_000, 10_000), "h264"), (4096, 4096));
        assert_eq!(codec_capped_resolution((1920, 1080), "h264"), (1920, 1080)); // already under
        assert_eq!(codec_capped_resolution((0, 0), "hevc"), (ENCODER_MAX_SIDE, ENCODER_MAX_SIDE));
    }

    #[test]
    fn software_realtime_side_shrinks_with_fps_and_has_a_floor() {
        // 30 fps -> the base; higher fps -> smaller (sqrt scaling); never below the floor.
        assert_eq!(software_realtime_max_side(30), SOFTWARE_REALTIME_BASE_SIDE);
        assert!(software_realtime_max_side(60) < SOFTWARE_REALTIME_BASE_SIDE);
        assert!(software_realtime_max_side(60) > 2000); // ~2263 at 60 fps
        assert_eq!(software_realtime_max_side(1000), 1280); // clamped to the floor
        assert_eq!(software_realtime_max_side(0), software_realtime_max_side(1)); // fps 0 == 1
    }

    #[test]
    fn encode_dims_agrees_across_the_pipeline_for_display_2() {
        // DRAGON-168: the Studio Display in scaled mode delivers 6400x3600 backing px.
        // Software @60fps caps the side to ~2263 (sqrt(30/60)*3200), aspect-preserved.
        let (w, h) = encode_dims(6400, 3600, (0, 0), "auto", "software", 60);
        assert!(w <= 2263 && h <= 2263, "software 60fps caps the long side: {w}x{h}");
        assert_eq!(w % 2, 0, "even width for the codec");
        assert_eq!(h % 2, 0, "even height for the codec");
        // 16:9 preserved.
        assert!((w as f64 / h as f64 - 16.0 / 9.0).abs() < 0.02, "aspect preserved: {w}x{h}");
        // Software @30fps: the 3200 side cap on the long axis -> 3200x1800.
        assert_eq!(encode_dims(6400, 3600, (0, 0), "auto", "software", 30), (3200, 1800));
        // VideoToolbox (hardware) is NOT real-time-capped: full 6400x3600 stays.
        assert_eq!(encode_dims(6400, 3600, (0, 0), "auto", "videotoolbox", 30), (6400, 3600));
    }

    #[test]
    fn encode_dims_honors_a_user_max_res_setting() {
        // Directive: the persisted max-res box is honored end-to-end. A 2560 cap on the
        // 6400x3600 display -> 2560x1440 even for the hardware encoder (no upscale, aspect
        // kept), and it wins over the software real-time cap when it's the tighter bound.
        assert_eq!(encode_dims(6400, 3600, (2560, 2560), "auto", "videotoolbox", 30), (2560, 1440));
        // Software @30 caps at 3200; a 2560 user box is tighter -> 2560 wins.
        assert_eq!(encode_dims(6400, 3600, (2560, 2560), "auto", "software", 30), (2560, 1440));
        // A user box LARGER than the software cap doesn't loosen it (software @30 = 3200).
        assert_eq!(encode_dims(6400, 3600, (5120, 5120), "auto", "software", 30), (3200, 1800));
        // Never upscales a small capture to the box.
        assert_eq!(encode_dims(1280, 720, (2560, 2560), "auto", "videotoolbox", 30), (1280, 720));
    }

    #[test]
    fn encoder_cap_shrinks_only_the_software_path() {
        // DRAGON-162: a 5K/4K "Original" (no user cap) capture on the SOFTWARE encoder is
        // pulled down to the sustainable side; a hardware encoder keeps the codec cap.
        let sw = encoder_capped_resolution((0, 0), "auto", "software", 30);
        assert_eq!(sw, (3200, 3200), "software auto @30fps caps to the sustainable side");
        // 60 fps caps tighter still.
        let sw60 = encoder_capped_resolution((0, 0), "auto", "software", 60);
        assert!(sw60.0 < 3200 && sw60.1 < 3200);
        // VideoToolbox / any hardware id is NOT shrunk: auto -> HEVC side limit (8192).
        let hw = encoder_capped_resolution((0, 0), "auto", "videotoolbox", 30);
        assert_eq!(hw, (ENCODER_MAX_SIDE, ENCODER_MAX_SIDE));
        // A user max-res smaller than the software cap wins (never upscales the cap).
        let user = encoder_capped_resolution((1920, 1080), "auto", "software", 30);
        assert_eq!(user, (1920, 1080));
        // Software + the codec cap interact: forced h264 caps at 4096, software at 3200,
        // so the tighter (3200) wins.
        let both = encoder_capped_resolution((0, 0), "h264", "software", 30);
        assert_eq!(both, (3200, 3200));
    }
}
