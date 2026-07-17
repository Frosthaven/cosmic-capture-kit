//! In-process consumers of a portal ScreenCast PipeWire stream (via `pipewire-rs`,
//! binding the system libpipewire). Given the remote fd + node id from
//! [`crate::platform::screencast`], negotiates a raw-RGB video format and delivers
//! each frame — cropped to a region when asked — as tightly-packed RGBA to a callback.
//!
//! Runs the PipeWire main loop on the calling thread (spawn a dedicated one) and
//! returns when `stop` is set. Buffers are requested CPU-mapped (`MAP_BUFFERS`),
//! so no DMA-BUF import is needed; frames a source only offers as DMA-BUF are
//! skipped.

use pipewire as pw;
use pw::spa;
use spa::pod::Pod;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::pixfmt::{bytes_per_pixel, convert_crop};
#[cfg(feature = "zero-copy")]
use super::pixfmt::drm_fourcc;

/// Crop in stream pixels: `(x, y, w, h)`. `None` delivers the whole frame.
pub type Crop = Option<(u32, u32, u32, u32)>;

struct UserData<F> {
    format: spa::param::video::VideoInfoRaw,
    crop: Crop,
    out: Vec<u8>,
    on_frame: F,
    /// Frames skipped without reaching the callback (not CPU-mapped, bad chunk,
    /// pre-negotiation) — a persistent run of these is a FROZEN recording (video
    /// stops while audio continues), so they must never be silent.
    skips: u64,
}

impl<F> UserData<F> {
    /// Log a skipped frame: the first occurrence and every 128th after it (a stall
    /// at 30fps logs ~every 4s), with the negotiated format for diagnosis.
    fn skipped(&mut self, why: &str) {
        if self.skips.is_multiple_of(128) {
            log::warn!(
                "pipewire capture: skipping frame ({why}); {} skipped so far \
                 (negotiated {}x{} fmt {:?}) — a persistent run of these freezes \
                 the recording's video while audio continues",
                self.skips + 1,
                self.format.size().width,
                self.format.size().height,
                self.format.format(),
            );
        }
        self.skips += 1;
    }
}

