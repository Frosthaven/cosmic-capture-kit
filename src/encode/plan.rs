//! Encode plans: the per-backend ffmpeg argument recipe ([`EncodePlan`]) plus the
//! builders that probe the machine (NVENC / VAAPI / software) and choose one. The
//! command module turns a plan into the actual ffmpeg invocation.

use std::process::{Command, Stdio};

use super::*;

/// How to encode the temp capture: pre-input args, output/codec args, any extra
/// env (e.g. `LIBVA_DRIVER_NAME` for VAAPI), and whether we feed NV12 (our own
/// threaded BT.709 conversion — the hardware path) or RGBA (software fallback,
/// where ffmpeg does the conversion).
pub struct EncodePlan {
    pub(crate) pre: Vec<String>,
    pub(crate) codec: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    /// Video filter applied via `-filter_complex [0:v]…[v]` then mapped (used by
    /// VAAPI's `hwupload`); `None` maps the input video directly.
    pub(crate) vf: Option<String>,
    /// Tag the stream BT.709 (NVENC accepts it; VAAPI rejects it, so it's off there
    /// and the driver tags the stream itself).
    pub(crate) color_tags: bool,
    pub(crate) nv12: bool,
    /// Quality-based rate control (CQ/CRF set in `codec`): the bitrate setting is then
    /// a peak CAP, not a target — small files for static content, good quality, no
    /// chroma crush. `false` = classic bitrate target (VAAPI).
    pub(crate) quality_rc: bool,
}

impl EncodePlan {
    /// Whether this plan encodes HEVC (its `-c:v` is an `hevc_*` encoder). The
    /// finalize remux uses this to re-apply the `hvc1` mp4 tag without re-probing.
    pub fn is_hevc(&self) -> bool {
        self.codec.iter().any(|a| a.contains("hevc") || a.contains("265"))
    }

    /// Build a plan for a SPECIFIC backend (`gpu`/`nvenc`, `cpu`/`vaapi`, `sw`) for
    /// benchmarking. `None` if that backend isn't available here.
    pub fn for_backend(backend: &str, w: u32, h: u32, presets: &Presets) -> Option<EncodePlan> {
        match backend {
            "nvenc" | "gpu" => nvenc_plan(w, h, &presets.nvenc, &presets.codec),
            "vaapi" | "cpu" => vaapi_plan(w, h, presets.vaapi_cl, &presets.codec),
            #[cfg(target_os = "macos")]
            "videotoolbox" | "vt" => videotoolbox_plan(w, h, &presets.codec),
            "software" | "sw" | "x264" => Some(software_plan(&presets.x264, &presets.codec, w, h)),
            _ => None,
        }
    }

    /// Resolve the encoder for a recording: software when `hardware` is off; otherwise
    /// honour the user's `preferred` choice, falling back to the best available
    /// (GPU → software) when it's "auto" or the preference isn't usable.
    pub fn resolve(preferred: &str, w: u32, h: u32, presets: &Presets) -> EncodePlan {
        let chosen = match preferred {
            "nvenc" => nvenc_plan(w, h, &presets.nvenc, &presets.codec),
            "vaapi" => vaapi_plan(w, h, presets.vaapi_cl, &presets.codec),
            #[cfg(target_os = "macos")]
            "videotoolbox" => videotoolbox_plan(w, h, &presets.codec),
            "software" => Some(software_plan(&presets.x264, &presets.codec, w, h)),
            _ => None, // "auto" / unknown
        };
        let chosen = chosen
            .or_else(|| nvenc_plan(w, h, &presets.nvenc, &presets.codec))
            .or_else(|| vaapi_plan(w, h, presets.vaapi_cl, &presets.codec));
        // macOS: VideoToolbox is the hardware tier — tried before the software fallback
        // (nvenc/vaapi above always no-op on mac: no /dev/nvidia0, no /dev/dri).
        #[cfg(target_os = "macos")]
        let chosen = chosen.or_else(|| videotoolbox_plan(w, h, &presets.codec));
        chosen.unwrap_or_else(|| software_plan(&presets.x264, &presets.codec, w, h))
    }

