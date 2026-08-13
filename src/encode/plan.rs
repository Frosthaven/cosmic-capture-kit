//! Encode plans: the per-backend ffmpeg argument recipe ([`EncodePlan`]) plus the
//! builders that probe the machine (NVENC / VAAPI / software) and choose one. The
//! command module turns a plan into the actual ffmpeg invocation.

use std::process::Stdio;

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
    /// The rawvideo input pixel format for the RGBA-feed (`nv12 == false`) path — the
    /// byte order the worker delivers, passed to ffmpeg's `-pix_fmt`. Defaults to
    /// `"rgba"` (every Linux/Windows worker, and the mac software/VideoToolbox default);
    /// the macOS SCK worker overrides it to `"bgra"` so it can hand ScreenCaptureKit's
    /// native BGRA buffers to ffmpeg WITHOUT a per-pixel swizzle (DRAGON-316). Ignored
    /// when `nv12` is true (the feed is NV12 then). Keeping the default `"rgba"` keeps
    /// every historical command line byte-identical.
    pub(crate) input_pix_fmt: &'static str,
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

    /// The stable id of the encoder THIS plan uses, read off the plan itself.
    ///
    /// [`resolve_encoder_id`](Self::resolve_encoder_id) answers the same question by
    /// re-walking the whole probe ladder, which is right for a caller that has no plan yet
    /// (it sizes the encode before resolving) and wrong for one that does. DRAGON-419's
    /// session log used it on a plan it had already built, which re-ran every hardware probe
    /// a second time on every recording start — including `ffmpeg_encoders`, an uncached
    /// `ffmpeg -encoders` spawn (~100ms measured here), and on Windows a probe-ENCODE per
    /// tier. That is pure latency on the video side of a recording start, and it is latency
    /// the user SEES: media 0 is anchored at audio-capture start (DRAGON-417), so every
    /// millisecond the video side takes to come up is a millisecond the finished recording
    /// opens on a held first frame.
    ///
    /// It is also the more honest answer. The probes are not guaranteed to agree across two
    /// runs — a VAAPI device that fails to initialise on the second call would have the log
    /// naming an encoder the session is not using — whereas this reads the plan that is
    /// actually about to run.
    pub fn encoder_id(&self) -> &'static str {
        let has = |needle: &str| self.codec.iter().any(|a| a.contains(needle));
        if has("_nvenc") {
            "nvenc"
        } else if has("_vaapi") {
            "vaapi"
        } else if has("_videotoolbox") {
            "videotoolbox"
        } else if has("_amf") {
            "amf"
        } else if has("_qsv") {
            "qsv"
        } else {
            "software"
        }
    }

    /// Build a plan for a SPECIFIC backend (`gpu`/`nvenc`, `cpu`/`vaapi`, `sw`) for
    /// benchmarking. `None` if that backend isn't available here.
    pub fn for_backend(backend: &str, w: u32, h: u32, presets: &Presets) -> Option<EncodePlan> {
        match backend {
            "nvenc" | "gpu" => nvenc_plan(w, h, &presets.nvenc, &presets.codec),
            "vaapi" | "cpu" => vaapi_plan(w, h, presets.vaapi_cl, &presets.codec),
            #[cfg(windows)]
            "amf" => amf_plan(w, h, &presets.codec),
            #[cfg(windows)]
            "qsv" => qsv_plan(w, h, &presets.codec),
            #[cfg(target_os = "macos")]
            "videotoolbox" | "vt" => videotoolbox_plan(w, h, &presets.codec),
            "software" | "sw" | "x264" => Some(software_plan(&presets.x264)),
            _ => None,
        }
    }

    /// Resolve the encoder for a recording: [`resolve_hinted`](Self::resolve_hinted)
    /// with no hint. Kept for callers that already hold a concrete id (the benchmark
    /// fallback, the e2e tests): their probe sequence and log line are unchanged.
    pub fn resolve(preferred: &str, w: u32, h: u32, presets: &Presets) -> EncodePlan {
        Self::resolve_hinted(preferred, None, w, h, presets)
    }

    /// Resolve the encoder for a recording (DRAGON-571): honour a CONCRETE `requested`
    /// id when its plan builds, falling back down [`AUTO_LADDER`] when it does not:
    /// the same probe sequence the old hand-chained `or_else` fallback ran, including
    /// the failing pick being probed once in its own slot and once more in ladder
    /// position. "auto" (or any unknown id) walks [`auto_probe_order`] instead, which
    /// hoists the last-known-good `hint` to the front: the happy path pays ONE
    /// probe-encode, the same single probe a concrete pick pays, and the rest of the
    /// ladder is only walked when the hinted encoder fails. The winner is never
    /// persisted as the user's preference; callers may update only the HINT (see
    /// [`auto_resolution_outcome`], applied by `record::resolve_session_plan`).
    pub fn resolve_hinted(
        requested: &str,
        hint: Option<&str>,
        w: u32,
        h: u32,
        presets: &Presets,
    ) -> EncodePlan {
        let build = |id: &str| -> Option<EncodePlan> {
            match id {
                "nvenc" => nvenc_plan(w, h, &presets.nvenc, &presets.codec),
                // vaapi always no-ops on Windows (no /dev/dri) and on mac; keeping it
                // in every ladder keeps the historical probe sequence byte-identical.
                "vaapi" => vaapi_plan(w, h, presets.vaapi_cl, &presets.codec),
                #[cfg(windows)]
                "amf" => amf_plan(w, h, &presets.codec),
                #[cfg(windows)]
                "qsv" => qsv_plan(w, h, &presets.codec),
                #[cfg(target_os = "macos")]
                "videotoolbox" => videotoolbox_plan(w, h, &presets.codec),
                "software" => Some(software_plan(&presets.x264)),
                _ => None,
            }
        };
        let order: Vec<&str> = if AUTO_LADDER.contains(&requested) {
            // A concrete pick: try it first, then the ranked ladder.
            std::iter::once(requested).chain(AUTO_LADDER.iter().copied()).collect()
        } else {
            // "auto" / unknown: the hint-first ladder.
            auto_probe_order(AUTO_LADDER, hint)
        };
        let plan = order
            .into_iter()
            .find_map(build)
            // Unreachable (every order ends in "software", which always builds), but a
            // recording resolver must never panic.
            .unwrap_or_else(|| software_plan(&presets.x264));
        // DRAGON-419: which encoder a session actually got was never recorded anywhere. It is
        // the first question of every recording report — a silent fall-through from the
        // requested hardware tier to software changes CPU load, frame pacing and quality all
        // at once, and "I picked NVENC" and "NVENC was usable" are different facts. One line
        // per session; no user content in any of it. DRAGON-571 added the hint so the line
        // shows all three facts: what was asked, what the cache suggested, what won.
        log::info!(
            "encoder plan: requested={requested} hint={} chosen={} encode_size={w}x{h} hevc={} nv12={}",
            hint.unwrap_or("none"),
            plan.encoder_id(),
            plan.is_hevc(),
            plan.nv12,
        );
        plan
    }

    /// The stable id of the encoder [`resolve`] will actually use (`"nvenc"` / `"vaapi"`
    /// / `"videotoolbox"` / `"software"`), without building a plan — so a caller can size
    /// the encode resolution to the encoder BEFORE resolving (the software real-time cap
    /// depends on knowing the CPU fallback is what wins). Mirrors `resolve`'s fallback
    /// chain: the encoder choice never depends on the frame size (only the codec/side
    /// checks inside each `*_plan` do), so a size-independent probe is exact. A generous
    /// side is passed to the probes so hardware availability, not a size limit, decides.
    ///
    /// The un-hinted wrapper over [`resolve_encoder_id_hinted`]. The mac SCK worker
    /// (`record::sck`) and the Windows WGC worker (`record::wgc`, DRAGON-229) used to call
    /// this directly (DRAGON-168 encode-size resolution) but have since moved to calling
    /// the hinted form themselves, so this wrapper's only remaining caller anywhere is the
    /// Windows-gated `cli::diagnostics` probe (`windows_record_test`) — dead in its own
    /// right on every other platform, hence the `not(windows)` gate below.
    ///
    /// **Do not call this when you already hold a plan** — use
    /// [`EncodePlan::encoder_id`], which reads the answer off the plan. Every call here
    /// re-walks the probe ladder, and the probes SPAWN PROCESSES (`ffmpeg_encoders` is an
    /// uncached `ffmpeg -encoders`; the Windows tiers each run a probe-encode). DRAGON-419's
    /// session log did exactly that on an already-resolved plan and doubled the encoder-
    /// resolution cost of every recording start (DRAGON-421).
    #[cfg_attr(not(windows), expect(dead_code))]
    pub fn resolve_encoder_id<'a>(preferred: &'a str, presets: &Presets) -> &'a str {
        Self::resolve_encoder_id_hinted(preferred, None, presets)
    }

    /// [`resolve_encoder_id`](Self::resolve_encoder_id) with the DRAGON-571 auto hint:
    /// the mac/Windows workers pass the same `(requested, hint)` pair here (to size the
    /// encode) and to [`resolve_hinted`] (to build the plan), and both walk the same
    /// hint-first order over the same probes, so the sizing answer and the plan's
    /// winner cannot drift apart.
    // `allow`, not `expect`: `record::sck` and `record::wgc` call this DIRECTLY now (see
    // `resolve_encoder_id`'s doc above), so it is dead only on Linux, where neither worker
    // exists — a plain per-platform absence, not the chain-propagation case `resolve_encoder_id`
    // itself is in (see the same note on `permissions::Tier`, DRAGON-625).
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    pub fn resolve_encoder_id_hinted<'a>(
        requested: &'a str,
        hint: Option<&'a str>,
        presets: &Presets,
    ) -> &'a str {
        // 1x1 is enough for the availability probes; each `*_plan` returns Some when the
        // hardware + encoder exist (its side checks only matter far above 1px).
        // Windows (DRAGON-238): the hardware tier is NVENC > AMF > QSV — a distinct
        // fallback order routed through the pure `windows_resolve_encoder_id` ranker.
        #[cfg(windows)]
        {
            let (w, h) = (1u32, 1u32);
            let usable = |id: &str| match id {
                "nvenc" => nvenc_plan(w, h, &presets.nvenc, &presets.codec).is_some(),
                "amf" => amf_plan(w, h, &presets.codec).is_some(),
                "qsv" => qsv_plan(w, h, &presets.codec).is_some(),
                _ => false,
            };
            windows_resolve_encoder_id(requested, hint, &usable)
        }
        #[cfg(not(windows))]
        {
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
            match requested {
                "nvenc" | "vaapi" | "videotoolbox" if usable(requested) => return requested,
                "software" => return "software",
                _ => {}
            }
            // "auto" / an unavailable preference: the hint-first ladder (DRAGON-571),
            // the same order `resolve_hinted` falls back through. `usable` answers false
            // for "software", so the ladder's tail lands on the fallback below.
            auto_probe_order(AUTO_LADDER, hint)
                .into_iter()
                .find(|id| usable(id))
                .unwrap_or("software")
        }
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
/// special hardware. ALWAYS `libx264` (H.264); `-tune zerolatency` keeps the real-time
/// pipeline from buffering, and CRF gives a constant quality (small for static content,
/// capped by the bitrate in spawn_ffmpeg).
///
/// **The software tier does not do HEVC** (DRAGON-674), whatever the codec setting says.
/// This is the RUNTIME half of the rule `encode::hevc_offerable` enforces in the
/// settings row, and it has to exist separately from that row: a config file travels
/// between machines, a driver breaks overnight, a game holds every NVENC session — so a
/// plan can land on software with `codec=hevc` persisted from a machine where it was a
/// perfectly honest choice. Measured in the field (DRAGON-671): a tester's NVENC probe
/// failed, the resolver fell to software, and the session logged `chosen=software …
/// hevc=true`, then `encoder backlog - 660 frames dropped`, then the DRAGON-118 muxer
/// watchdog correctly killed a recording that had written nothing for 12s. libx265
/// cannot encode 1080p in real time; a slightly larger file always beats a killed
/// recording, so the resolver degrades to H.264 itself rather than trusting the UI.
///
/// This subsumes the DRAGON-162 macOS carve-out, which said the same thing for one
/// platform and one reason (libx264 is ~2.4× faster — measured ~16 vs ~6.6 fps at
/// 5120x2880 on an M1 Pro — so `auto` switching to libx265 above 4096 px made a big
/// software recording FREEZE rather than merely look worse). The rule is unconditional
/// now, which is why neither the codec choice nor the frame size reaches this plan any
/// more: there is nothing left for them to decide.
fn software_plan(x264_preset: &str) -> EncodePlan {
    let preset = valid_x264_preset(x264_preset);
    EncodePlan {
        pre: Vec::new(),
        codec: vstr(&[
            "-c:v", "libx264", "-preset", preset, "-tune", "zerolatency",
            "-pix_fmt", "yuv420p", "-crf", "23",
        ]),
        env: Vec::new(),
        vf: None,
        color_tags: false,
        nv12: false,
        input_pix_fmt: "rgba",
        quality_rc: true,
    }
}