/// Consume `node_id` from the portal `fd`, delivering each frame's RGBA to
/// `on_frame(rgba, w, h, pts, pw_delay_ns)` (tightly packed, `w*h*4` bytes). `pts` is
/// the frame's capture timestamp (CLOCK_MONOTONIC ns) or 0 if absent; `pw_delay_ns`
/// is PipeWire's reported source latency or 0 — either is used to measure A/V lag.
/// Blocks until `stop` is set or the stream ends. `crop` (stream pixels) trims a
/// region; `None` = full.
pub fn consume_frames<F>(
    fd: OwnedFd,
    node_id: u32,
    crop: Crop,
    stop: Arc<AtomicBool>,
    on_frame: F,
) -> Result<(), String>
where
    F: FnMut(Vec<u8>, u32, u32, i64, i64) + 'static,
{
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
    let core = context
        .connect_fd_rc(fd, None)
        .map_err(|e| format!("pipewire connect_fd: {e}"))?;

    let data = UserData {
        format: Default::default(),
        crop,
        out: Vec::new(),
        on_frame,
        skips: 0,
    };

    let stream = pw::stream::StreamBox::new(
        &core,
        "cosmic-capture-kit",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
        },
    )
    .map_err(|e| e.to_string())?;

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(|_, ud, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = spa::param::format_utils::parse_format(param)
            else {
                return;
            };
            if media_type != spa::param::format::MediaType::Video
                || media_subtype != spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            let _ = ud.format.parse(param);
        })
        .process(|stream, ud| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            // Capture timestamp (CLOCK_MONOTONIC ns) for A/V-latency measurement; 0
            // when the compositor doesn't fill the header.
            let pts = buffer
                .find_meta::<spa::buffer::meta::MetaHeader>()
                .map_or(0, |h| h.pts());
            // PipeWire's own reported latency to the source ("delay" in ticks, scaled
            // by the stream rate). The alternative A/V-latency signal when pts is 0.
            let pw_delay_ns = stream.time().ok().map_or(0, |t| {
                let r = t.rate();
                if r.denom == 0 {
                    0
                } else {
                    (t.delay() as i128 * r.num as i128 * 1_000_000_000 / r.denom as i128) as i64
                }
            });
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                ud.skipped("buffer carries no data planes");
                return;
            }
            let d = &mut datas[0];
            let stride = d.chunk().stride().max(0) as usize;
            // The valid frame starts at the chunk's offset within the mapped buffer,
            // not necessarily at byte 0 (some producers rotate frames through one
            // mapping at varying offsets). Reading from 0 yields stale/frozen pixels.
            let offset = d.chunk().offset() as usize;
            let fw = ud.format.size().width;
            let fh = ud.format.size().height;
            let fmt = ud.format.format();
            if fw == 0 || fh == 0 || stride == 0 {
                ud.skipped("format/stride not negotiated yet");
                return;
            }
            let Some(src) = d.data() else {
                // Not CPU-mapped (e.g. the producer switched to DMA-BUF-only
                // buffers) — nothing this path can read. If this persists, the
                // recording's video freezes while audio continues, so shout.
                ud.skipped("frame not CPU-mapped (DMA-BUF only?)");
                return;
            };
            // Honour the chunk offset; bail if it's out of range for the mapping.
            let src = match src.get(offset..) {
                Some(s) if s.len() >= stride => s,
                _ => {
                    ud.skipped("chunk offset out of range for the mapping");
                    return;
                }
            };
            // Crop, clamped to the frame.
            let (cx, cy, mut cw, mut ch) = ud.crop.unwrap_or((0, 0, fw, fh));
            let cx = cx.min(fw);
            let cy = cy.min(fh);
            cw = cw.min(fw - cx);
            ch = ch.min(fh - cy);
            // Even dimensions — h264/hevc need them, and it spares the encoder a
            // rescale when the source is already at the target size.
            cw &= !1;
            ch &= !1;
            if cw == 0 || ch == 0 {
                return;
            }
            let Some(bpp) = bytes_per_pixel(fmt) else {
                return;
            };
            let need = (cw * ch * 4) as usize;
            if ud.out.len() != need {
                ud.out.resize(need, 0);
            }
            convert_crop(src, stride, fmt, bpp, cx, cy, cw, ch, &mut ud.out);
            // Hand the converted frame out BY VALUE: the consumer keeps it with no
            // second full-frame copy inside this callback (a 5120-wide frame is
            // ~30MB — the callback must stay fast or the compositor's screencast
            // throttles). The next frame re-allocates `out` via the resize above.
            let out = std::mem::take(&mut ud.out);
            (ud.on_frame)(out, cw, ch, pts, pw_delay_ns);
        })
        .register()
        .map_err(|e| e.to_string())?;

    // Request a raw-RGB format (we convert to RGBA ourselves); the compositor picks
    // the size/framerate.
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::RGBx,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::RGBA,
            spa::param::video::VideoFormat::RGB,
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 8192, height: 8192 }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 60, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 240, denom: 1 }
        ),
    );
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| e.to_string())?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).ok_or("invalid format pod")?];

    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| e.to_string())?;

    // Poll the stop flag on the loop thread and quit the loop when it's set.
    let ml = mainloop.clone();
    let stop_check = stop.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop_check.load(Ordering::Relaxed) {
            ml.quit();
        }
    });
    let _ = timer.update_timer(
        Some(Duration::from_millis(100)),
        Some(Duration::from_millis(100)),
    );

    mainloop.run();
    Ok(())
}

/// DRM_FORMAT_MOD_INVALID — "implicit modifier": ask the compositor for a dmabuf and
/// let it pick the tiling, reporting it back. The simplest negotiation that avoids
/// the explicit modifier-list dance (callers verify on real hardware).
#[cfg(feature = "zero-copy")]
const DRM_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// A captured DMA-BUF frame: per-plane `(fd, offset, stride)` plus the DRM format and
/// modifier, for zero-copy import into a GPU encoder. The fds are owned by PipeWire
/// (valid only for the callback); the consumer must encode before returning.
#[cfg(feature = "zero-copy")]
pub struct DmabufFrame<'a> {
    pub planes: &'a [(std::os::fd::RawFd, u32, u32)],
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
}

