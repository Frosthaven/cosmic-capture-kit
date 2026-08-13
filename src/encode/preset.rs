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
///
/// HEVC is a HARDWARE-ONLY option (DRAGON-674). The software tier encodes H.264
/// whatever this value says, and the settings row only OFFERS HEVC while the tier a
/// recording would actually run on has a working hardware HEVC encoder. See
/// [`hevc_offerable`] for the rule and why it is written that way.
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

// DRAGON-674 removed `software_supports_hevc()`, which answered
// `ffmpeg_encoders().contains("libx265")`. That is "is libx265 in this build", and the
// question the UI and the resolver were both asking it is "can this machine RECORD in
// HEVC". libx265 is in essentially every ffmpeg build and cannot hold 1080p in real
// time in any of them, so the old answer was true exactly when it should have been
// false. Do not reintroduce it: the software tier's HEVC answer is a constant `false`
// (see [`hevc_encoder_for`]), and no probe can change that.

/// The ffmpeg HEVC encoder belonging to an encoder tier id, or `None` for a tier with
/// no HARDWARE HEVC encoder behind it.
///
/// `software` is deliberately absent even though ffmpeg ships `libx265`: the software
/// tier's HEVC answer is a constant no, for the reason [`hevc_offerable`] records. An
/// id that is not a tier at all (a hand-edited config, a retired encoder) answers
/// `None` the same way, so an unrecognised value can never be treated as capable.
///
/// Pure, unit-tested.
pub fn hevc_encoder_for(tier: &str) -> Option<&'static str> {
    match tier {
        "nvenc" => Some("hevc_nvenc"),
        "amf" => Some("hevc_amf"),
        "qsv" => Some("hevc_qsv"),
        "vaapi" => Some("hevc_vaapi"),
        "videotoolbox" => Some("hevc_videotoolbox"),
        _ => None,
    }
}

/// Whether the video-codec setting may OFFER HEVC, given `tier` (the encoder a
/// recording would actually run on right now) and `hw_hevc_ok`, that tier's probed
/// hardware-HEVC answer: `Some(true)` it works here, `Some(false)` it does not,
/// `None` nobody has asked yet.
///
/// The rule is "offer only what we can deliver" (DRAGON-674). Offering a codec we then
/// silently swap is dishonest; offering one that KILLS the recording is worse, and that
/// is what the old gate did. Measured in the field (DRAGON-671): a tester's NVENC probe
/// failed, so the plan fell to software while her codec setting still said HEVC. The
/// session logged `chosen=software … hevc=true`, then `encoder backlog - 660 frames
/// dropped`, then the DRAGON-118 muxer watchdog correctly killed a recording that had
/// written nothing for 12s. The settings row had gone on offering HEVC throughout,
/// because the only thing it checked was whether libx265 was in the ffmpeg build.
///
/// Two deliberate decisions live here, and both are load-bearing:
///
/// - **Software is never offerable**, whatever `hw_hevc_ok` says. Real-time screen
///   capture is the only thing this setting configures, and libx265 cannot sustain
///   1080p at capture frame rates on a CPU that is also running the machine. There is
///   no probe that would make this true, so it is not probed.
/// - **Unknown means OFFER, not hide.** "The probe has not run yet" is not "the probe
///   failed", and collapsing the two would quietly strip HEVC from perfectly capable
///   machines — a capability regression that reads to the user exactly like a bug,
///   with no message to explain it. Hiding a working option is the more expensive
///   mistake here, because the failure this gate exists to stop needs a CONFIRMED
///   `Some(false)` to happen at all. In practice the settings page resolves the probe
///   before it builds the row (`pages/video.rs` waits on `encoders_ready`), so `None`
///   is the rare case of a tier absent from the probed list, not the normal path.
///
/// The gate is DYNAMIC on purpose. A failing hardware HEVC encoder is a state of the
/// machine (a driver mid-update, an ffmpeg build missing the encoder, every NVENC
/// session held by a game), never a property of the platform, so nothing here is a
/// per-OS ban: the moment the tier's probe passes again, HEVC comes back with no code
/// change and no migration.
///
/// Pure, unit-tested.
pub fn hevc_offerable(tier: &str, hw_hevc_ok: Option<bool>) -> bool {
    if hevc_encoder_for(tier).is_none() {
        return false;
    }
    hw_hevc_ok.unwrap_or(true)
}