/// Discrete NVIDIA GPU encoder (NVENC), fed NV12. `None` if there's no nvidia
/// device, the driver stack is in the post-update NVML mismatch state (reboot
/// pending), or the frame exceeds the codec's max side (H.264 ≤4096, HEVC ≤8192).
fn nvenc_plan(w: u32, h: u32, nvenc_preset: &str, codec_choice: &str) -> Option<EncodePlan> {
    // Windows (DRAGON-238): there is no `/dev/nvidia0` to probe — the honest availability
    // gate is a real probe-encode through the bundled ffmpeg (cached). Success = the NVENC
    // driver stack initialises; failure degrades to the next encoder, same as Linux.
    #[cfg(windows)]
    if !hw_encoder_probe_ok("h264_nvenc") {
        return None;
    }
    #[cfg(not(windows))]
    if !std::path::Path::new("/dev/nvidia0").exists() {
        return None;
    }
    // A driver update since boot means NVENC can't initialise — fall back to the
    // next best encoder rather than fail the recording (the Health page carries the
    // warning + reboot hint). The encoder stays listed/persisted; the state clears
    // on reboot.
    #[cfg(not(windows))]
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
        input_pix_fmt: "rgba",
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
        input_pix_fmt: "rgba",
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
        input_pix_fmt: "rgba",
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
    let ok = crate::util::ffmpeg_command()
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
///
/// DRAGON-425 re-expressed this over the shared [`crate::encode::list_render_nodes`] +
/// [`crate::encode::libva_driver_for`]. Behaviour is unchanged — same sorted walk, same two
/// mappings, same first-match-wins — but this was the FOURTH copy of the `/dev/dri` walk and
/// the second copy of the libva table, and the zero-copy path now has to agree with it about
/// which node is which.
pub(crate) fn vaapi_device() -> Option<(String, String)> {
    crate::encode::list_render_nodes().into_iter().find_map(|(path, driver)| {
        let libva = crate::encode::libva_driver_for(&driver)?;
        Some((path.to_string_lossy().into_owned(), libva.to_string()))
    })
}

// ── Windows hardware encoder tier (DRAGON-238) ──────────────────────────────────────
//
// Windows exposes NVIDIA/AMD/Intel H.264 encoders through the bundled ffmpeg binary
// (`h264_nvenc` / `h264_amf` / `h264_qsv`). Unlike Linux (device-node checks) there is no
// `/dev` node to sniff, so the honest availability gate is a real probe-encode: an encoder
// present in the build but with no working driver FAILS the probe and degrades to the next
// tier, never a broken recording. These mirror the existing cfg(macos) VideoToolbox arm's
// shape and reuse `pick_h264_or_hevc` / the `EncodePlan` fields exactly like the other
// hardware plans; only the encoder names + the probe gate differ.

/// Whether `encoder` (an ffmpeg encoder name like `h264_nvenc`) can actually INITIALISE on
/// this machine — a throwaway 2-frame encode of a tiny synthetic source to `-f null`.
/// Success = the hardware + driver are present and working. Cached per encoder name so a
/// recording start (or the settings/Health page) never re-spawns the probe. Mirrors
/// `vaapi_supports_qvbr`'s cached-probe idiom; routed through the console-free
/// `util::ffmpeg_command` seam (DRAGON-236).
///
/// A FAILING probe is logged, once, with ffmpeg's own error text (DRAGON-669). It used to
/// discard stderr, which made "recording no longer detects my GPU" unanswerable: the only
/// other evidence is `chosen=software` on the plan line, and that is the SYMPTOM of every
/// possible cause at once. Two testers on NVENC-capable NVIDIA cards were diagnosed by
/// guesswork for want of the one line ffmpeg was already printing. The spawn failure and
/// the non-zero exit are reported SEPARATELY: `unwrap_or(false)` collapsed "there is no
/// ffmpeg to run" into "the hardware said no", which are different problems with different
/// fixes.
#[cfg(windows)]
pub(crate) fn hw_encoder_probe_ok(encoder: &str) -> bool {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Some(&v) = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(encoder) {
        return v;
    }
    // `.output()` rather than `.status()`, so stderr is captured instead of discarded.
    // `-loglevel error` is already set, so what comes back IS the reason and nothing else.
    let ok = match crate::util::ffmpeg_command()
        .args([
            "-hide_banner", "-loglevel", "error",
            "-f", "lavfi", "-i", "color=c=black:s=256x256:r=5:d=1",
            "-c:v", encoder,
            "-frames:v", "2", "-f", "null", "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            log::warn!(
                "hardware encoder probe failed for {encoder} (ffmpeg exit {}): {}",
                out.status.code().map_or_else(|| "signal".to_string(), |c| c.to_string()),
                probe_failure_reason(&String::from_utf8_lossy(&out.stderr)),
            );
            false
        }
        Err(e) => {
            log::warn!("hardware encoder probe for {encoder} could not run ffmpeg at all: {e}");
            false
        }
    };
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(encoder.to_string(), ok);
    ok
}