#[cfg(feature = "zero-copy")]
struct DmabufUserData<F> {
    format: spa::param::video::VideoInfoRaw,
    planes: Vec<(std::os::fd::RawFd, u32, u32)>,
    on_frame: F,
    /// `CCK_DMABUF_DEBUG=1`: log the negotiated format + the buffer's data type, to
    /// tell "cosmic gave shm" apart from "our dmabuf negotiation was insufficient".
    debug: bool,
    logged: bool,
    /// DONT_FIXATE handshake: set once we've re-sent the format pinned to a single
    /// concrete modifier, so we only re-fixate on the compositor's first reply.
    fixated: bool,
}

/// Build an EnumFormat pod that pins the negotiated format to a single, concrete
/// modifier (no DONT_FIXATE) so the compositor can allocate dmabufs. Serialized
/// bytes; the caller wraps them in a `Pod` for `update_params`.
#[cfg(feature = "zero-copy")]
fn fixated_format_pod(info: &spa::param::video::VideoInfoRaw) -> Option<Vec<u8>> {
    let size = info.size();
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(spa::param::format::FormatProperties::VideoFormat, Id, info.format()),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoModifier,
            Long,
            info.modifier() as i64
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Rectangle,
            spa::utils::Rectangle { width: size.width, height: size.height }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 60, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 240, denom: 1 }
        ),
    );
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .ok()
    .map(|r| r.0.into_inner())
}