    /// The stable id of the encoder [`resolve`] will actually use (`"nvenc"` / `"vaapi"`
    /// / `"videotoolbox"` / `"software"`), without building a plan — so a caller can size
    /// the encode resolution to the encoder BEFORE resolving (the software real-time cap
    /// depends on knowing the CPU fallback is what wins). Mirrors `resolve`'s fallback
    /// chain: the encoder choice never depends on the frame size (only the codec/side
    /// checks inside each `*_plan` do), so a size-independent probe is exact. A generous
    /// side is passed to the probes so hardware availability, not a size limit, decides.
    ///
    /// Only the mac SCK worker (`record::sck`) calls this today (the software real-time
    /// cap is a mac-gated concern); it is dead on Linux, hence the `not(macos)` allow.
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    pub fn resolve_encoder_id<'a>(preferred: &'a str, presets: &Presets) -> &'a str {
        // 1x1 is enough for the availability probes; each `*_plan` returns Some when the
        // hardware + encoder exist (its side checks only matter far above 1px).
        let (w, h) = (1u32, 1u32);
        let usable = |id: &str| -> bool {
            match id {
                "nvenc" => nvenc_plan(w, h, &presets.nvenc, &presets.codec).is_some(),
                "vaapi" => vaapi_plan(w, h, presets.vaapi_cl, &presets.codec).is_some(),
                #[cfg(target_os = "macos")]
                "videotoolbox" => videotoolbox_plan(w, h, &presets.codec).is_some(),
                _ => false,
            }
        };
        match preferred {
            "nvenc" | "vaapi" | "videotoolbox" if usable(preferred) => return preferred,
            "software" => return "software",
            _ => {}
        }
        // "auto" / an unavailable preference: the same order `resolve` falls back through.
        for id in ["nvenc", "vaapi", "videotoolbox"] {
            if usable(id) {
                return id;
            }
        }
        "software"
    }
}

/// The exact encode a real recording of a `px_w`×`px_h` capture would run on the
/// `encoder` backend — the SAME resolution + codec routing the recording workers
/// use (`record::sck` / `record::screencopy`), so a benchmark of the result predicts
/// real recording behaviour on that monitor (DRAGON-163). Returns the resolved
/// backend id, the encode dimensions after the codec + software real-time caps
/// (`encoder_capped_resolution` + `fit_within`), and whether the plan lands on HEVC.
///
/// `encoder` is the specific backend to test (a concrete id like `"videotoolbox"` /
/// `"software"`, matching `EncoderInfo::id`); its availability is assumed (the caller
/// only benchmarks encoders it already listed), so the returned id echoes it — this is
/// NOT `resolve_encoder_id`'s auto fallback chain, it is "what does THIS encoder do at
/// this size". `codec`/`fps` come from the recording settings; `max_res` is the user's
/// max-resolution box (`(0,0)` = no cap).
pub struct BenchPlan {
    pub width: u32,
    pub height: u32,
    pub is_hevc: bool,
}

/// The PURE dimension half of [`bench_plan_for`]: the encode `(w, h)` a recording of a
/// `px_w`×`px_h` capture would run on `encoder` — `encoder_capped_resolution` (codec +
/// software real-time caps) then `fit_within`. No ffmpeg probe, so it's unit-testable.
pub fn bench_encode_dims(
    encoder: &str,
    px_w: u32,
    px_h: u32,
    max_res: (u32, u32),
    fps: u32,
    codec: &str,
) -> (u32, u32) {
    // One source of truth with the recording path (DRAGON-168): the SCK stream delivers,
    // and this benches, at exactly these dims.
    super::encode_dims(px_w, px_h, max_res, codec, encoder, fps.max(1))
}

pub fn bench_plan_for(
    encoder: &str,
    px_w: u32,
    px_h: u32,
    max_res: (u32, u32),
    fps: u32,
    presets: &Presets,
) -> BenchPlan {
    let (ew, eh) = bench_encode_dims(encoder, px_w, px_h, max_res, fps, &presets.codec);
    // Build the plan for this exact backend at the capped size to read back the codec
    // it resolves to (h264 vs hevc — the DRAGON-162 routing above 4096). Fall back to
    // `resolve` (which honours the fallback chain) when the backend can't build one,
    // so the reported codec matches what would actually run.
    let is_hevc = EncodePlan::for_backend(encoder, ew, eh, presets)
        .unwrap_or_else(|| EncodePlan::resolve(encoder, ew, eh, presets))
        .is_hevc();
    BenchPlan { width: ew, height: eh, is_hevc }
}