/// How many trailing lines of a failed probe's stderr reach the log. ffmpeg names the real
/// cause in its LAST lines (the driver-version refusal, the missing API library, the
/// "Cannot load" from the loader); anything earlier is the lavfi source announcing itself.
#[cfg(any(windows, test))]
const PROBE_REASON_LINES: usize = 3;

/// The loggable one-line reason from a failed probe's raw stderr: the last
/// [`PROBE_REASON_LINES`] non-blank lines, joined with `; `, run through
/// [`crate::diag::redact_paths`] because ffmpeg names files in its errors and the debug log
/// records what the app DID, never the user's filesystem. Empty stderr becomes an explicit
/// phrase rather than a blank, so a silent failure is still distinguishable in the log from
/// a missing log line.
///
/// Pure, unit-tested.
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
fn probe_failure_reason(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return "(ffmpeg printed nothing)".to_string();
    }
    let tail = &lines[lines.len().saturating_sub(PROBE_REASON_LINES)..];
    crate::diag::redact_paths(&tail.join("; "))
}

/// AMD AMF hardware encoder (`h264_amf` / `hevc_amf`), fed NV12 (our own threaded BT.709
/// conversion). `None` unless the probe-encode initialises AND the encoder is in the build
/// and the frame fits the codec's max side (H.264 ≤4096, HEVC ≤8192). `-quality speed`
/// selects AMF's low-latency preset; the `-b:v` bitrate target/cap is added in spawn_ffmpeg
/// (AMF is bitrate-based, so `quality_rc` is false).
#[cfg(windows)]
fn amf_plan(w: u32, h: u32, codec_choice: &str) -> Option<EncodePlan> {
    if !hw_encoder_probe_ok("h264_amf") {
        return None;
    }
    let encoders = ffmpeg_encoders();
    let side = w.max(h);
    let want_hevc = match codec_choice {
        "hevc" => true,
        "h264" => false,
        _ => side > 4096,
    };
    let h264 = || vstr(&["-c:v", "h264_amf", "-quality", "speed"]);
    let hevc = || vstr(&["-c:v", "hevc_amf", "-quality", "speed", "-tag:v", "hvc1"]);
    let h264_ok = side <= 4096 && encoders.contains("h264_amf");
    let hevc_ok = side <= 8192 && encoders.contains("hevc_amf");
    let codec = pick_h264_or_hevc(want_hevc, h264_ok, hevc_ok, h264, hevc)?;
    Some(EncodePlan {
        pre: Vec::new(),
        codec,
        env: Vec::new(),
        vf: None,
        color_tags: false,
        nv12: true,
        input_pix_fmt: "rgba",
        quality_rc: false,
    })
}