/// Like [`consume_frames`], but negotiates **DMA-BUF** buffers and delivers each
/// frame's plane fds (zero-copy) instead of a CPU-mapped RGBA copy. Used only by
/// the opt-in GPU zero-copy recording path; the compositor must agree to dmabuf,
/// else no frames arrive and the caller falls back to [`consume_frames`].
/// Experimental — validate on hardware.
#[cfg(feature = "zero-copy")]
pub fn consume_dmabuf<F>(
    fd: OwnedFd,
    node_id: u32,
    stop: Arc<AtomicBool>,
    on_frame: F,
) -> Result<(), String>
where
    F: FnMut(DmabufFrame) + 'static,
{
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
    let core = context
        .connect_fd_rc(fd, None)
        .map_err(|e| format!("pipewire connect_fd: {e}"))?;

    let data = DmabufUserData {
        format: Default::default(),
        planes: Vec::new(),
        on_frame,
        debug: std::env::var_os("CCK_DMABUF_DEBUG").is_some(),
        logged: false,
        fixated: false,
    };

    let stream = pw::stream::StreamBox::new(
        &core,
        "cosmic-capture-kit-zc",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
        },
    )
    .map_err(|e| e.to_string())?;

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(|stream, ud, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((mt, ms)) = spa::param::format_utils::parse_format(param) else {
                return;
            };
            if mt != spa::param::format::MediaType::Video
                || ms != spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            let _ = ud.format.parse(param);
            if ud.debug {
                eprintln!(
                    "dmabuf-debug: negotiated {:?} {}x{} modifier=0x{:016x}",
                    ud.format.format(),
                    ud.format.size().width,
                    ud.format.size().height,
                    ud.format.modifier(),
                );
            }
            // DONT_FIXATE handshake: our EnumFormat offered the modifier with the
            // DONT_FIXATE flag, so the compositor replies with the modifier(s) it can
            // produce but allocates NOTHING until we pin exactly one. On that first
            // reply, re-send the format fixed to the concrete modifier it chose; the
            // next param_changed carries the fixated format and buffers start flowing.
            // (An implicit/INVALID reply means it offered no concrete modifier — leave
            // it; the recorder's no-frames guard then falls back to the CPU path.)
            if !ud.fixated && ud.format.modifier() != DRM_MOD_INVALID {
                ud.fixated = true;
                if let Some(bytes) = fixated_format_pod(&ud.format)
                    && let Some(pod) = spa::pod::Pod::from_bytes(&bytes)
                {
                    let mut params = [pod];
                    let _ = stream.update_params(&mut params);
                    if ud.debug {
                        eprintln!(
                            "dmabuf-debug: re-sent fixated modifier=0x{:016x}",
                            ud.format.modifier()
                        );
                    }
                }
            }
        })
        .process(|stream, ud| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            if ud.debug && !ud.logged {
                ud.logged = true;
                let types: Vec<String> =
                    buffer.datas_mut().iter().map(|d| format!("{:?}", d.type_())).collect();
                eprintln!("dmabuf-debug: first buffer data types = [{}]", types.join(", "));
            }
            let fw = ud.format.size().width;
            let fh = ud.format.size().height;
            let fmt = ud.format.format();
            if fw == 0 || fh == 0 {
                return;
            }
            let Some(fourcc) = drm_fourcc(fmt) else {
                return;
            };
            // Collect each plane's dmabuf fd + offset + stride. A non-dmabuf buffer
            // (compositor gave shm) has no usable fd here → skip (caller falls back).
            ud.planes.clear();
            for d in buffer.datas_mut() {
                if d.type_() != spa::buffer::DataType::DmaBuf {
                    ud.planes.clear();
                    return;
                }
                let raw_fd = d.as_raw().fd as std::os::fd::RawFd;
                if raw_fd < 0 {
                    ud.planes.clear();
                    return;
                }
                let offset = d.chunk().offset();
                let stride = d.chunk().stride().max(0) as u32;
                ud.planes.push((raw_fd, offset, stride));
            }
            if ud.planes.is_empty() {
                return;
            }
            // The compositor reports the real (fixated) modifier on the negotiated
            // format; the import needs it to interpret the buffer's tiling.
            (ud.on_frame)(DmabufFrame {
                planes: &ud.planes,
                fourcc,
                modifier: ud.format.modifier(),
                width: fw,
                height: fh,
            });
        })
        .register()
        .map_err(|e| e.to_string())?;

    // Request raw video AND advertise dmabuf via the modifier property. No
    // MAP_BUFFERS: we want the dmabuf fd, not a CPU map. The modifier is flagged
    // DONT_FIXATE so the compositor negotiates a CONCRETE modifier — a single
    // implicit value makes it agree on the format but allocate zero buffers. We
    // offer "any" (INVALID) and pin its choice in the param_changed handshake.
    let mut obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::RGBx,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::RGBA,
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 8192, height: 8192 }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 60, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 240, denom: 1 }
        ),
    );
    obj.properties.push(spa::pod::Property {
        key: spa::param::format::FormatProperties::VideoModifier.as_raw(),
        // SPA_POD_PROP_FLAG_DONT_FIXATE (1<<4) is feature-gated (v0_3_33) in the
        // bindings; use the raw bit so we don't depend on that cargo feature.
        flags: spa::pod::PropertyFlags::MANDATORY
            | spa::pod::PropertyFlags::from_bits_retain(1 << 4),
        value: spa::pod::Value::Choice(spa::pod::ChoiceValue::Long(spa::utils::Choice(
            spa::utils::ChoiceFlags::empty(),
            spa::utils::ChoiceEnum::Enum {
                default: DRM_MOD_INVALID as i64,
                alternatives: vec![DRM_MOD_INVALID as i64],
            },
        ))),
    });
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| e.to_string())?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).ok_or("invalid format pod")?];

    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT,
            &mut params,
        )
        .map_err(|e| e.to_string())?;

    let ml = mainloop.clone();
    let stop_check = stop.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop_check.load(Ordering::Relaxed) {
            ml.quit();
        }
    });
    let _ = timer.update_timer(
        Some(Duration::from_millis(100)),
        Some(Duration::from_millis(100)),
    );

    mainloop.run();
    Ok(())
}

/// Grab a single frame from `node_id` on the portal `fd`, cropped per `crop`, as
/// an RGBA image. Blocks on the calling thread (run it off the UI thread). Returns
/// `None` if no frame arrives within a few seconds.
pub fn grab_frame(fd: OwnedFd, node_id: u32, crop: Crop) -> Option<image::RgbaImage> {
    let stop = Arc::new(AtomicBool::new(false));
    let got: Arc<std::sync::Mutex<Option<image::RgbaImage>>> =
        Arc::new(std::sync::Mutex::new(None));
    // Watchdog so a stalled stream doesn't block forever.
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(5));
            stop.store(true, Ordering::Relaxed);
        });
    }
    let stop_cb = stop.clone();
    let got_cb = got.clone();
    let _ = consume_frames(fd, node_id, crop, stop, move |rgba, w, h, _pts, _delay| {
        if let Ok(mut g) = got_cb.lock()
            && g.is_none()
        {
            *g = image::RgbaImage::from_raw(w, h, rgba);
            stop_cb.store(true, Ordering::Relaxed);
        }
    });
    got.lock().ok().and_then(|mut g| g.take())
}