/// Whether `tier` can ACTUALLY encode HEVC on this machine: its HEVC encoder is in the
/// ffmpeg build and, on Windows, a real probe-encode initialises it (the same honest
/// gate the H.264 tier check uses there — Windows has no `/dev` node to sniff).
///
/// `encoders` is the already-fetched `ffmpeg -encoders` text rather than something this
/// re-reads: `ffmpeg_encoders()` is an UNCACHED spawn and the caller enumerating the
/// tiers is holding its output already, so injecting it keeps the cost at one spawn per
/// enumeration instead of one per question. The DECISION this feeds is the pure,
/// unit-tested pair above ([`hevc_encoder_for`] + [`hevc_offerable`]); all that is left
/// here is the effectful lookup, which is why this lives on the syscall side of the seam.
///
/// The caller must already have established that `tier` itself is usable (device node,
/// driver state, H.264 probe). This answers only the narrower HEVC question on top, and
/// off Windows that question IS the ffmpeg build: NVENC/VAAPI/VideoToolbox gate their
/// H.264 side the same way, with no probe-encode to run.
pub(crate) fn hardware_hevc_ok_with(encoders: &str, tier: &str) -> bool {
    let Some(name) = hevc_encoder_for(tier) else {
        return false;
    };
    if !encoders.contains(name) {
        return false;
    }
    #[cfg(windows)]
    {
        super::plan::hw_encoder_probe_ok(name)
    }
    #[cfg(not(windows))]
    {
        true
    }
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

// DRAGON-674: "offer only what we can deliver", pinned as its own island. The probe is
// injected as a plain `Option<bool>`, so every arm — including the not-yet-known one —
// is provable on any host even though the probes themselves are per-platform.
#[cfg(test)]
mod hevc_offer_tests {
    use super::{hevc_encoder_for, hevc_offerable};

    /// Every hardware tier this build can resolve to names its own HEVC encoder, and
    /// nothing else does. A missing row here is a tier that would silently lose HEVC.
    #[test]
    fn each_hardware_tier_names_its_hevc_encoder() {
        for (tier, encoder) in [
            ("nvenc", "hevc_nvenc"),
            ("amf", "hevc_amf"),
            ("qsv", "hevc_qsv"),
            ("vaapi", "hevc_vaapi"),
            ("videotoolbox", "hevc_videotoolbox"),
        ] {
            assert_eq!(hevc_encoder_for(tier), Some(encoder), "{tier}");
        }
    }

    #[test]
    fn software_and_foreign_ids_have_no_hevc_encoder() {
        assert_eq!(hevc_encoder_for("software"), None);
        assert_eq!(hevc_encoder_for(""), None);
        assert_eq!(hevc_encoder_for("x265"), None);
    }

    /// The DRAGON-671 field failure, in one assertion: libx265 IS in the ffmpeg build
    /// (so the old `software_supports_hevc()` gate said yes), and the answer is still
    /// no, because "the encoder exists" was never the question.
    #[test]
    fn software_never_offers_hevc_however_the_probe_answers() {
        for probe in [Some(true), Some(false), None] {
            assert!(!hevc_offerable("software", probe), "probe={probe:?}");
        }
    }

    #[test]
    fn a_hardware_tier_with_a_working_probe_offers_hevc() {
        // The whole point of the ticket is that this stays true: HEVC is worth having
        // wherever it can actually be recorded, and mac VideoToolbox most of all.
        for tier in ["nvenc", "amf", "qsv", "vaapi", "videotoolbox"] {
            assert!(hevc_offerable(tier, Some(true)), "{tier}");
        }
    }

    #[test]
    fn a_hardware_tier_with_a_failing_probe_hides_hevc() {
        for tier in ["nvenc", "amf", "qsv", "vaapi", "videotoolbox"] {
            assert!(!hevc_offerable(tier, Some(false)), "{tier}");
        }
    }

    /// UNKNOWN is not FAILED. A probe that has not answered must not strip HEVC from a
    /// capable machine: that is a silent capability regression with no message
    /// attached, and the failure this gate exists to prevent needs a confirmed
    /// `Some(false)` to occur at all.
    #[test]
    fn an_unresolved_probe_still_offers_hevc() {
        for tier in ["nvenc", "amf", "qsv", "vaapi", "videotoolbox"] {
            assert!(hevc_offerable(tier, None), "{tier}");
        }
    }

    /// The gate is a state of the machine, never a platform ban: the SAME tier flips
    /// back to offerable the moment its probe passes, with no code change.
    #[test]
    fn a_recovered_probe_brings_hevc_straight_back() {
        assert!(!hevc_offerable("nvenc", Some(false)));
        assert!(hevc_offerable("nvenc", Some(true)));
    }

    #[test]
    fn an_unrecognised_tier_is_never_offered_hevc() {
        // A hand-edited or foreign encoder id cannot be assumed capable.
        for probe in [Some(true), Some(false), None] {
            assert!(!hevc_offerable("wxyz", probe), "probe={probe:?}");
            assert!(!hevc_offerable("", probe), "probe={probe:?}");
        }
    }
}