/// Intel Quick Sync (QSV) hardware encoder (`h264_qsv` / `hevc_qsv`), fed NV12. `None`
/// unless the probe-encode initialises AND the encoder is in the build and the frame fits
/// the codec's max side. `-preset veryfast` matches the real-time capture pipeline; the
/// bitrate target/cap is added in spawn_ffmpeg (QSV is bitrate-based here, so `quality_rc`
/// is false).
#[cfg(windows)]
fn qsv_plan(w: u32, h: u32, codec_choice: &str) -> Option<EncodePlan> {
    if !hw_encoder_probe_ok("h264_qsv") {
        return None;
    }
    let encoders = ffmpeg_encoders();
    let side = w.max(h);
    let want_hevc = match codec_choice {
        "hevc" => true,
        "h264" => false,
        _ => side > 4096,
    };
    let h264 = || vstr(&["-c:v", "h264_qsv", "-preset", "veryfast"]);
    let hevc = || vstr(&["-c:v", "hevc_qsv", "-preset", "veryfast", "-tag:v", "hvc1"]);
    let h264_ok = side <= 4096 && encoders.contains("h264_qsv");
    let hevc_ok = side <= 8192 && encoders.contains("hevc_qsv");
    let codec = pick_h264_or_hevc(want_hevc, h264_ok, hevc_ok, h264, hevc)?;
    Some(EncodePlan {
        pre: Vec::new(),
        codec,
        env: Vec::new(),
        vf: None,
        color_tags: false,
        nv12: true,
        input_pix_fmt: "rgba",
        quality_rc: false,
    })
}

