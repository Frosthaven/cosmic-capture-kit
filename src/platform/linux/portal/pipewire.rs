//! In-process consumers of a portal ScreenCast PipeWire stream (via `pipewire-rs`,
//! binding the system libpipewire). Given the remote fd + node id from
//! [`crate::platform::screencast`], negotiates a raw-RGB video format and delivers
//! each frame — cropped to a region when asked — as tightly-packed RGBA to a callback.
//!
//! Runs the PipeWire main loop on the calling thread (spawn a dedicated one) and
//! returns when `stop` is set. Buffers are requested CPU-mapped (`MAP_BUFFERS`),
//! so no DMA-BUF import is needed; frames a source only offers as DMA-BUF are
//! skipped.
//!
//! ## The stream that never starts (DRAGON-714)
//!
//! [`consume_frames`] watches for one thing besides frames: whether the stream ever
//! reaches Streaming at all. Before this watch existed the only thing that spoke was the
//! recording worker's generic 10s "no frames from the PipeWire stream", which names a
//! symptom and not a cause, and the recording was lost either way.
//!
//! A failed negotiation can arrive two ways, and this path handles both. MEASURED on
//! cosmic-comp with PipeWire 1.6.8: it walks `Connecting -> Paused -> Error("no more
//! output formats")`, with the error 4ms after the connect, so the stream DOES report it
//! here. On a producer or version that does not report it, the stream simply sits in
//! Paused and only the clock can tell. "Never reached Streaming" is the condition that
//! covers both; the error is an early exit from the same wait, not a separate mechanism.
//!
//! The rules (how long to wait, which transitions count, what to say) are PURE and live
//! in the shared tree, unit-tested on every host: `platform::pipewire_stream_watch`,
//! `platform::pipewire_stall_reason`, `platform::PW_STREAMING_BUDGET`. Only the listener
//! and the loop timer are here.
//!
//! [`consume_dmabuf`] deliberately does NOT carry the watch: a zero-copy attempt that
//! never delivers is a DECLINE, bounded by its caller's own first-frame budget and
//! answered by re-running the session on the CPU path (`record::zero_copy`), so it costs
//! a fallback rather than a recording. This path is the one where the silence is final.

use crate::errln;

use pipewire as pw;
use pw::spa;
use spa::pod::Pod;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::pixfmt::{bytes_per_pixel, convert_crop};
#[cfg(feature = "zero-copy")]
use super::pixfmt::drm_fourcc;

/// Crop in stream pixels: `(x, y, w, h)`. `None` delivers the whole frame.
pub type Crop = Option<(u32, u32, u32, u32)>;

/// DRAGON-562 alpha probe (`CCK_ALPHA_PROBE=1`), inert unless set: measure
/// whether a portal stream can deliver real per-pixel alpha, BEFORE
/// `convert_crop` forces every pixel opaque. Two effects while set: the format
/// request offers the alpha-carrying formats FIRST (BGRA/RGBA before
/// BGRx/RGBx), and the first few delivered frames print a raw byte-3 histogram
/// to stderr (`pixfmt::alpha_histogram`). Run it as
/// `CCK_ALPHA_PROBE=1 cosmic-capture-kit --test pw window`, picking a
/// TRANSLUCENT window in the portal dialog. The answer gates flipping the
/// portal backend's `transparency` capability; an experiment variable is
/// temporary by contract — delete this hook the moment the answer is measured.
pub const ALPHA_PROBE_ENV: &str = "CCK_ALPHA_PROBE";

/// DRAGON-714 test hook (`CCK_TEST_PW_BREAK_NEGOTIATION=1`), inert unless set: ask for a
/// video size no producer can meet (a fixed 17x17 rectangle instead of the usual range),
/// so the format intersection is empty and the stream never reaches Streaming. That is the
/// exact shape of the failure the negotiation watch exists to catch, and there is no other
/// way to reach it on a healthy desktop.
///
/// It is a HOOK, never a code path: the normal request is byte-identical with the variable
/// unset. Production must keep sending a `Choice::Range` and must never send a fixed
/// smaller rectangle, because SPA compares rectangles by AREA, so a narrower request
/// constrains neither axis and an empty intersection is a lost recording rather than a
/// fallback (the whole finding this ticket's research landed on).
pub const TEST_BREAK_NEGOTIATION_ENV: &str = "CCK_TEST_PW_BREAK_NEGOTIATION";

