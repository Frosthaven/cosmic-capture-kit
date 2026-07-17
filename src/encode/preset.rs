//! Encoder catalogue and preset policy: the per-family speed-preset ladders, the
//! codec choices, their friendly labels and defaults, the user's [`Presets`]
//! selection, and the validators that keep a bad persisted value off the ffmpeg
//! command line.

/// NVENC presets, fastest → best quality (ffmpeg `-preset` values); same `p1`–`p7`
/// ladder OBS exposes. `DEFAULT_NVENC_PRESET` (`p4`) keeps the original behaviour.
pub const NVENC_PRESETS: [&str; 7] = ["p1", "p2", "p3", "p4", "p5", "p6", "p7"];
/// Friendly labels (index-aligned with [`NVENC_PRESETS`]) for the settings dropdown.
pub const NVENC_PRESET_LABELS: [&str; 7] = [
    "P1 (fastest)", "P2", "P3", "P4 (default)", "P5", "P6", "P7 (best quality)",
];
pub const DEFAULT_NVENC_PRESET: &str = "p4";

/// libx264 presets, fastest → best quality (the full `-preset` ladder, like OBS).
/// `DEFAULT_X264_PRESET` is `fast` (the middle of the ladder).
pub const X264_PRESETS: [&str; 9] = [
    "ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow", "slower", "veryslow",
];
pub const X264_PRESET_LABELS: [&str; 9] = [
    "ultrafast (fastest)", "superfast", "veryfast", "faster", "fast (default)", "medium", "slow",
    "slower", "veryslow (best quality)",
];
pub const DEFAULT_X264_PRESET: &str = "fast";

/// VAAPI speed/quality ladder. Unlike `-quality` (which AMD radeonsi silently
/// ignores), `-compression_level` *does* take effect — higher = more encode effort =
/// slower but better (measured ~199 fps at level 0 vs ~81 at level 4+ on an AMD iGPU
/// at 1440p). `-1` means "don't set it" (the driver's own default), which is the
/// default and reproduces prior behaviour. Index-aligned value/label lists.
pub const VAAPI_CL_VALUES: [i32; 8] = [-1, 0, 1, 2, 3, 4, 5, 6];
pub const VAAPI_CL_LABELS: [&str; 8] = [
    "Driver default", "0 (fastest)", "1", "2", "3 (default)", "4", "5", "6 (best quality)",
];
pub const DEFAULT_VAAPI_CL: i32 = 3;

/// Video codec choice. `auto` keeps the resolution-driven pick (H.264 ≤ 4096 px,
/// HEVC above — for VAAPI, HEVC when available); `h264`/`hevc` force one. H.264 is
/// the maximum-compatibility choice (plays everywhere); HEVC makes smaller files /
/// sharper text at the same bitrate but isn't as universally playable.
pub const CODEC_VALUES: [&str; 3] = ["auto", "h264", "hevc"];
pub const CODEC_LABELS: [&str; 3] = [
    "Auto (by resolution)",
    "H.264 (max compatibility)",
    "HEVC (smaller files)",
];
pub const DEFAULT_CODEC: &str = "auto";

/// The user's chosen encoder configuration: a speed preset per encoder family (each
/// has its own namespace) plus the video codec. The active plan picks the relevant
/// preset and honours the codec.
#[derive(Clone)]
pub struct Presets {
    pub nvenc: String,
    pub x264: String,
    /// VAAPI `-compression_level`; `-1` = leave at the driver default.
    pub vaapi_cl: i32,
    /// `auto` | `h264` | `hevc` (see [`CODEC_VALUES`]).
    pub codec: String,
}

impl Default for Presets {
    fn default() -> Self {
        Presets {
            nvenc: DEFAULT_NVENC_PRESET.to_string(),
            x264: DEFAULT_X264_PRESET.to_string(),
            vaapi_cl: DEFAULT_VAAPI_CL,
            codec: DEFAULT_CODEC.to_string(),
        }
    }
}