/// The ranked auto/fallback encoder ladder for THIS build's platform, best first,
/// with "software" always last (it cannot fail to build). [`EncodePlan::resolve_hinted`]
/// walks it for an "auto" request (through [`auto_probe_order`]) and after a concrete
/// request whose own plan failed; the settings picker's display resolution
/// (`app::display_encoder_choice`) filters the same order through the probed list, so
/// what auto shows and what auto records cannot disagree on rank. "vaapi" stays in the
/// Windows/mac ladders even though it always no-ops there (no /dev/dri): that keeps the
/// probe sequence identical to the historical hand-chained fallback.
#[cfg(not(any(windows, target_os = "macos")))]
pub const AUTO_LADDER: &[&str] = &["nvenc", "vaapi", "software"];
#[cfg(windows)]
pub const AUTO_LADDER: &[&str] = &["nvenc", "vaapi", "amf", "qsv", "software"];
#[cfg(target_os = "macos")]
pub const AUTO_LADDER: &[&str] = &["nvenc", "vaapi", "videotoolbox", "software"];

/// Pure, unit-tested (DRAGON-571). The order an "auto" encoder resolution tries the
/// ranked `ladder` in: the last-known-good `hint` is hoisted to the front, so the
/// happy path pays a single probe-encode (exactly what a concrete user pick pays),
/// and the rest of the ladder keeps its rank for the walk that only happens when the
/// hinted encoder fails.
///
/// A "software" hint is IGNORED on purpose: software is the unconditional tail of
/// every ladder (it cannot fail), so hoisting it to the front would make auto land on
/// software forever, re-creating the exact permanent pin DRAGON-571 removes. A hint
/// naming an id not in `ladder` (foreign config, retired encoder) is ignored the same
/// way. A hoisted hint is deduplicated out of its ladder slot, so a failing hinted
/// encoder is probed once, not twice.
pub fn auto_probe_order<'a>(ladder: &[&'a str], hint: Option<&'a str>) -> Vec<&'a str> {
    match hint {
        Some(h) if h != "software" && ladder.contains(&h) => std::iter::once(h)
            .chain(ladder.iter().copied().filter(|id| *id != h))
            .collect(),
        _ => ladder.to_vec(),
    }
}

/// What a finished session resolution writes back, decided by
/// [`auto_resolution_outcome`] and applied by `record::resolve_session_plan`.
pub struct AutoResolutionOutcome {
    /// `Some(id)`: update the persisted last-known-good hint to `id` (it changed).
    /// NEVER the user's `preferred_encoder`: the hint is a cache with its own field,
    /// and an auto resolution is never written as if the user chose it (DRAGON-571).
    pub new_hint: Option<String>,
    /// The stored hint named a hardware encoder (proof it worked before), yet this
    /// session still landed on software. Worth a loud warn.
    pub hardware_hint_failed: bool,
}

/// Pure, unit-tested (DRAGON-571). After a session's encoder resolution: what, if
/// anything, to write back to the last-known-good AUTO hint, and whether the outcome
/// deserves a warn.
///
/// - A CONCRETE `requested` (a real user pick, including "software") teaches nothing:
///   the session used the user's choice, not the ladder, so neither the hint nor
///   `preferred_encoder` is ever written. This is the never-persist-auto rule's
///   write side, in one place.
/// - An auto (or unknown) request updates the hint to the winner whenever it changed,
///   including to "software", which records "nothing hardware worked last time". That
///   can never re-pin the CPU fallback, because [`auto_probe_order`] ignores software
///   hints; it only keeps the cache honest.
/// - `hardware_hint_failed` fires when a HARDWARE hint loses to software now: the
///   DRAGON-571 regression signature (driver asleep, NVENC sessions held by a game or
///   overlay, the ffmpeg sidecar missing an encoder). Nothing is pinned either way;
///   the next session re-probes.
pub fn auto_resolution_outcome(
    requested: &str,
    chosen: &str,
    stored_hint: Option<&str>,
) -> AutoResolutionOutcome {
    if AUTO_LADDER.contains(&requested) {
        return AutoResolutionOutcome { new_hint: None, hardware_hint_failed: false };
    }
    let hardware_hint = stored_hint.is_some_and(|h| h != "software" && AUTO_LADDER.contains(&h));
    AutoResolutionOutcome {
        new_hint: (stored_hint != Some(chosen)).then(|| chosen.to_string()),
        hardware_hint_failed: hardware_hint && chosen == "software",
    }
}