/// What the `state_changed` listener has seen, read by the loop timer that judges it. Both
/// run on the PipeWire loop thread; the mutex is for the borrow checker, not contention.
#[derive(Default)]
struct NegotiationWatch {
    /// The phase the stream is in NOW.
    phase: Option<crate::platform::PwStreamPhase>,
    /// Whether Streaming was EVER reached. Latches: it disarms the watch for good.
    reached_streaming: bool,
    /// The stream's own error message, when it had one. A negotiation failure has none.
    error: Option<String>,
}

struct UserData<F> {
    format: spa::param::video::VideoInfoRaw,
    crop: Crop,
    out: Vec<u8>,
    on_frame: F,
    /// Frames skipped without reaching the callback (not CPU-mapped, bad chunk,
    /// pre-negotiation) — a persistent run of these is a FROZEN recording (video
    /// stops while audio continues), so they must never be silent.
    skips: u64,
    /// [`ALPHA_PROBE_ENV`]: histogram the raw byte-3 channel of the first few
    /// frames before conversion. Inert (false) unless the env is set.
    alpha_probe: bool,
    /// Frames the probe has reported so far (it stops after a handful).
    alpha_frames: u32,
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

    let alpha_probe = std::env::var_os(ALPHA_PROBE_ENV).is_some();
    let data = UserData {
        format: Default::default(),
        crop,
        out: Vec::new(),
        on_frame,
        skips: 0,
        alpha_probe,
        alpha_frames: 0,
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

    // DRAGON-714: what the stream is doing, for the negotiation watch below. Shared with
    // the loop timer, which is the only thing that judges it.
    let watch: Arc<Mutex<NegotiationWatch>> = Arc::new(Mutex::new(NegotiationWatch::default()));
    let watch_cb = watch.clone();

    let _listener = stream
        .add_local_listener_with_user_data(data)
        // DRAGON-714. What this listener is really for is recording whether Streaming was
        // ever reached, which is the one signal available on every producer. It ALSO
        // captures the error text when the stream reports one, which cosmic-comp does (see
        // the module doc): that turns a five second wait into a 4ms diagnosis carrying the
        // compositor's own words.
        .state_changed(move |_, _, old, new| {
            use crate::platform::PwStreamPhase as Phase;
            let phase = match &new {
                pw::stream::StreamState::Unconnected => Phase::Unconnected,
                pw::stream::StreamState::Connecting => Phase::Connecting,
                pw::stream::StreamState::Paused => Phase::Paused,
                pw::stream::StreamState::Streaming => Phase::Streaming,
                pw::stream::StreamState::Error(_) => Phase::Error,
            };
            if let Ok(mut w) = watch_cb.lock() {
                w.phase = Some(phase);
                w.error = match &new {
                    pw::stream::StreamState::Error(e) => Some(e.clone()),
                    _ => None,
                };
                if phase == Phase::Streaming {
                    w.reached_streaming = true;
                }
            }
            // Behaviour, never content: two stream states and nothing about what is on
            // screen. This is also the only record anywhere of a negotiation that failed.
            log::debug!("pipewire capture: stream state {old:?} -> {new:?}");
        })
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
            // DRAGON-714: what we ASKED for against what we GOT, once per session. On every
            // Linux compositor shipping today this is the producer's fixed size and our
            // request never had a say, so this line is the only thing that will ever tell
            // us the day that changes.
            log::debug!(
                "pipewire capture: negotiated {}x{} fmt {:?}",
                ud.format.size().width,
                ud.format.size().height,
                ud.format.format(),
            );
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
            // DRAGON-562 alpha probe: read the RAW byte-3 channel BEFORE
            // convert_crop overwrites it with 255 (first frames only; stderr,
            // like the dmabuf debug hook above).
            if ud.alpha_probe && ud.alpha_frames < 5 {
                ud.alpha_frames += 1;
                report_alpha_probe(src, stride, fmt, bpp, (cx, cy, cw, ch), ud.alpha_frames);
            }
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
    //
    // DRAGON-562 alpha probe: while probing, offer the alpha-carrying formats
    // FIRST so a compositor that CAN hand per-window alpha will; the normal
    // request keeps BGRx first (what every compositor offers for monitors) and
    // is byte-identical with the env unset.
    let fmt_prop = if alpha_probe {
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::RGBA,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::RGBx,
            spa::param::video::VideoFormat::RGB,
        )
    } else {
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
        )
    };
    // The size request stays a `Choice::Range`: it costs nothing against a producer that
    // offers one fixed size (every Linux compositor shipping today), and it cannot break
    // negotiation. The `CCK_TEST_PW_BREAK_NEGOTIATION` arm asks for something no producer
    // can meet, on purpose, to reach the failure the watch below exists for.
    let size_prop = if std::env::var_os(TEST_BREAK_NEGOTIATION_ENV).is_some() {
        log::warn!(
            "pipewire capture: {TEST_BREAK_NEGOTIATION_ENV} is set — requesting an \
             impossible 17x17 video size so this stream's negotiation FAILS. This is a \
             test hook; unset it for a working capture."
        );
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Rectangle,
            spa::utils::Rectangle { width: 17, height: 17 }
        )
    } else {
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 8192, height: 8192 }
        )
    };
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
        fmt_prop,
        size_prop,
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

    // DRAGON-714: the negotiation clock starts HERE, at the connect, because that is what
    // the budget is measured against.
    let connected_at = Instant::now();
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| e.to_string())?;

    // Filled by the timer below when it gives up on a stream that never started, and read
    // once the loop has quit. `None` means the loop ended for any ordinary reason.
    let stall: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Poll the stop flag on the loop thread and quit the loop when it's set; and, until the
    // stream has provably started, judge the negotiation watch on the same tick (DRAGON-714).
    let ml = mainloop.clone();
    let stop_check = stop.clone();
    let watch_timer = watch.clone();
    let stall_timer = stall.clone();
    // Latches false the moment the watch settles, so a healthy session stops touching the
    // mutex for the rest of the recording. An atomic because the loop hands the callback
    // out as `Fn`, not `FnMut`.
    let armed = AtomicBool::new(true);
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop_check.load(Ordering::Relaxed) {
            ml.quit();
            return;
        }
        if !armed.load(Ordering::Relaxed) {
            return;
        }
        let Ok(w) = watch_timer.lock() else { return };
        // No transition seen yet is `Unconnected`: the state a fresh stream starts in.
        let phase = w.phase.unwrap_or(crate::platform::PwStreamPhase::Unconnected);
        let (reached, error) = (w.reached_streaming, w.error.clone());
        drop(w);
        match crate::platform::pipewire_stream_watch(
            connected_at.elapsed(),
            crate::platform::PW_STREAMING_BUDGET,
            reached,
            phase,
        ) {
            crate::platform::PwStreamWatch::Settled => armed.store(false, Ordering::Relaxed),
            crate::platform::PwStreamWatch::Waiting => {}
            crate::platform::PwStreamWatch::GiveUp => {
                armed.store(false, Ordering::Relaxed);
                let reason = crate::platform::pipewire_stall_reason(
                    crate::platform::PW_STREAMING_BUDGET,
                    phase,
                    error.as_deref(),
                );
                log::error!("pipewire capture: {reason}");
                if let Ok(mut g) = stall_timer.lock() {
                    *g = Some(reason);
                }
                // Ending the loop drops the frame sender with it, so a caller waiting on
                // the channel sees the disconnect at once instead of sitting out its own
                // (outer, longer) first-frame budget.
                ml.quit();
            }
        }
    });
    let _ = timer.update_timer(
        Some(Duration::from_millis(100)),
        Some(Duration::from_millis(100)),
    );

    mainloop.run();
    // A stream that never started is a NAMED failure, not a silent end (DRAGON-714). The
    // recording worker repeats this string into `Failure::RecordingFailed`, where it is the
    // whole diagnosis the user gets.
    match stall.lock().ok().and_then(|mut g| g.take()) {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

/// DRAGON-562 alpha probe: print one frame's raw byte-3 histogram to stderr,
/// labelled with whether the negotiated format makes that byte REAL alpha
/// (BGRA/RGBA) or padding (BGRx/RGBx — the structural "no alpha negotiated"
/// answer). The counting itself is `pixfmt::alpha_histogram` (pure, tested);
/// this is only the report. Behavior-only output: format, dimensions, counts —
/// never pixel content.
fn report_alpha_probe(
    src: &[u8],
    stride: usize,
    fmt: spa::param::video::VideoFormat,
    bpp: usize,
    crop: (u32, u32, u32, u32),
    n: u32,
) {
    use spa::param::video::VideoFormat as V;
    let (cx, cy, cw, ch) = crop;
    let channel = match fmt {
        V::BGRA | V::RGBA => "a REAL alpha channel",
        V::BGRx | V::RGBx => "a PADDING byte (no alpha format negotiated)",
        _ => {
            errln!("alpha-probe: frame {n}: fmt {fmt:?} carries no fourth byte to read");
            return;
        }
    };
    match super::pixfmt::alpha_histogram(src, stride, bpp, cx, cy, cw, ch) {
        Some((zero, partial, opaque)) => errln!(
            "alpha-probe: frame {n}: fmt {fmt:?} (byte 3 is {channel}) {cw}x{ch}px: \
             alpha==0: {zero}  0<alpha<255: {partial}  alpha==255: {opaque}"
        ),
        None => errln!("alpha-probe: frame {n}: fmt {fmt:?}: bpp {bpp} != 4, nothing to read"),
    }
}

/// DRM_FORMAT_MOD_INVALID — "implicit modifier": ask the compositor for a dmabuf and
/// let it pick the tiling, reporting it back. The simplest negotiation that avoids
/// the explicit modifier-list dance (callers verify on real hardware).
///
/// DRAGON-425 unified the three copies of this literal behind one ungated const; the alias
/// keeps every use site below reading exactly as it did.
#[cfg(feature = "zero-copy")]
const DRM_MOD_INVALID: u64 = crate::encode::DRM_FORMAT_MOD_INVALID;

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
                errln!(
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
                        errln!(
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
                errln!("dmabuf-debug: first buffer data types = [{}]", types.join(", "));
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

/// The wait [`grab_frame`] gives a stalled stream. Generous on purpose: both of its
/// callers are seeding a frozen backdrop the session cannot proceed without, so waiting
/// beats showing nothing.
pub const GRAB_FRAME_BUDGET: Duration = Duration::from_secs(5);

/// Grab a single frame from `node_id` on the portal `fd`, cropped per `crop`, as
/// an RGBA image. Blocks on the calling thread (run it off the UI thread). Returns
/// `None` if no frame arrives within [`GRAB_FRAME_BUDGET`].
pub fn grab_frame(fd: OwnedFd, node_id: u32, crop: Crop) -> Option<image::RgbaImage> {
    grab_frame_within(fd, node_id, crop, GRAB_FRAME_BUDGET)
}

/// [`grab_frame`] with the caller's own bound (DRAGON-737).
///
/// Split out because the five second default is right for a backdrop seed and wrong for
/// anything on a path the user is waiting on: DRAGON-737's tie-break runs between the
/// portal dialog closing and the countdown starting, where five seconds of silence would
/// read as a hang. The rule the split serves is DRAGON-118's, that a wait states its own
/// deadline; this one lets the CALLER state it.
///
/// The stop flag is sampled by a 100ms loop timer, so the real ceiling is `budget` plus
/// one tick. Callers with a tight budget should count on that.
pub fn grab_frame_within(
    fd: OwnedFd,
    node_id: u32,
    crop: Crop,
    budget: Duration,
) -> Option<image::RgbaImage> {
    let stop = Arc::new(AtomicBool::new(false));
    let got: Arc<std::sync::Mutex<Option<image::RgbaImage>>> =
        Arc::new(std::sync::Mutex::new(None));
    // Watchdog so a stalled stream doesn't block forever.
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(budget);
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