fn vstr(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// Choose between two already-built codec arg lists (lazy — only the winner is
/// built): `h264_ok`/`hevc_ok` already fold in that codec's side limit + encoder-name
/// availability, so this is just "try the preferred one, fall back to the other, else
/// neither fits". NVENC's and VAAPI's h264-vs-hevc branches both reduce to this shape;
/// only the args + what makes `h264_ok`/`hevc_ok` true differ per backend.
fn pick_h264_or_hevc(
    prefer_hevc: bool,
    h264_ok: bool,
    hevc_ok: bool,
    h264_args: impl FnOnce() -> Vec<String>,
    hevc_args: impl FnOnce() -> Vec<String>,
) -> Option<Vec<String>> {
    if prefer_hevc {
        if hevc_ok {
            Some(hevc_args())
        } else if h264_ok {
            Some(h264_args())
        } else {
            None
        }
    } else if h264_ok {
        Some(h264_args())
    } else if hevc_ok {
        Some(hevc_args())
    } else {
        None
    }
}

/// Software encode (RGBA in, ffmpeg converts) — the universal fallback that needs no
/// special hardware. `libx264` (H.264) or `libx265` (HEVC) per the codec choice;
/// `-tune zerolatency` keeps the real-time pipeline from buffering, and CRF gives a
/// constant quality (small for static content, capped by the bitrate in spawn_ffmpeg).
fn software_plan(x264_preset: &str, codec_choice: &str, w: u32, h: u32) -> EncodePlan {
    let preset = valid_x264_preset(x264_preset);
    // `auto` stays H.264 (fast, universally compatible) unless the frame exceeds
    // H.264's 4096 limit; `hevc`/`h264` force the choice. HEVC needs libx265.
    //
    // macOS (DRAGON-162): `auto` NEVER auto-switches to libx265 for the software path.
    // libx264 is ~2.4× faster for real-time capture (measured ~16 vs ~6.6 fps at
    // 5120x2880 on an M1 Pro), so switching to HEVC above 4096 px made a big software
    // recording FREEZE, not just look worse. The mac SCK worker caps the software
    // encode side to a sustainable value (`encoder_capped_resolution`), so the frame is
    // already within H.264's limit here; only an EXPLICIT `hevc` choice pays the x265
    // cost. Linux keeps its historical size-based auto behaviour (byte-identical).
    #[cfg(target_os = "macos")]
    let want_hevc = {
        let _ = (w, h); // size does not drive the mac software codec choice
        codec_choice == "hevc"
    };
    #[cfg(not(target_os = "macos"))]
    let want_hevc = match codec_choice {
        "hevc" => true,
        "h264" => false,
        _ => w.max(h) > 4096,
    };
    // HEVC's CRF scale runs higher for similar quality, so 28 ≈ x264's 23 but smaller;
    // hvc1 tag so the mp4 plays in Apple players.
    let codec = if want_hevc && software_supports_hevc() {
        vstr(&[
            "-c:v", "libx265", "-preset", preset, "-tune", "zerolatency",
            "-pix_fmt", "yuv420p", "-crf", "28", "-tag:v", "hvc1",
        ])
    } else {
        vstr(&[
            "-c:v", "libx264", "-preset", preset, "-tune", "zerolatency",
            "-pix_fmt", "yuv420p", "-crf", "23",
        ])
    };
    EncodePlan {
        pre: Vec::new(),
        codec,
        env: Vec::new(),
        vf: None,
        color_tags: false,
        nv12: false,
        quality_rc: true,
    }
}

/// Discrete NVIDIA GPU encoder (NVENC), fed NV12. `None` if there's no nvidia
/// device, the driver stack is in the post-update NVML mismatch state (reboot
/// pending), or the frame exceeds the codec's max side (H.264 ≤4096, HEVC ≤8192).
fn nvenc_plan(w: u32, h: u32, nvenc_preset: &str, codec_choice: &str) -> Option<EncodePlan> {
    if !std::path::Path::new("/dev/nvidia0").exists() {
        return None;
    }
    // A driver update since boot means NVENC can't initialise — fall back to the
    // next best encoder rather than fail the recording (the Health page carries the
    // warning + reboot hint). The encoder stays listed/persisted; the state clears
    // on reboot.
    if nvenc_driver_mismatch() {
        return None;
    }
    let encoders = ffmpeg_encoders();
    let side = w.max(h);
    let preset = valid_nvenc_preset(nvenc_preset);
    // Codec: honour the user's pick; `auto` is the old resolution rule (H.264 ≤ 4096,
    // else HEVC). The capture was already downscaled to the codec's max side, so the
    // size checks below normally pass; the extra arms are graceful fallbacks.
    let want_hevc = match codec_choice {
        "hevc" => true,
        "h264" => false,
        _ => side > 4096,
    };
    // `-rc vbr -cq N -b:v 0`: constant-quality VBR — spend only the bits the content
    // needs (small for static screen demos) at a steady quality, with the bitrate
    // setting acting as a peak cap (added in spawn_ffmpeg). Avoids the chroma crush a
    // forced low bitrate caused.
    let h264 = || vstr(&[
        "-c:v", "h264_nvenc", "-preset", preset, "-bf", "0", "-rc", "vbr", "-cq", "23", "-b:v", "0",
    ]);
    // hvc1 tag so the HEVC mp4 plays in QuickTime/Apple players too.
    let hevc = || vstr(&[
        "-c:v", "hevc_nvenc", "-preset", preset, "-bf", "0", "-rc", "vbr", "-cq", "23", "-b:v", "0",
        "-tag:v", "hvc1",
    ]);
    // `-bf 0` (no B-frames): for real-time capture this both lowers encode latency
    // (no frame reordering — the live file grows smoothly) and measurably raises
    // throughput on NVENC. (We avoid `-tune ll`: it disables B-frames too but flips
    // hevc into a slower rate-control mode; `-bf 0` gets the win on both.)
    let h264_ok = side <= 4096 && encoders.contains("h264_nvenc");
    let hevc_ok = side <= 8192 && encoders.contains("hevc_nvenc");
    let codec = pick_h264_or_hevc(want_hevc, h264_ok, hevc_ok, h264, hevc)?;
    Some(EncodePlan {
        pre: Vec::new(),
        codec,
        env: Vec::new(),
        vf: None,
        color_tags: true,
        nv12: true,
        quality_rc: true,
    })
}

/// Integrated/discrete VAAPI encoder (AMD APU/iGPU, Intel iGPU), fed NV12. Detects
/// the correct render node + libva driver. `None` if there's no usable VAAPI device
/// or the frame is too big.
fn vaapi_plan(w: u32, h: u32, compression_level: i32, codec_choice: &str) -> Option<EncodePlan> {
    let (dev, driver) = vaapi_device()?;
    let encoders = ffmpeg_encoders();
    let side = w.max(h);
    let compression_level = valid_vaapi_cl(compression_level);
    // `-bf 0` (no B-frames): on AMD/Intel VAAPI this is a big throughput win for the
    // H.264 path — measured +71% (1080p) / +77% (1440p) on an AMD iGPU, where the
    // driver's B-frame pipeline is expensive — and neutral for HEVC, while lowering
    // latency on both (the live file grows smoothly). Safe everywhere.
    // Codec: `auto` prefers HEVC when available (the old behaviour); the user can
    // force H.264 (compatibility) or HEVC. The capture was clamped to the codec's max
    // side already.
    let prefer_hevc = codec_choice != "h264";
    let h264 = || vstr(&["-c:v", "h264_vaapi", "-bf", "0"]);
    let hevc = || vstr(&["-c:v", "hevc_vaapi", "-bf", "0", "-tag:v", "hvc1"]);
    let h264_ok = side <= 4096 && encoders.contains("h264_vaapi");
    let hevc_ok = side <= 8192 && encoders.contains("hevc_vaapi");
    let mut codec = pick_h264_or_hevc(prefer_hevc, h264_ok, hevc_ok, h264, hevc)?;
    // `-compression_level` is the real speed/quality knob on AMD radeonsi (higher =
    // slower + better); only set it when the user picked a level (>= 0), else leave
    // the driver default.
    if compression_level >= 0 {
        codec.push("-compression_level".to_string());
        codec.push(compression_level.to_string());
    }
    // Quality-defined VBR (QVBR): like NVENC's CQ, hold a target quality and let the
    // bitrate float beneath the cap, so static screen content makes small files. QVBR
    // is driver/Mesa-dependent, so probe the *actual* encoder once and only enable it
    // when the driver accepts it; otherwise fall through to the classic bitrate target
    // below. Either way the `-b:v`/`-maxrate` cap from spawn_ffmpeg still applies (in
    // QVBR it's the ceiling), so `quality_rc` stays false.
    let encoder = codec.get(1).cloned().unwrap_or_default();
    if vaapi_supports_qvbr(&dev, &driver, &encoder) {
        codec.extend(vstr(&["-rc_mode", "QVBR", "-global_quality", "25"]));
    }
    Some(EncodePlan {
        pre: vstr(&["-vaapi_device", &dev]),
        codec,
        // libva often can't auto-pick the driver when an nvidia node is also
        // present, so name it explicitly for the detected GPU.
        env: vec![("LIBVA_DRIVER_NAME".to_string(), driver)],
        // Upload to a VAAPI surface via filter_complex (mapping its labelled output,
        // not the raw input). VAAPI rejects the explicit colour tags, so leave them
        // off — the driver tags the stream itself.
        vf: Some("format=nv12,hwupload".to_string()),
        color_tags: false,
        nv12: true,
        // QVBR (when probed above) makes the bitrate a ceiling; without it, a target.
        // Both want spawn_ffmpeg's `-b:v`, so quality_rc is false in both cases.
        quality_rc: false,
    })
}

/// Apple VideoToolbox hardware encoder (Apple Silicon + recent Intel Macs), fed RGBA —
/// ffmpeg does the colour conversion (correctness-first, matching `software_plan`'s feed
/// path; a native NV12 feed is a later optimisation). `None` if ffmpeg lacks the encoder
/// or the frame exceeds the codec's max side (H.264 ≤4096, HEVC ≤8192). `-realtime true`
/// selects VideoToolbox's live rate control; the `-b:v` bitrate target/cap is added in
/// `spawn_ffmpeg` (VideoToolbox is bitrate-based, so `quality_rc` is false).
#[cfg(target_os = "macos")]
fn videotoolbox_plan(w: u32, h: u32, codec_choice: &str) -> Option<EncodePlan> {
    videotoolbox_plan_with(&ffmpeg_encoders(), w, h, codec_choice)
}

/// The pure core of [`videotoolbox_plan`]: the encoder set is passed in (rather than
/// probed) so the arg selection + size gating is unit-testable without spawning ffmpeg.
#[cfg(target_os = "macos")]
fn videotoolbox_plan_with(encoders: &str, w: u32, h: u32, codec_choice: &str) -> Option<EncodePlan> {
    let side = w.max(h);
    // Codec: honour the user's pick; `auto` is the resolution rule (H.264 ≤4096, else
    // HEVC) — the same policy the other hardware plans use.
    let want_hevc = match codec_choice {
        "hevc" => true,
        "h264" => false,
        _ => side > 4096,
    };
    let h264 = || vstr(&["-c:v", "h264_videotoolbox", "-realtime", "true"]);
    // hvc1 tag so the HEVC mp4 plays in QuickTime/Apple players.
    let hevc = || vstr(&["-c:v", "hevc_videotoolbox", "-realtime", "true", "-tag:v", "hvc1"]);
    let h264_ok = side <= 4096 && encoders.contains("h264_videotoolbox");
    let hevc_ok = side <= 8192 && encoders.contains("hevc_videotoolbox");
    let codec = pick_h264_or_hevc(want_hevc, h264_ok, hevc_ok, h264, hevc)?;
    Some(EncodePlan {
        pre: Vec::new(),
        codec,
        env: Vec::new(),
        vf: None,
        color_tags: false,
        nv12: false,
        quality_rc: false,
    })
}

/// Whether this VAAPI device + driver accepts QVBR (quality-defined VBR) rate control
/// for `encoder`. QVBR support is driver/Mesa-dependent, so probe the *actual* encoder
/// once with a throwaway 2-frame encode: drivers that lack it fail here and we fall back
/// to the classic bitrate target — never breaking a real recording. Cached per encoder.
fn vaapi_supports_qvbr(dev: &str, driver: &str, encoder: &str) -> bool {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    // A panicked probe thread would poison this mutex; the cached value is just a
    // plain bool snapshot, so keep using it rather than poisoning every later
    // recording's QVBR probe too.
    if let Some(&v) = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(encoder) {
        return v;
    }
    let ok = Command::new(crate::util::ffmpeg_path())
        .args([
            "-hide_banner", "-loglevel", "error",
            "-vaapi_device", dev,
            "-f", "lavfi", "-i", "color=c=black:s=256x256:r=5:d=1",
            "-vf", "format=nv12,hwupload",
            "-c:v", encoder,
            "-rc_mode", "QVBR", "-global_quality", "25", "-b:v", "1000k",
            "-frames:v", "2", "-f", "null", "-",
        ])
        .env("LIBVA_DRIVER_NAME", driver)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(encoder.to_string(), ok);
    ok
}

/// Find a VAAPI-capable render node and its libva driver, skipping nvidia (whose
/// VAAPI support is unreliable): amdgpu→`radeonsi`, Intel i915/xe→`iHD`.
pub(crate) fn vaapi_device() -> Option<(String, String)> {
    let mut nodes: Vec<String> = std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("renderD").then_some(n)
        })
        .collect();
    nodes.sort();
    for node in nodes {
        let driver = std::fs::read_link(format!("/sys/class/drm/{node}/device/driver"))
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()));
        let libva = match driver.as_deref() {
            Some("amdgpu") => "radeonsi",
            Some("i915") | Some("xe") => "iHD",
            _ => continue,
        };
        return Some((format!("/dev/dri/{node}"), libva.to_string()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with_codec(codec: &[&str]) -> EncodePlan {
        EncodePlan {
            pre: Vec::new(),
            codec: vstr(codec),
            env: Vec::new(),
            vf: None,
            color_tags: false,
            nv12: false,
            quality_rc: false,
        }
    }

    // DRAGON-163: the benchmark's dimension resolution must mirror the recording workers
    // (record::sck / record::screencopy). Uses the PURE `bench_encode_dims` — no ffmpeg.
    #[test]
    fn bench_dims_cap_software_but_not_hardware_at_5k() {
        // Software: capped to the sustainable side (matches encoder_capped_resolution).
        let (sw, sh) = bench_encode_dims("software", 5120, 2880, (0, 0), 30, "auto");
        assert!(sw <= 3200 && sh <= 3200, "software 5K encode is downscaled");
        // A hardware id keeps the full footprint (only the codec side cap applies, 8192).
        let hw = bench_encode_dims("videotoolbox", 5120, 2880, (0, 0), 30, "auto");
        assert_eq!(hw, (5120, 2880), "hardware keeps the true 5K size");
    }

    #[test]
    fn bench_dims_honour_the_user_max_resolution_box() {
        // A 1080p cap wins over the true 5K footprint, on any encoder.
        let (w, h) = bench_encode_dims("software", 5120, 2880, (1920, 1080), 30, "auto");
        assert!(w <= 1920 && h <= 1080);
    }

    #[test]
    fn is_hevc_detects_hevc_and_x265_encoders() {
        assert!(plan_with_codec(&["-c:v", "hevc_nvenc", "-cq", "23"]).is_hevc());
        assert!(plan_with_codec(&["-c:v", "hevc_vaapi", "-tag:v", "hvc1"]).is_hevc());
        // libx265 matches on the "265" substring.
        assert!(plan_with_codec(&["-c:v", "libx265", "-crf", "28"]).is_hevc());
    }

    #[test]
    fn is_hevc_false_for_h264_encoders() {
        assert!(!plan_with_codec(&["-c:v", "h264_nvenc"]).is_hevc());
        assert!(!plan_with_codec(&["-c:v", "h264_vaapi"]).is_hevc());
        assert!(!plan_with_codec(&["-c:v", "libx264", "-crf", "23"]).is_hevc());
    }

    // software_plan only invokes ffmpeg (software_supports_hevc) when want_hevc is
    // true, which `&&` short-circuits away for the h264 / auto-small cases below — so
    // these stay pure (no external binary).
    #[test]
    fn software_plan_forced_h264_picks_libx264_with_validated_preset() {
        let p = software_plan("medium", "h264", 1920, 1080);
        assert!(p.codec.iter().any(|a| a == "libx264"));
        assert!(p.codec.iter().any(|a| a == "medium")); // valid preset passes through
        assert!(!p.is_hevc());
        assert!(!p.nv12); // software path feeds RGBA
        assert!(!p.color_tags);
        assert!(p.quality_rc);
    }

    #[test]
    fn software_plan_auto_under_4096_stays_h264() {
        // auto codec + side <= 4096 -> want_hevc == false -> libx264 (no ffmpeg probe),
        // on every platform.
        let p = software_plan("fast", "auto", 3840, 2160);
        assert!(p.codec.iter().any(|a| a == "libx264"));
        assert!(!p.is_hevc());
    }

    // DRAGON-162: the software path's `auto` codec choice above 4096 px is platform-split.
    // macOS never switches to libx265 (far too slow for real-time; the SCK worker caps the
    // size so H.264 always fits); Linux keeps its historical size-based auto → x265.
    #[cfg(target_os = "macos")]
    #[test]
    fn software_plan_auto_stays_h264_even_over_4096_on_mac() {
        let p = software_plan("fast", "auto", 5120, 2880);
        assert!(p.codec.iter().any(|a| a == "libx264"), "mac auto @5K must stay x264");
        assert!(!p.is_hevc());
    }

    #[test]
    fn software_plan_invalid_preset_falls_back_to_default() {
        let p = software_plan("not-a-preset", "h264", 1280, 720);
        assert!(p.codec.iter().any(|a| a == crate::encode::DEFAULT_X264_PRESET));
        assert!(!p.codec.iter().any(|a| a == "not-a-preset"));
    }

    // The VideoToolbox tests drive the pure `videotoolbox_plan_with` core (encoder set
    // injected) so they never spawn ffmpeg — matching the "no live probes in unit tests"
    // rule while still asserting the exact args the hardware plan emits.
    #[cfg(target_os = "macos")]
    const VT_BOTH: &str = "h264_videotoolbox hevc_videotoolbox";

    #[cfg(target_os = "macos")]
    #[test]
    fn videotoolbox_forced_h264_emits_realtime_rgba_bitrate_plan() {
        let p = videotoolbox_plan_with(VT_BOTH, 1920, 1080, "h264").expect("vt h264 plan");
        assert!(p.codec.iter().any(|a| a == "h264_videotoolbox"));
        assert!(p.codec.windows(2).any(|w| w == ["-realtime", "true"]));
        assert!(!p.is_hevc());
        assert!(!p.nv12, "the mac plan feeds RGBA (ffmpeg converts)");
        assert!(!p.color_tags);
        assert!(!p.quality_rc, "VideoToolbox is bitrate-based (spawn adds -b:v)");
        assert!(p.pre.is_empty() && p.env.is_empty() && p.vf.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn videotoolbox_forced_hevc_emits_hevc_encoder_and_hvc1_tag() {
        let p = videotoolbox_plan_with(VT_BOTH, 1920, 1080, "hevc").expect("vt hevc plan");
        assert!(p.codec.iter().any(|a| a == "hevc_videotoolbox"));
        assert!(p.codec.windows(2).any(|w| w == ["-tag:v", "hvc1"]));
        assert!(p.codec.windows(2).any(|w| w == ["-realtime", "true"]));
        assert!(p.is_hevc());
        assert!(!p.nv12);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn videotoolbox_auto_picks_h264_small_and_hevc_when_over_4096() {
        // auto + side <= 4096 -> H.264; auto + side > 4096 -> HEVC (the resolution rule).
        assert!(!videotoolbox_plan_with(VT_BOTH, 3840, 2160, "auto").unwrap().is_hevc());
        assert!(videotoolbox_plan_with(VT_BOTH, 5120, 2160, "auto").unwrap().is_hevc());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn videotoolbox_none_when_encoder_absent_so_software_is_reached() {
        // No VideoToolbox encoders in ffmpeg -> None, so `resolve`'s fallback chain
        // falls through to software (this is the precedence contract, checked purely).
        assert!(videotoolbox_plan_with("", 1920, 1080, "auto").is_none());
        // Forced H.264 but the frame is too wide for H.264 and no HEVC encoder -> None.
        assert!(videotoolbox_plan_with("h264_videotoolbox", 5120, 2160, "h264").is_none());
        // Forced HEVC without the HEVC encoder falls back to H.264 when it fits.
        assert!(!videotoolbox_plan_with("h264_videotoolbox", 1920, 1080, "hevc").unwrap().is_hevc());
    }

    // DRAGON-163: the exact regression the fixed benchmark now catches — a 6400x3600
    // Studio Display capture (the user's scaled-mode footprint) on VideoToolbox resolves
    // to HEVC at full size, never the h264_videotoolbox that DRAGON-162 proved crashes
    // above 4096. `videotoolbox_plan_with` (encoders injected) keeps this pure.
    #[cfg(target_os = "macos")]
    #[test]
    fn bench_plan_6400x3600_videotoolbox_is_full_res_hevc() {
        // The mirror keeps the full footprint (hardware isn't downscaled) and routes to
        // HEVC (side 6400 > 4096) — exactly the DRAGON-162 fix the benchmark now tests.
        // Kept pure (no ffmpeg): compose the same caps + the injected-encoders plan core.
        let (mw, mh) = encoder_capped_resolution((0, 0), "auto", "videotoolbox", 30);
        let (ew, eh) = fit_within(6400, 3600, mw, mh);
        assert_eq!((ew, eh), (6400, 3600), "VideoToolbox keeps the full 6400x3600 footprint");
        assert!(
            videotoolbox_plan_with(VT_BOTH, ew, eh, "auto").unwrap().is_hevc(),
            "6400x3600 on VideoToolbox must route to HEVC, never crashing H.264"
        );
    }

    // DRAGON-162: the 5K root-cause + fix, pinned against REAL ffmpeg. These build tiny
    // synthetic encodes (no screen capture, no display) at exactly the boundary that
    // broke recording the Apple Studio Display (5120x2880) and LOUDLY skip when ffmpeg
    // is unavailable, matching `record::sck`'s convention.
    #[cfg(target_os = "macos")]
    fn ffmpeg_encode_ok(codec_args: &[&str], w: u32, h: u32) -> bool {
        // A 1-frame testsrc2 encode through the given `-c:v …` args to `-f null`. Returns
        // whether ffmpeg exited 0 (the encoder opened + wrote a packet). Mirrors the shape
        // the recorder builds: `-realtime true` + an explicit `-b:v`.
        Command::new(crate::util::ffmpeg_path())
            .args(["-hide_banner", "-loglevel", "error"])
            .args(["-f", "lavfi", "-i", &format!("testsrc2=size={w}x{h}:rate=30")])
            .args(["-frames:v", "3"])
            .args(codec_args)
            .args(["-b:v", "40000k", "-f", "null", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[cfg(target_os = "macos")]
    fn have_ffmpeg_with(encoder: &str) -> bool {
        ffmpeg_encoders().contains(encoder)
    }

    /// The proven ROOT CAUSE: `h264_videotoolbox` cannot open a compression session at
    /// 5120x2880 (H.264's 4096 side limit is an Apple-Silicon hardware ceiling too), so a
    /// 5K recording forced onto it fails; `hevc_videotoolbox` at the SAME size succeeds.
    /// This is exactly why `videotoolbox_plan` selects HEVC above 4096.
    #[cfg(target_os = "macos")]
    #[test]
    fn videotoolbox_h264_fails_at_5k_but_hevc_succeeds_real_ffmpeg() {
        if !have_ffmpeg_with("h264_videotoolbox") || !have_ffmpeg_with("hevc_videotoolbox") {
            eprintln!(
                "SKIPPED (loud): videotoolbox_h264_fails_at_5k_but_hevc_succeeds_real_ffmpeg \
                 needs ffmpeg with the VideoToolbox encoders — the DRAGON-162 root-cause \
                 proof did not run"
            );
            return;
        }
        // Small sizes: both encoders open fine (sanity, so a FAIL below is really the size).
        assert!(
            ffmpeg_encode_ok(&["-c:v", "h264_videotoolbox", "-realtime", "true"], 1920, 1080),
            "h264_videotoolbox must open at 1080p"
        );
        // 5120x2880: H.264 must FAIL (the reported crash), HEVC must SUCCEED (our fix path).
        assert!(
            !ffmpeg_encode_ok(&["-c:v", "h264_videotoolbox", "-realtime", "true"], 5120, 2880),
            "h264_videotoolbox is expected to FAIL at 5120x2880 (over H.264's 4096 limit) — \
             if this now passes, Apple raised the ceiling and the plan's cap can relax"
        );
        assert!(
            ffmpeg_encode_ok(
                &["-c:v", "hevc_videotoolbox", "-realtime", "true", "-tag:v", "hvc1"],
                5120, 2880
            ),
            "hevc_videotoolbox must succeed at 5120x2880 (this is the 5K recording fix)"
        );
    }

    /// The FIX, pinned: the plan a 5K "auto" VideoToolbox recording resolves to is HEVC,
    /// and its exact `-c:v` args actually initialize at 5120x2880 through real ffmpeg — so
    /// recording the Studio Display no longer picks the encoder that crashes.
    #[cfg(target_os = "macos")]
    #[test]
    fn resolved_5k_videotoolbox_plan_initializes_real_ffmpeg() {
        if !have_ffmpeg_with("hevc_videotoolbox") {
            eprintln!(
                "SKIPPED (loud): resolved_5k_videotoolbox_plan_initializes_real_ffmpeg needs \
                 ffmpeg with hevc_videotoolbox — the DRAGON-162 fix proof did not run"
            );
            return;
        }
        let presets = Presets {
            nvenc: "p4".into(), x264: "fast".into(), vaapi_cl: 3, codec: "auto".into(),
        };
        // Hardware isn't downscaled: auto @ 5120x2880 -> HEVC VideoToolbox.
        let (mw, mh) = encoder_capped_resolution((0, 0), "auto", "videotoolbox", 30);
        let (ew, eh) = fit_within(5120, 2880, mw, mh);
        assert_eq!((ew, eh), (5120, 2880), "VideoToolbox keeps the full 5K size");
        let plan = EncodePlan::resolve("videotoolbox", ew, eh, &presets);
        assert!(plan.is_hevc(), "5K auto+VideoToolbox must resolve to HEVC, never crashing H.264");
        let codec: Vec<&str> = plan.codec.iter().map(String::as_str).collect();
        assert!(
            ffmpeg_encode_ok(&codec, ew, eh),
            "the resolved 5K VideoToolbox plan ({codec:?}) must initialize at {ew}x{eh}"
        );
    }

    /// The SOFTWARE fix, pinned: a 5K "auto" software recording is (a) capped to a
    /// real-time-sustainable side and (b) resolves to libx264 (never the far-slower
    /// libx265 that made the software path freeze), and that plan initializes.
    #[cfg(target_os = "macos")]
    #[test]
    fn resolved_5k_software_plan_is_capped_x264_and_initializes_real_ffmpeg() {
        if !have_ffmpeg_with("libx264") {
            eprintln!(
                "SKIPPED (loud): resolved_5k_software_plan_is_capped_x264_and_initializes_real_ffmpeg \
                 needs ffmpeg with libx264 — the DRAGON-162 software fix proof did not run"
            );
            return;
        }
        let presets = Presets {
            nvenc: "p4".into(), x264: "fast".into(), vaapi_cl: 3, codec: "auto".into(),
        };
        let (mw, mh) = encoder_capped_resolution((0, 0), "auto", "software", 30);
        let (ew, eh) = fit_within(5120, 2880, mw, mh);
        assert!(ew <= 3200 && eh <= 3200, "software 5K encode is capped to a sustainable side");
        let plan = EncodePlan::resolve("software", ew, eh, &presets);
        assert!(!plan.is_hevc(), "software auto must stay libx264 at 5K, never libx265");
        let codec: Vec<&str> = plan.codec.iter().map(String::as_str).collect();
        assert!(
            ffmpeg_encode_ok(&codec, ew, eh),
            "the resolved capped software plan ({codec:?}) must initialize at {ew}x{eh}"
        );
    }
}