/// The Windows hardware-encoder preference order (DRAGON-238): honour the user's
/// `preferred` when it's usable, else fall back NVENC → AMF → QSV → software, with the
/// DRAGON-571 auto hint hoisted to the front of that fallback (via [`auto_probe_order`],
/// so the hoist-and-ignore rules cannot drift from the plan resolver's). Pure over a
/// `usable` predicate so the ranking is unit-testable without spawning ffmpeg — the real
/// [`EncodePlan::resolve_encoder_id`] feeds it the probe-backed predicate, which answers
/// false for "software", landing the ladder's tail on the explicit fallback.
#[cfg(windows)]
fn windows_resolve_encoder_id<'a>(
    preferred: &'a str,
    hint: Option<&'a str>,
    usable: &dyn Fn(&str) -> bool,
) -> &'a str {
    match preferred {
        "nvenc" | "amf" | "qsv" if usable(preferred) => preferred,
        "software" => "software",
        // "auto" / an unavailable preference: the hint-first ranked order, then software.
        _ => auto_probe_order(AUTO_LADDER, hint)
            .into_iter()
            .find(|id| usable(id))
            .unwrap_or("software"),
    }
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
            input_pix_fmt: "rgba",
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

    /// DRAGON-421: the session log names the encoder from the plan it already has, instead
    /// of re-walking the probe ladder (spawning `ffmpeg -encoders` again) on every recording
    /// start. Every tier must be readable off the plan, or the cheap path would have to fall
    /// back to the expensive one.
    #[test]
    fn encoder_id_is_read_off_the_plan_for_every_tier() {
        for (args, id) in [
            (&["-c:v", "h264_nvenc", "-cq", "23"][..], "nvenc"),
            (&["-c:v", "hevc_nvenc"][..], "nvenc"),
            (&["-c:v", "h264_vaapi", "-bf", "0"][..], "vaapi"),
            (&["-c:v", "hevc_vaapi", "-tag:v", "hvc1"][..], "vaapi"),
            (&["-c:v", "h264_videotoolbox"][..], "videotoolbox"),
            (&["-c:v", "h264_amf"][..], "amf"),
            (&["-c:v", "h264_qsv"][..], "qsv"),
            (&["-c:v", "libx264", "-crf", "23"][..], "software"),
            (&["-c:v", "libx265", "-crf", "28"][..], "software"),
        ] {
            assert_eq!(plan_with_codec(args).encoder_id(), id, "{args:?}");
        }
    }

    #[test]
    fn is_hevc_false_for_h264_encoders() {
        assert!(!plan_with_codec(&["-c:v", "h264_nvenc"]).is_hevc());
        assert!(!plan_with_codec(&["-c:v", "h264_vaapi"]).is_hevc());
        assert!(!plan_with_codec(&["-c:v", "libx264", "-crf", "23"]).is_hevc());
    }

    // software_plan spawns nothing at all now (DRAGON-674 took the last ffmpeg probe out
    // of it), so these are pure on every platform.
    #[test]
    fn software_plan_is_libx264_with_a_validated_preset() {
        let p = software_plan("medium");
        assert!(p.codec.iter().any(|a| a == "libx264"));
        assert!(p.codec.iter().any(|a| a == "medium")); // valid preset passes through
        assert!(!p.is_hevc());
        assert!(!p.nv12); // software path feeds RGBA
        assert!(!p.color_tags);
        assert!(p.quality_rc);
    }

    #[test]
    fn software_plan_invalid_preset_falls_back_to_default() {
        let p = software_plan("not-a-preset");
        assert!(p.codec.iter().any(|a| a == crate::encode::DEFAULT_X264_PRESET));
        assert!(!p.codec.iter().any(|a| a == "not-a-preset"));
    }

    /// DRAGON-674: the runtime safety net. A resolution that lands on software must
    /// emit H.264 no matter what the persisted codec setting says — a config travels
    /// between machines, so the UI gate alone cannot be the only guard. The field
    /// signature this replaces is the `chosen=software … hevc=true` plan line that
    /// preceded 660 dropped frames and a watchdog kill (DRAGON-671).
    ///
    /// Also subsumes the old macOS-only DRAGON-162 assertion (auto above 4096 px must
    /// never become libx265): the 5120x2880 case below is that one, now checked on
    /// every platform because the rule is no longer platform-split.
    #[test]
    fn no_software_plan_ever_resolves_to_hevc() {
        for codec in ["hevc", "auto", "h264"] {
            let presets = Presets {
                nvenc: "p4".into(),
                x264: "fast".into(),
                vaapi_cl: 3,
                codec: codec.into(),
            };
            // Both entry points, at a size that used to route `auto` to libx265.
            for (w, h) in [(1920, 1080), (5120, 2880)] {
                let direct = EncodePlan::for_backend("software", w, h, &presets)
                    .expect("software always builds");
                assert!(!direct.is_hevc(), "for_backend codec={codec} {w}x{h}");
                assert_eq!(direct.encoder_id(), "software");
                // `resolve` on a concrete "software" request builds it first, so this
                // stays probe-free (no hardware tier is ever reached).
                let resolved = EncodePlan::resolve("software", w, h, &presets);
                assert!(!resolved.is_hevc(), "resolve codec={codec} {w}x{h}");
                assert!(
                    resolved.codec.iter().all(|a| a != "libx265"),
                    "libx265 must never reach the command line: {:?}",
                    resolved.codec
                );
            }
        }
    }

    // DRAGON-238: the Windows hardware-encoder ranker (NVENC > AMF > QSV > software) is a
    // pure island — inject the `usable` predicate so no ffmpeg is spawned.
    #[cfg(windows)]
    mod windows_tier {
        use super::super::windows_resolve_encoder_id;

        /// Build a `usable` predicate that returns true only for the listed ids.
        fn only(avail: &'static [&'static str]) -> impl Fn(&str) -> bool {
            move |id: &str| avail.contains(&id)
        }

        #[test]
        fn honours_a_usable_preference() {
            let u = only(&["nvenc", "amf", "qsv"]);
            assert_eq!(windows_resolve_encoder_id("amf", None, &u), "amf");
            assert_eq!(windows_resolve_encoder_id("qsv", None, &u), "qsv");
            assert_eq!(windows_resolve_encoder_id("nvenc", None, &u), "nvenc");
        }

        #[test]
        fn software_preference_is_always_honoured() {
            let u = only(&["nvenc", "amf", "qsv"]);
            assert_eq!(windows_resolve_encoder_id("software", None, &u), "software");
        }

        #[test]
        fn an_unusable_preference_falls_back_in_rank_order() {
            // Preferred amf but only qsv works -> the fallback picks the best available.
            let u = only(&["qsv"]);
            assert_eq!(windows_resolve_encoder_id("amf", None, &u), "qsv");
            // Preferred nvenc, only amf + qsv work -> amf (ranked above qsv).
            let u = only(&["amf", "qsv"]);
            assert_eq!(windows_resolve_encoder_id("nvenc", None, &u), "amf");
        }

        #[test]
        fn auto_picks_the_ranked_best_available() {
            assert_eq!(
                windows_resolve_encoder_id("auto", None, &only(&["nvenc", "amf", "qsv"])),
                "nvenc"
            );
            assert_eq!(windows_resolve_encoder_id("auto", None, &only(&["amf", "qsv"])), "amf");
            assert_eq!(windows_resolve_encoder_id("auto", None, &only(&["qsv"])), "qsv");
            assert_eq!(windows_resolve_encoder_id("", None, &only(&["qsv"])), "qsv");
        }

        #[test]
        fn nothing_usable_is_software() {
            let none = only(&[]);
            assert_eq!(windows_resolve_encoder_id("nvenc", None, &none), "software");
            assert_eq!(windows_resolve_encoder_id("auto", None, &none), "software");
        }

        // DRAGON-571: the last-known-good hint leads the auto fallback but never
        // outranks a usable concrete preference, and a failed hint degrades to the
        // plain ranked order.
        #[test]
        fn auto_probes_a_usable_hint_first() {
            let u = only(&["nvenc", "qsv"]);
            assert_eq!(windows_resolve_encoder_id("auto", Some("qsv"), &u), "qsv");
        }

        #[test]
        fn auto_with_a_failed_hint_falls_back_in_rank_order() {
            let u = only(&["nvenc", "amf"]);
            assert_eq!(windows_resolve_encoder_id("auto", Some("qsv"), &u), "nvenc");
        }

        #[test]
        fn auto_ignores_a_software_hint() {
            // A software hint must never pin the CPU fallback (the DRAGON-571 bug).
            let u = only(&["nvenc"]);
            assert_eq!(windows_resolve_encoder_id("auto", Some("software"), &u), "nvenc");
        }

        #[test]
        fn a_usable_concrete_preference_beats_the_hint() {
            let u = only(&["nvenc", "amf", "qsv"]);
            assert_eq!(windows_resolve_encoder_id("amf", Some("qsv"), &u), "amf");
        }
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
        std::process::Command::new(crate::util::ffmpeg_path())
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