/// Validate a persisted/CLI NVENC preset against the known ladder, falling back to
/// the default so a bad value can never reach ffmpeg.
pub(crate) fn valid_nvenc_preset(p: &str) -> &str {
    if NVENC_PRESETS.contains(&p) {
        p
    } else {
        DEFAULT_NVENC_PRESET
    }
}

pub(crate) fn valid_x264_preset(p: &str) -> &str {
    if X264_PRESETS.contains(&p) {
        p
    } else {
        DEFAULT_X264_PRESET
    }
}

/// Validate a persisted/CLI VAAPI compression level against the known ladder, falling
/// back to the default so a bad value can never reach ffmpeg unclamped. Unlike the two
/// string presets above, `vaapi_plan` only ever checked `>= 0` before putting the value
/// on the command line — no upper bound — so an out-of-ladder value (e.g. a persisted
/// `7`, which the settings-load clamp of `-1..=7` lets through even though the ladder
/// stops at `6`) would reach ffmpeg as-is.
pub(crate) fn valid_vaapi_cl(cl: i32) -> i32 {
    if VAAPI_CL_VALUES.contains(&cl) {
        cl
    } else {
        DEFAULT_VAAPI_CL
    }
}

/// Whether ffmpeg has the software HEVC encoder (libx265). Cached — used by the UI.
pub fn software_supports_hevc() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| super::ffmpeg_encoders().contains("libx265"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("p1")]
    #[case("p4")]
    #[case("p7")]
    fn valid_nvenc_preset_accepts_known_ladder(#[case] p: &str) {
        assert_eq!(valid_nvenc_preset(p), p);
    }

    #[rstest]
    #[case("")]
    #[case("fast")] // an x264 preset, not an NVENC one
    #[case("p0")]
    #[case("p8")]
    #[case("garbage")]
    #[case("P4")] // case-sensitive: uppercase is not in the ladder
    fn valid_nvenc_preset_rejects_garbage_to_default(#[case] p: &str) {
        assert_eq!(valid_nvenc_preset(p), DEFAULT_NVENC_PRESET);
    }

    #[rstest]
    #[case("ultrafast")]
    #[case("fast")]
    #[case("medium")]
    #[case("veryslow")]
    fn valid_x264_preset_accepts_known_ladder(#[case] p: &str) {
        assert_eq!(valid_x264_preset(p), p);
    }

    #[rstest]
    #[case("")]
    #[case("p4")] // an NVENC preset, not an x264 one
    #[case("turbo")]
    #[case("Fast")] // case-sensitive
    fn valid_x264_preset_rejects_garbage_to_default(#[case] p: &str) {
        assert_eq!(valid_x264_preset(p), DEFAULT_X264_PRESET);
    }

    #[rstest]
    #[case(-1)] // "driver default" sentinel
    #[case(0)]
    #[case(3)]
    #[case(6)] // top of the ladder
    fn valid_vaapi_cl_accepts_known_ladder(#[case] cl: i32) {
        assert_eq!(valid_vaapi_cl(cl), cl);
    }

    #[rstest]
    #[case(7)] // one past the ladder's top — the settings-load clamp (-1..=7) lets this
               // through even though it isn't a real level; this is the case that matters.
    #[case(-2)]
    #[case(100)]
    #[case(i32::MIN)]
    fn valid_vaapi_cl_rejects_out_of_ladder_to_default(#[case] cl: i32) {
        assert_eq!(valid_vaapi_cl(cl), DEFAULT_VAAPI_CL);
    }

    #[test]
    fn defaults_are_themselves_valid() {
        assert_eq!(valid_nvenc_preset(DEFAULT_NVENC_PRESET), DEFAULT_NVENC_PRESET);
        assert_eq!(valid_x264_preset(DEFAULT_X264_PRESET), DEFAULT_X264_PRESET);
        assert_eq!(valid_vaapi_cl(DEFAULT_VAAPI_CL), DEFAULT_VAAPI_CL);
    }
}