// DRAGON-571: the hint-first probe order, pinned as its own island. The ladder is
// injected so every hoist/ignore rule is provable on any host, on top of a shape
// check for this platform's real ladder.
#[cfg(test)]
mod auto_probe_order_tests {
    use super::{AUTO_LADDER, auto_probe_order};

    #[test]
    fn no_hint_keeps_the_ladder_rank() {
        assert_eq!(auto_probe_order(&["a", "b", "software"], None), ["a", "b", "software"]);
    }

    #[test]
    fn a_hardware_hint_is_hoisted_and_deduplicated() {
        // The hinted encoder moves to the front and vacates its ladder slot, so a
        // failing hint costs one probe, not two.
        assert_eq!(auto_probe_order(&["a", "b", "software"], Some("b")), ["b", "a", "software"]);
    }

    #[test]
    fn a_software_hint_is_ignored() {
        // Software always builds, so hoisting it would pin the CPU fallback forever,
        // the exact DRAGON-571 bug this ordering exists to remove.
        assert_eq!(
            auto_probe_order(&["a", "b", "software"], Some("software")),
            ["a", "b", "software"]
        );
    }

    #[test]
    fn an_unknown_hint_is_ignored() {
        // A foreign config's id (or a retired encoder) cannot derail the ladder.
        assert_eq!(auto_probe_order(&["a", "software"], Some("zz")), ["a", "software"]);
    }

    #[test]
    fn the_platform_ladder_ends_in_software() {
        // `resolve_hinted` relies on every order terminating in an encoder that
        // cannot fail to build.
        assert_eq!(AUTO_LADDER.last(), Some(&"software"));
        // And the hoist keeps that true for any non-software hint.
        for hint in AUTO_LADDER.iter().filter(|id| **id != "software") {
            assert_eq!(auto_probe_order(AUTO_LADDER, Some(hint)).last(), Some(&"software"));
        }
    }
}

// DRAGON-571: the write-back decision, pinned as its own island. The load-bearing
// rules: a concrete request writes NOTHING (user intent and the cache never cross),
// and only a previously-good hardware hint losing to software raises the warn.
#[cfg(test)]
mod auto_resolution_outcome_tests {
    use super::auto_resolution_outcome;

    #[test]
    fn a_concrete_request_writes_nothing() {
        // "nvenc" and "software" are concrete on every platform's ladder.
        for requested in ["nvenc", "software"] {
            let out = auto_resolution_outcome(requested, "software", Some("nvenc"));
            assert_eq!(out.new_hint, None, "requested={requested}");
            assert!(!out.hardware_hint_failed, "requested={requested}");
        }
    }

    #[test]
    fn an_auto_win_updates_a_changed_hint_only() {
        assert_eq!(
            auto_resolution_outcome("auto", "nvenc", None).new_hint.as_deref(),
            Some("nvenc")
        );
        assert_eq!(auto_resolution_outcome("auto", "nvenc", Some("nvenc")).new_hint, None);
    }

    #[test]
    fn auto_landing_on_software_is_recorded_but_only_a_lost_hardware_hint_warns() {
        // No hint yet (first run on a software-only box): record it, no warn.
        let first = auto_resolution_outcome("auto", "software", None);
        assert_eq!(first.new_hint.as_deref(), Some("software"));
        assert!(!first.hardware_hint_failed);
        // Stable software-only box: nothing to write, nothing to warn about.
        let stable = auto_resolution_outcome("auto", "software", Some("software"));
        assert_eq!(stable.new_hint, None);
        assert!(!stable.hardware_hint_failed);
        // Hardware worked before and lost to software now: the regression signature.
        let regressed = auto_resolution_outcome("auto", "software", Some("nvenc"));
        assert_eq!(regressed.new_hint.as_deref(), Some("software"));
        assert!(regressed.hardware_hint_failed);
    }

    #[test]
    fn an_unknown_request_is_treated_as_auto() {
        // A hand-edited or foreign id resolves through the ladder, so its winner is
        // cache-worthy exactly like a literal "auto".
        assert_eq!(
            auto_resolution_outcome("wxyz", "nvenc", None).new_hint.as_deref(),
            Some("nvenc")
        );
    }
}

/// DRAGON-669: the failed-probe reason is what a "recording no longer detects my GPU"
/// report is diagnosed from, so it has to survive intact and carry no filesystem.
#[cfg(test)]
mod probe_failure_reason_tests {
    use super::probe_failure_reason;

    #[test]
    fn the_driver_refusal_survives_verbatim() {
        // The line that actually settles the two open field reports.
        let out = probe_failure_reason(
            "[h264_nvenc @ 0000021c] The minimum required Nvidia driver for nvenc is 570.0 \
             or newer\nError initializing output stream: Error while opening encoder",
        );
        assert!(out.contains("minimum required Nvidia driver"), "{out}");
        assert!(out.contains("Error while opening encoder"), "{out}");
    }

    #[test]
    fn only_the_last_lines_are_kept_and_they_join_readably() {
        let out = probe_failure_reason("one\ntwo\nthree\nfour\nfive");
        assert_eq!(out, "three; four; five");
    }

    #[test]
    fn blank_lines_are_not_spent_from_the_budget() {
        // ffmpeg pads its error blocks; a run of newlines must not push the real
        // cause out of the tail.
        let out = probe_failure_reason("the real cause\n\n\n\n");
        assert_eq!(out, "the real cause");
    }

    #[test]
    fn silence_is_reported_as_silence() {
        // Distinguishable in the log from "we forgot to log anything".
        assert_eq!(probe_failure_reason("   \n\n"), "(ffmpeg printed nothing)");
    }

    #[test]
    fn a_path_in_the_reason_is_redacted() {
        // The privacy rule is hard: the debug log records what the app DID, never
        // what the user captured or where it lives.
        let out = probe_failure_reason("Unable to open C:\\Users\\Jane Smith\\Videos\\standup.mp4");
        assert!(!out.contains("Jane Smith"), "{out}");
        assert!(!out.contains("standup"), "{out}");
    }
}
