//! GPU zero-copy recording paths (`#[cfg(feature = "zero-copy")]`): import DMA-BUF
//! frames straight into an in-process encoder with no CPU readback. Covers the
//! PipeWire portal stream and full-output screencopy capture, plus the screencopy
//! DMA-BUF diagnostic.

use cosmic_client_toolkit::screencopy::{
    CaptureOptions, CaptureSession, CaptureSource, Formats, Rect, ScreencopyFrameData,
    ScreencopySessionData,
};
use cosmic_client_toolkit::sctk::dmabuf::DmabufState;
use crate::screencopy::{ScreencopyClient, connect, outputs};
use super::ToggleEvent;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use wayland_client::protocol::wl_buffer;
use wayland_client::{Connection, EventQueue, QueueHandle};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1;

/// Experimental GPU zero-copy PipeWire recording: negotiate DMA-BUF frames, encode
/// them in-process on the GPU (no CPU readback), and pipe the compressed packets to
/// ffmpeg which only muxes the audio (`-c:v copy`). Returns `Err` (so the caller can
/// fall back to the CPU path) when there's no VAAPI device, or the stream never
/// delivers a usable dmabuf frame within the watchdog, or the GPU encode fails.
///
/// Honours the max-resolution cap and the codec's side limit by downscaling on the
/// GPU (`scale_vaapi`) inside the encoder. Limitations of this first cut: full-stream
/// only (no region crop — those use the CPU path) and the A/V auto-calibration isn't
/// updated (the saved offset still applies). VAAPI only.
///
/// The audio-side pre-flight check ([`super::owned::try_start_owned_audio`]) runs
/// FIRST, before this function touches the portal `fd`/`node_id` at all, so a failure
/// here never risks them — it fails the recording outright with a named, actionable
/// reason instead of falling back (DRAGON-127 retired the legacy wallclock+CFR+segments
/// recorder this used to fall back to).
#[cfg(feature = "zero-copy")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_pipewire_zero_copy(
    fd: OwnedFd,
    node_id: u32,
    fps: u32,
    codec: &str,
    max_res: (u32, u32),
    mic: bool,
    system_audio: bool,
    bitrate_kbps: u32,
    audio_offset_ms: i32,
    auto_device_compensation: bool,
    out_path: &std::path::Path,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    events: &Mutex<Vec<ToggleEvent>>,
    dims: Arc<Mutex<Option<(u32, u32)>>>,
    metadata: &str,
) -> Result<PathBuf, String> {
    match super::owned::try_start_owned_audio() {
        Ok(owned) => {
            log::info!("zero-copy pipeline: media-clock owned path (DRAGON-127)");
            record_pipewire_zero_copy_owned(
                fd, node_id, fps, codec, max_res, mic, system_audio, bitrate_kbps,
                audio_offset_ms, auto_device_compensation, out_path, stop, paused, events, dims,
                metadata, owned,
            )
        }
        Err(reason) => {
            log::error!("zero-copy pipeline: audio pre-flight failed ({reason}); cannot record");
            Err(format!("could not start recording audio: {reason}"))
        }
    }
}

/// The media-clock owned GPU zero-copy PipeWire session (DRAGON-127): ONE
/// continuous encoder + muxer for the whole recording instead of the legacy
/// per-pause-segment model — audio rendered by [`super::pump`]'s `Mixer`-backed
/// engine through plain FIFOs (`spawn_ffmpeg_encoded_media_clock`), matching what
/// `record::pipewire::record_pipewire_owned` does for the raw-frame path. The video
/// side stays event-driven (fed only when the portal actually delivers a dmabuf
/// frame — there is no video-ticker here, unlike the CPU owned paths): a pause
/// simply stops feeding the encoder (never resets it, never reopens the muxer), so
/// video's total encoded length is naturally "wall time minus paused time" — the
/// same media-time invariant the pump's clock also converges to, with no extra
/// reconciliation math needed. `owned` is consumed here (its FIFOs/tap/monitor
/// become the pump's).
///
/// Residual A/V length mismatch: unlike the CPU owned paths (which can duplicate
/// their last raw frame to cover any gap — see `pump::VideoTicker::ticks_to_cover`),
/// a `DmabufFrame` is only valid for the duration of its callback (its planes
/// reference a compositor-owned buffer that may be reused/recycled the instant the
/// callback returns — see [`crate::platform::pipewire::consume_dmabuf`]'s doc), so
/// it cannot be retained and re-submitted at stop to pad the video's end. The video
/// elementary stream may therefore end up to one PipeWire frame-delivery interval
/// short of the audio's measured media length; `-shortest` trims the mux to
/// whichever stream is shorter, so this reads as an early cut of AT MOST one frame
/// period, never a desync. (Contrast `record_screencopy_zero_copy_owned`, which
/// keeps a small pool of its OWN dmabuf targets alive for the whole session and can
/// re-submit the pool's last-used target to close this exact gap.)
#[cfg(feature = "zero-copy")]
#[allow(clippy::too_many_arguments)]
fn record_pipewire_zero_copy_owned(
    fd: OwnedFd,
    node_id: u32,
    fps: u32,
    codec: &str,
    max_res: (u32, u32),
    mic: bool,
    system_audio: bool,
    bitrate_kbps: u32,
    audio_offset_ms: i32,
    auto_device_compensation: bool,
    out_path: &std::path::Path,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    events: &Mutex<Vec<ToggleEvent>>,
    dims: Arc<Mutex<Option<(u32, u32)>>>,
    metadata: &str,
    owned: super::owned::OwnedAudioStart,
) -> Result<PathBuf, String> {
    let Some(node) = crate::encode::gpu::default_vaapi_node() else {
        owned.cleanup();
        return Err("no VAAPI device for zero-copy".to_string());
    };
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let super::owned::OwnedAudioStart { mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx } =
        owned;
    let temp = super::recording_temp_path(out_path);

    // State shared between the (single-threaded, 'static-bound — see
    // `consume_dmabuf`'s signature) PipeWire callback and this function. Much
    // simpler than the legacy path's `Zc`: ONE encoder + ONE muxer for the whole
    // session (no segment restart on pause — see the fn doc), and no
    // mic/system-relay/monitor-probe bookkeeping at all (the pump owns all of
    // that now).
    struct Session {
        node: String,
        fps: u32,
        bitrate: u32,
        codec: String,
        max_res: (u32, u32),
        temp: PathBuf,
        enc: Option<crate::encode::gpu::Encoder>,
        child: Option<std::process::Child>,
        watchdog: Option<super::MuxerWatchdog>,
        tx: Option<std::sync::mpsc::Sender<Vec<u8>>>,
        writer: Option<std::thread::JoinHandle<()>>,
        is_hevc: bool,
        frames: u64,
        error: Option<String>,
    }
    let sess = std::rc::Rc::new(std::cell::RefCell::new(Session {
        node,
        fps: fps.max(1),
        bitrate: bitrate_kbps,
        codec: codec.to_string(),
        max_res,
        temp: temp.clone(),
        enc: None,
        child: None,
        watchdog: None,
        tx: None,
        writer: None,
        is_hevc: false,
        frames: 0,
        error: None,
    }));

    // First-frame watchdog: if no dmabuf frame arrives, stop so `consume_dmabuf`
    // returns and we report failure (caller falls back to the CPU path).
    let got_frame = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let got = got_frame.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(4));
            if !got.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
            }
        });
    }

    let pump_cfg = super::pump::PumpConfig {
        fps: fps.max(1),
        audio_offset_ms,
        auto_device_compensation,
        mic_on0: mic,
        sys_on0: system_audio,
        duck_system: crate::audio::config::recording_duck_system(),
    };
    let session_start = std::time::Instant::now();

    // The pump thread borrows `events`/`stop`/`paused` for its own lifetime (see
    // `pump::spawn`'s doc) — `std::thread::scope` is what makes that sound, exactly
    // like the CPU owned paths.
    /// One channel's mute intervals plus the session's frame/codec facts — named
    /// so the `std::thread::scope` return type below doesn't trip clippy's
    /// `type_complexity` (mirrors `pipewire::MuteIntervals`'s same reason).
    type OwnedZcResult = (Vec<(f64, f64)>, Vec<(f64, f64)>, u64, bool);
    let scope_result: Result<OwnedZcResult, String> = std::thread::scope(|scope| {
            let (pump_handle, _ticker) = match super::pump::spawn(
                scope, session_start, pump_cfg, mic_fifo_path.clone(), sys_fifo_path.clone(),
                mic_tap, mic_rx, monitor, sys_rx, &stop, &paused, events,
            ) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };

            let cb_sess = sess.clone();
            let cb_stop = stop.clone();
            let cb_got = got_frame.clone();
            let cb_paused = paused.clone();
            let cb_paused_watchdog = paused.clone();
            let cb_mic_fifo = mic_fifo_path.clone();
            let cb_sys_fifo = sys_fifo_path.clone();
            let cb_dims = dims.clone();
            let run = crate::platform::pipewire::consume_dmabuf(fd, node_id, stop.clone(), move |frame| {
                cb_got.store(true, Ordering::Relaxed);
                let mut z = cb_sess.borrow_mut();
                if z.error.is_some() {
                    return;
                }
                // Paused (DRAGON-111/127): feed the encoder NOTHING — never close
                // or restart the session (unlike the legacy per-segment model);
                // the pump's clock freezes for the identical span (both read the
                // same `paused` flag), so the two naturally agree on media length
                // with no reconciliation needed (see the fn doc).
                if cb_paused.load(Ordering::Relaxed) {
                    return;
                }
                if z.enc.is_none() {
                    let (mw, mh) = crate::encode::codec_capped_resolution(z.max_res, &z.codec);
                    let (dw, dh) = crate::encode::fit_within(frame.width, frame.height, mw, mh);
                    if let Ok(mut g) = cb_dims.lock() {
                        *g = Some((frame.width, frame.height));
                    }
                    let hevc = match z.codec.as_str() {
                        "h264" => false,
                        "hevc" => true,
                        _ => dw.max(dh) > 4096,
                    };
                    match crate::encode::gpu::Encoder::new(
                        &z.node, hevc, frame.width, frame.height, dw, dh, z.fps, z.bitrate,
                    ) {
                        Ok(e) => z.enc = Some(e),
                        Err(e) => {
                            z.error = Some(format!("encoder init: {e}"));
                            cb_stop.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                    match crate::encode::spawn_ffmpeg_encoded_media_clock(
                        hevc, z.fps, &z.temp, &cb_mic_fifo, &cb_sys_fifo,
                    ) {
                        Ok(mut c) => {
                            let Some(mut stdin) = c.stdin.take() else {
                                z.enc = None;
                                z.error = Some("muxer stdin unavailable".to_string());
                                cb_stop.store(true, Ordering::Relaxed);
                                return;
                            };
                            let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
                            let writer = std::thread::spawn(move || {
                                while let Ok(bytes) = rx.recv() {
                                    if stdin.write_all(&bytes).is_err() {
                                        break;
                                    }
                                }
                            });
                            // Armed BEFORE the muxer can be handed any bytes: a
                            // startup scheduler wedge (DRAGON-118/123) never reads
                            // stdin, and the pause gate matches the CPU owned
                            // paths' reasoning (this session-long muxer is fed
                            // nothing while paused, by design).
                            z.watchdog = Some(super::MuxerWatchdog::arm_gated(
                                c.id(), z.temp.clone(), cb_paused_watchdog.clone(),
                            ));
                            z.tx = Some(tx);
                            z.writer = Some(writer);
                            z.child = Some(c);
                            z.is_hevc = hevc;
                        }
                        Err(e) => {
                            z.enc = None;
                            z.error = Some(format!("muxer spawn: {e}"));
                            cb_stop.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                }
                let outcome: Result<(), String> = {
                    let Session { enc, tx, .. } = &mut *z;
                    match (enc.as_mut(), tx.as_ref()) {
                        (Some(enc), Some(tx)) => {
                            match enc.encode_dmabuf(frame.fourcc, frame.modifier, frame.planes) {
                                Ok(bytes) if bytes.is_empty() => Ok(()),
                                Ok(bytes) => {
                                    tx.send(bytes).map_err(|_| "muxer writer gone".to_string())
                                }
                                Err(e) => Err(format!("gpu encode: {e}")),
                            }
                        }
                        _ => return,
                    }
                };
                match outcome {
                    Ok(()) => z.frames += 1,
                    Err(e) => {
                        z.error = Some(e);
                        cb_stop.store(true, Ordering::Relaxed);
                    }
                }
            });

            // Stop: the pump IS the audio drain — join its control thread first
            // (mirrors every other owned path's stop tail exactly).
            stop.store(true, Ordering::Relaxed);
            let pump_out = pump_handle.join();
            log::info!(
                "media-clock pump stats: mic(late={} paused_drop={} gap={}) sys(late={} \
                 paused_drop={} gap={})",
                pump_out.mic_stats.late_chunks,
                pump_out.mic_stats.discarded_paused_chunks,
                pump_out.mic_stats.gap_samples,
                pump_out.sys_stats.late_chunks,
                pump_out.sys_stats.discarded_paused_chunks,
                pump_out.sys_stats.gap_samples,
            );

            // Log the residual A/V length gap this model can't close (see the fn
            // doc) — informational only; `-shortest` already makes it safe.
            let mut z = sess.borrow_mut();
            let covered = z.frames as f64 / z.fps.max(1) as f64;
            let residual = pump_out.final_media - covered;
            if residual > 0.0 {
                log::info!(
                    "zero-copy owned session: video covers {covered:.3}s vs audio media \
                     {:.3}s (residual {residual:.3}s, expected ≤ ~1/{} frame period — see \
                     record_pipewire_zero_copy_owned's doc)",
                    pump_out.final_media, z.fps.max(1),
                );
            }

            // Drain the encoder's tail through the writer queue, close the
            // channel (writer flushes + drops stdin → muxer EOF), join the
            // writer, reap the muxer.
            if let Some(mut enc) = z.enc.take()
                && let (Ok(tail), Some(tx)) = (enc.finish(), z.tx.as_ref())
            {
                let _ = tx.send(tail);
            }
            z.tx = None;
            if let Some(w) = z.writer.take() {
                let _ = w.join();
            }
            z.watchdog = None;
            let wait_result = z.child.take().map(|mut c| super::wait_or_kill(&mut c, std::time::Duration::from_secs(30)));
            let frames = z.frames;
            let is_hevc = z.is_hevc;
            let error = z.error.take();
            drop(z);

            if let Err(e) = run {
                if frames == 0 {
                    return Err(format!("zero-copy capture: {e}"));
                }
                log::warn!("zero-copy capture ended early ({e}); keeping what's recorded");
            }
            if let Some(e) = error {
                if frames == 0 {
                    return Err(e);
                }
                log::warn!("zero-copy error after recording started ({e}); keeping what's recorded");
            }
            if frames == 0 {
                return Err("zero-copy: no dmabuf frames (compositor declined dmabuf?)".to_string());
            }
            match wait_result {
                Some(Ok(s)) if s.success() => {}
                other => {
                    // A stop-tail-only failure leaves a structurally sound mkv
                    // behind (mirrors every other owned path's salvage
                    // rationale) — SALVAGE it instead of deleting the recording.
                    if super::muxer_alive(&temp) {
                        log::warn!(
                            "zero-copy muxer had to be killed at stop ({other:?}); salvaging \
                             the written temp into a finalized recording"
                        );
                    } else {
                        let _ = std::fs::remove_file(&temp);
                        return Err("zero-copy muxer failed".to_string());
                    }
                }
            }
            Ok((pump_out.mic_off, pump_out.sys_off, frames, is_hevc))
        });

    let (mic_off, sys_off, frames, is_hevc) = scope_result?;
    let _ = frames;
    super::finalize::finalize_with_intervals(&temp, out_path, &mic_off, &sys_off, is_hevc, metadata)
}

// ---------------------------------------------------------------------------
// Screencopy DMA-BUF (zero-copy) capture
//
// Instead of an shm buffer the compositor blits into and we read back over PCIe,
// we allocate a GPU buffer object (gbm) on the device the compositor reports via
// screencopy, hand its dmabuf to the compositor to copy each frame into, then
// import that same buffer straight into the in-process encoder. The pixels never
// leave the GPU. This only works when an encoder lives on the SAME device (the
// compositor's render node) — e.g. VAAPI on an AMD/Intel iGPU. On a discrete
// NVIDIA primary the buffer is NVIDIA-tiled and only NVENC could import it, so
// the caller falls back to the CPU path.
// ---------------------------------------------------------------------------

/// DRM modifier sentinel meaning "implicit / driver's choice".
#[cfg(feature = "zero-copy")]
const DRM_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// DRM fourccs the in-process VAAPI import understands, most-opaque first.
#[cfg(feature = "zero-copy")]
const DMABUF_PREF_FORMATS: [u32; 4] = [
    0x3432_5258, // XR24  XRGB8888
    0x3432_5241, // AR24  ARGB8888
    0x3432_4258, // XB24  XBGR8888
    0x3432_4241, // AB24  ABGR8888
];

/// Resolve a DRM `dev_t` (screencopy's `dmabuf_device`) to the `/dev/dri/renderD*`
/// node on that GPU. We allocate the capture buffer and encode on this node, so
/// the compositor's copy and our import stay on one device. When the compositor
/// names a primary (card) node, find its render sibling through sysfs.
#[cfg(feature = "zero-copy")]
fn render_node_for_dev(dev: u64) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let mut matched: Option<PathBuf> = None;
    for entry in std::fs::read_dir("/dev/dri").ok()?.flatten() {
        if matches!(entry.metadata(), Ok(md) if md.rdev() == dev) {
            matched = Some(entry.path());
            break;
        }
    }
    let path = matched?;
    let name = path.file_name()?.to_str()?;
    if name.starts_with("renderD") {
        return Some(path.to_string_lossy().into_owned());
    }
    let card = name.strip_prefix("card")?;
    let drm_dir = format!("/sys/class/drm/card{card}/device/drm");
    for entry in std::fs::read_dir(&drm_dir).ok()?.flatten() {
        let fname = entry.file_name();
        if fname.to_str().is_some_and(|n| n.starts_with("renderD")) {
            return Some(format!("/dev/dri/{}", fname.to_string_lossy()));
        }
    }
    None
}

/// Pick a (gbm format, concrete modifiers) the compositor offers and the encoder
/// can import. The implicit-modifier sentinel is dropped — gbm allocates with an
/// explicit modifier we then read back and pass to the encoder.
#[cfg(feature = "zero-copy")]
fn pick_dmabuf_format(formats: &[(u32, Vec<u64>)]) -> Option<(gbm::Format, Vec<u64>)> {
    for want in DMABUF_PREF_FORMATS {
        let Some((f, mods)) = formats.iter().find(|(f, _)| *f == want) else {
            continue;
        };
        let Ok(fmt) = gbm::Format::try_from(*f) else { continue };
        let mods: Vec<u64> = mods.iter().copied().filter(|m| *m != DRM_MOD_INVALID).collect();
        return Some((fmt, mods));
    }
    None
}

/// A GPU buffer the compositor copies a screencopy frame into, plus everything
/// the encoder needs to import it. Holds the gbm device + buffer object alive for
/// the recording (the `wl_buffer` and the encoder both reference its memory).
#[cfg(feature = "zero-copy")]
struct DmabufTarget {
    /// The `wl_buffer` handed to `session.capture` for the compositor to fill.
    buffer: wl_buffer::WlBuffer,
    /// Per-plane (fd, offset, stride). Fds are kept alive here for the encoder.
    planes: Vec<(OwnedFd, u32, u32)>,
    fourcc: u32,
    modifier: u64,
    width: u32,
    height: u32,
    /// `/dev/dri/renderD*` the buffer lives on — also where we must encode.
    render_node: String,
    // Dropped after `buffer`/`planes`: the bo's memory must outlive its uses, and
    // the gbm device must outlive the bo (declared last so it drops last).
    _bo: gbm::BufferObject<()>,
    _device: gbm::Device<std::fs::File>,
}

#[cfg(feature = "zero-copy")]
impl DmabufTarget {
    /// Allocate a buffer matching the compositor's reported device + formats and
    /// register it as a `wl_buffer`. Errors (no dmabuf device, cross-vendor, no
    /// usable format) are the caller's cue to fall back to the shm/CPU path.
    fn new(
        formats: &Formats,
        dmabuf_state: &DmabufState,
        qh: &QueueHandle<ScreencopyClient>,
    ) -> Result<Self, String> {
        let dev = formats.dmabuf_device.ok_or("compositor offered no dmabuf device")?;
        let (w, h) = formats.buffer_size;
        if w == 0 || h == 0 {
            return Err("zero buffer size".into());
        }
        let node = render_node_for_dev(dev).ok_or("no render node for the dmabuf device")?;
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .open(&node)
            .map_err(|e| format!("open {node}: {e}"))?;
        let device = gbm::Device::new(file).map_err(|e| format!("gbm device: {e}"))?;
        let (fmt, mods) = pick_dmabuf_format(&formats.dmabuf_formats)
            .ok_or("no encoder-compatible dmabuf format offered")?;
        let bo = if mods.is_empty() {
            device
                .create_buffer_object::<()>(w, h, fmt, gbm::BufferObjectFlags::empty())
                .map_err(|e| format!("gbm allocate (implicit modifier): {e}"))?
        } else {
            device
                .create_buffer_object_with_modifiers2::<()>(
                    w,
                    h,
                    fmt,
                    mods.iter().map(|m| gbm::Modifier::from(*m)),
                    gbm::BufferObjectFlags::empty(),
                )
                .map_err(|e| format!("gbm allocate: {e}"))?
        };
        let modifier: u64 = bo.modifier().map_err(|e| format!("bo modifier: {e}"))?.into();
        let plane_count = bo.plane_count().map_err(|e| format!("plane_count: {e}"))? as i32;
        let params = dmabuf_state.create_params(qh).map_err(|e| format!("dmabuf params: {e}"))?;
        let mut planes = Vec::with_capacity(plane_count as usize);
        for i in 0..plane_count {
            let fd = bo.fd_for_plane(i).map_err(|e| format!("fd_for_plane({i}): {e}"))?;
            let offset = bo.offset(i).map_err(|e| format!("offset({i}): {e}"))?;
            let stride = bo.stride_for_plane(i).map_err(|e| format!("stride({i}): {e}"))?;
            params.add(fd.as_fd(), i as u32, offset, stride, modifier);
            planes.push((fd, offset, stride));
        }
        let (buffer, _) = params.create_immed(
            w as i32,
            h as i32,
            fmt as u32,
            zwp_linux_buffer_params_v1::Flags::empty(),
            qh,
        );
        Ok(Self {
            buffer,
            planes,
            fourcc: fmt as u32,
            modifier,
            width: w,
            height: h,
            render_node: node,
            _bo: bo,
            _device: device,
        })
    }

    /// Plane tuples for `Encoder::encode_dmabuf` (raw fds borrowed from the bo).
    fn encode_planes(&self) -> Vec<(i32, u32, u32)> {
        self.planes.iter().map(|(fd, off, st)| (fd.as_raw_fd(), *off, *st)).collect()
    }
}

/// Diagnostic: validate the screencopy DMA-BUF capture pipeline on this machine —
/// allocate a GPU buffer on the compositor's reported device and have it copy one
/// frame in, with no encoder involved. Reports what we learned so we can tell
/// "capture works, encoder is the gap" from "the compositor won't do dmabuf".
#[cfg(feature = "zero-copy")]
pub fn screencopy_dmabuf_test() -> String {
    let Some((conn, mut queue, mut data)) = connect(false) else {
        return "screencopy-dmabuf-test: wayland connect failed".into();
    };
    let qh = queue.handle();
    let Some((output, name, _, _)) = outputs(&data).into_iter().next() else {
        return "screencopy-dmabuf-test: no outputs".into();
    };
    let src = CaptureSource::Output(output);
    data.formats = None;
    data.result = None;
    let Ok(session) = data.screencopy_state.capturer().create_session(
        &src,
        CaptureOptions::empty(),
        &qh,
        ScreencopySessionData::default(),
    ) else {
        return "screencopy-dmabuf-test: screencopy session failed".into();
    };
    let _ = conn.flush();
    let mut guard = 0;
    while data.formats.is_none() {
        if queue.blocking_dispatch(&mut data).is_err() {
            return "screencopy-dmabuf-test: dispatch failed".into();
        }
        guard += 1;
        if guard > 200 {
            return "screencopy-dmabuf-test: capture formats never arrived".into();
        }
    }
    // Guaranteed Some: the while loop above only exits once data.formats is Some
    // (a dispatch failure or exceeded guard returns a diagnostic string first).
    let formats = data.formats.clone().expect("format wait loop above exits only when Some");
    let dev = formats.dmabuf_device;
    let nfmt = formats.dmabuf_formats.len();
    let target = match DmabufTarget::new(&formats, &data.dmabuf_state, &qh) {
        Ok(t) => t,
        Err(e) => {
            return format!(
                "screencopy-dmabuf-test: output {name}: could not allocate a dmabuf target: {e}\n  \
                 dmabuf_device={dev:?}, {nfmt} dmabuf format(s) offered. If the device is your \
                 discrete GPU this is expected for the VAAPI encoder (cross-vendor); the CPU path \
                 is used."
            );
        }
    };
    data.result = None;
    session.capture(&target.buffer, &[], &qh, ScreencopyFrameData::default());
    let _ = conn.flush();
    let mut guard = 0;
    while data.result.is_none() {
        if queue.blocking_dispatch(&mut data).is_err() {
            break;
        }
        guard += 1;
        if guard > 400 {
            break;
        }
    }
    match data.result.clone() {
        Some(Ok(_)) => format!(
            "screencopy-dmabuf-test: SUCCESS on output {name}\n  \
             device={} size={}x{} format=0x{:08x} modifier=0x{:016x} planes={}\n  \
             The compositor copied a frame straight into a GPU buffer, so zero-copy capture works \
             here. Encoding it still needs a same-device encoder on {}.",
            target.render_node,
            target.width,
            target.height,
            target.fourcc,
            target.modifier,
            target.planes.len(),
            target.render_node,
        ),
        Some(Err(())) => format!(
            "screencopy-dmabuf-test: capture FAILED on output {name} after allocation \
             (device={} format=0x{:08x} modifier=0x{:016x}). The compositor declined to copy into \
             our dmabuf.",
            target.render_node, target.fourcc, target.modifier,
        ),
        None => format!("screencopy-dmabuf-test: timed out waiting for the frame on output {name}"),
    }
}

/// Result of attempting screencopy zero-copy recording.
#[cfg(feature = "zero-copy")]
pub(crate) enum ZcOutcome {
    /// Zero-copy couldn't start (no dmabuf / cross-vendor encoder / spawn failed);
    /// the caller should use the CPU path. No frames were recorded.
    Fallback(String),
    /// Zero-copy ran to completion — this is the final recording result.
    Done(Result<PathBuf, String>),
}

/// How many extra `1/fps` frames [`record_screencopy_zero_copy_owned`] must
/// re-submit so `frames_encoded` (at `fps`) covers AT LEAST `final_media` seconds —
/// the dmabuf-frame analogue of `pump::VideoTicker::ticks_to_cover`, factored out
/// as a pure function so the rounding (never fewer than needed, no over-counting
/// what's already encoded) is unit-tested without a live GPU encoder. Rounds UP;
/// never negative (already-sufficient coverage needs zero extra frames).
#[cfg(feature = "zero-copy")]
fn trailing_frames_needed(frames_encoded: u64, fps: u32, final_media: f64) -> u64 {
    let covered = frames_encoded as f64 / fps.max(1) as f64;
    let needed = (final_media - covered).max(0.0);
    (needed * fps.max(1) as f64).ceil() as u64
}

/// Record a FULL output (monitor mode) with GPU zero-copy: the compositor copies
/// each frame straight into a gbm buffer we allocated on its own render node, which
/// we import into the in-process encoder (no CPU readback). Encoding happens on the
/// buffer's device, so a VAAPI iGPU output works; an NVIDIA-rendered output fails at
/// encoder init (cross-vendor) and falls back to the CPU path.
///
/// We cycle a small pool of buffers so the compositor never overwrites one the GPU
/// is still encoding (single-buffer reuse would tear).
///
/// Runs its own audio pre-flight check ([`super::owned::try_start_owned_audio`])
/// before committing to the owned zero-copy session; a failure there reports as
/// [`ZcOutcome::Fallback`] so the caller (`record::screencopy`'s owned loop, which by
/// construction already holds its OWN successfully-started `OwnedAudioStart`) simply
/// continues with its CPU readback path instead — this function never gets to touch
/// that outer one. `stop`/`paused` take owned `Arc<AtomicBool>` (not a bare
/// `&AtomicBool`, unlike this file's other zero-copy caller convention) because the
/// owned variant needs a genuine `Arc` to hand to [`super::MuxerWatchdog::arm_gated`]
/// (its own background thread isn't scoped) — `record::screencopy`'s caller already
/// holds a real `Arc`, so this costs it nothing.
#[cfg(feature = "zero-copy")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_screencopy_zero_copy(
    conn: &Connection,
    queue: &mut EventQueue<ScreencopyClient>,
    data: &mut ScreencopyClient,
    qh: &QueueHandle<ScreencopyClient>,
    session: &CaptureSession,
    formats: &Formats,
    fps: u32,
    codec: &str,
    max_res: (u32, u32),
    mic: bool,
    system_audio: bool,
    bitrate_kbps: u32,
    audio_offset_ms: i32,
    auto_device_compensation: bool,
    out_path: &std::path::Path,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    events: &Mutex<Vec<ToggleEvent>>,
    dims: &Mutex<Option<(u32, u32)>>,
    metadata: &str,
) -> ZcOutcome {
    match super::owned::try_start_owned_audio() {
        Ok(owned) => {
            log::info!("screencopy zero-copy pipeline: media-clock owned path (DRAGON-127)");
            record_screencopy_zero_copy_owned(
                conn, queue, data, qh, session, formats, fps, codec, max_res, mic, system_audio,
                bitrate_kbps, audio_offset_ms, auto_device_compensation, out_path, stop, paused,
                events, dims, metadata, owned,
            )
        }
        Err(reason) => ZcOutcome::Fallback(format!(
            "zero-copy audio pre-flight failed ({reason})"
        )),
    }
}

/// The media-clock owned GPU zero-copy screencopy session (DRAGON-127): ONE
/// continuous encoder + muxer for the whole recording instead of the legacy
/// per-pause-segment model, audio rendered by [`super::pump`] through plain FIFOs —
/// the screencopy sibling of `record_pipewire_zero_copy_owned` (see its doc for the
/// shared reasoning: a pause simply stops feeding the encoder, never restarts it, so
/// video's total encoded length naturally converges to the pump's media time with no
/// extra reconciliation). Unlike the PipeWire dmabuf callback, this function keeps
/// its OWN small pool of `targets` alive for the whole session, so — unlike that
/// sibling's documented residual mismatch — it CAN close the video/audio length gap
/// exactly: at stop, it re-submits the last-used pool target through the encoder as
/// many extra times as needed to cover the audio's measured media length (the
/// dmabuf-frame analogue of `pump::VideoTicker::ticks_to_cover`).
#[cfg(feature = "zero-copy")]
#[allow(clippy::too_many_arguments)]
fn record_screencopy_zero_copy_owned(
    conn: &Connection,
    queue: &mut EventQueue<ScreencopyClient>,
    data: &mut ScreencopyClient,
    qh: &QueueHandle<ScreencopyClient>,
    session: &CaptureSession,
    formats: &Formats,
    fps: u32,
    codec: &str,
    max_res: (u32, u32),
    mic: bool,
    system_audio: bool,
    bitrate_kbps: u32,
    audio_offset_ms: i32,
    auto_device_compensation: bool,
    out_path: &std::path::Path,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    events: &Mutex<Vec<ToggleEvent>>,
    dims: &Mutex<Option<(u32, u32)>>,
    metadata: &str,
    owned: super::owned::OwnedAudioStart,
) -> ZcOutcome {
    const POOL: usize = 3;
    // --- init: any failure here means "fall back to the CPU path" (identical to
    // the legacy path's own init section) ---
    let mut targets: Vec<DmabufTarget> = Vec::with_capacity(POOL);
    for _ in 0..POOL {
        match DmabufTarget::new(formats, &data.dmabuf_state, qh) {
            Ok(t) => targets.push(t),
            Err(e) => {
                owned.cleanup();
                return ZcOutcome::Fallback(e);
            }
        }
    }
    let (cw, ch) = formats.buffer_size;
    let fps = fps.max(1);
    let (mw, mh) = crate::encode::codec_capped_resolution(max_res, codec);
    let (ew, eh) = crate::encode::fit_within(cw, ch, mw, mh);
    if let Ok(mut g) = dims.lock() {
        *g = Some((cw, ch));
    }
    let hevc = match codec {
        "h264" => false,
        "hevc" => true,
        _ => ew.max(eh) > 4096,
    };
    let mut enc = match crate::encode::gpu::Encoder::new(
        &targets[0].render_node,
        hevc,
        cw,
        ch,
        ew,
        eh,
        fps,
        bitrate_kbps,
    ) {
        Ok(e) => e,
        Err(e) => {
            owned.cleanup();
            return ZcOutcome::Fallback(format!("encoder init: {e}"));
        }
    };
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // --- committed: run the capture+encode session to completion ---
    let fourcc = targets[0].fourcc;
    let modifier = targets[0].modifier;
    let damage = [Rect { x: 0, y: 0, width: cw as i32, height: ch as i32 }];
    let frame_dur = std::time::Duration::from_secs_f64(1.0 / fps as f64);

    let super::owned::OwnedAudioStart { mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx } =
        owned;
    let temp = super::recording_temp_path(out_path);
    let mut child = match crate::encode::spawn_ffmpeg_encoded_media_clock(
        hevc, fps, &temp, &mic_fifo_path, &sys_fifo_path,
    ) {
        Ok(c) => c,
        Err(e) => {
            drop(mic_tap);
            let _ = monitor.stop();
            let _ = std::fs::remove_file(&mic_fifo_path);
            let _ = std::fs::remove_file(&sys_fifo_path);
            return ZcOutcome::Done(Err(format!("muxer spawn: {e}")));
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        drop(mic_tap);
        let _ = monitor.stop();
        let _ = std::fs::remove_file(&mic_fifo_path);
        let _ = std::fs::remove_file(&sys_fifo_path);
        return ZcOutcome::Done(Err("muxer stdin unavailable".to_string()));
    };

    let pump_cfg = super::pump::PumpConfig {
        fps,
        audio_offset_ms,
        auto_device_compensation,
        mic_on0: mic,
        sys_on0: system_audio,
        duck_system: crate::audio::config::recording_duck_system(),
    };
    let session_start = std::time::Instant::now();
    let mut last_idx: usize = 0;

    type OwnedZcResult = (Vec<(f64, f64)>, Vec<(f64, f64)>);
    let scope_result: Result<OwnedZcResult, String> = std::thread::scope(|scope| {
        let (pump_handle, _ticker) = match super::pump::spawn(
            scope, session_start, pump_cfg, mic_fifo_path.clone(), sys_fifo_path.clone(), mic_tap,
            mic_rx, monitor, sys_rx, &stop, &paused, events,
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = child.kill();
                return Err(e);
            }
        };

        // MuxerWatchdog: armed BEFORE the muxer can receive its first bytes, pause-
        // gated exactly like the CPU owned paths (this session-long muxer is fed
        // nothing while paused, by design — DRAGON-118/123/125/127).
        let watchdog = super::MuxerWatchdog::arm_gated(child.id(), temp.clone(), paused.clone());
        let mut next = std::time::Instant::now() + frame_dur;
        let mut muxer_wedged = false;
        let mut frames: u64 = 0;

        'grab: loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            // Paused: feed the encoder NOTHING (mirrors the PipeWire owned
            // sibling) — idle with the capture session open, a periodic
            // roundtrip keeping the wayland socket drained.
            if paused.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(15));
                if queue.roundtrip(data).is_err() {
                    break 'grab;
                }
                next = std::time::Instant::now() + frame_dur;
                continue;
            }
            let now = std::time::Instant::now();
            if now < next {
                std::thread::sleep(next - now);
            }
            next += frame_dur;
            if next < now {
                next = now + frame_dur;
            }
            if stop.load(Ordering::Relaxed) || paused.load(Ordering::Relaxed) {
                continue;
            }
            let idx = last_idx % POOL;
            last_idx = last_idx.wrapping_add(1);
            let target = &targets[idx];
            data.result = None;
            session.capture(&target.buffer, &damage, qh, ScreencopyFrameData::default());
            if conn.flush().is_err() {
                break 'grab;
            }
            let mut guard = 0;
            while data.result.is_none() {
                if queue.blocking_dispatch(data).is_err() {
                    break;
                }
                guard += 1;
                if guard > 400 {
                    break;
                }
            }
            match data.result.clone() {
                Some(Ok(_)) => match enc.encode_dmabuf(fourcc, modifier, &target.encode_planes()) {
                    Ok(bytes) => {
                        if !bytes.is_empty() && stdin.write_all(&bytes).is_err() {
                            break 'grab;
                        }
                        frames += 1;
                    }
                    Err(e) => {
                        log::error!("zero-copy owned session: gpu encode failed ({e}); stopping");
                        stop.store(true, Ordering::Relaxed);
                        break 'grab;
                    }
                },
                Some(Err(())) | None => {
                    // A single missed/failed capture isn't fatal to a whole
                    // session the way it was to a segment — keep going; the
                    // muxer-liveness watchdog catches a truly wedged compositor.
                    continue;
                }
            }
        }
        if watchdog.fired() {
            muxer_wedged = true;
        }
        watchdog.disarm();

        // Stop: the pump IS the audio drain — join its control thread first
        // (mirrors every other owned path's stop tail exactly).
        stop.store(true, Ordering::Relaxed);
        let pump_out = pump_handle.join();
        log::info!(
            "media-clock pump stats: mic(late={} paused_drop={} gap={}) sys(late={} \
             paused_drop={} gap={})",
            pump_out.mic_stats.late_chunks,
            pump_out.mic_stats.discarded_paused_chunks,
            pump_out.mic_stats.gap_samples,
            pump_out.sys_stats.late_chunks,
            pump_out.sys_stats.discarded_paused_chunks,
            pump_out.sys_stats.gap_samples,
        );

        // Close the video/audio length gap exactly (unlike the PipeWire owned
        // sibling): re-submit the last-used pool target's dmabuf through the
        // encoder as many extra times as needed to cover the audio's measured
        // media length — the dmabuf-frame analogue of `VideoTicker::ticks_to_cover`.
        //
        // The trailing-coverage + `enc.finish()` writes go through a scoped
        // NON-BLOCKING stdin (DRAGON-161, the shared `owned::NonblockingStdin`): an
        // ffmpeg whose audio was the shorter `-shortest` stream stops draining the
        // video pipe without closing it, so a blocking write here could park forever
        // (the DRAGON-160 wedge — reachable here too, GPU-encoded packets and all).
        // A covering-write failure is BENIGN (the video is already long enough), so
        // it just stops the loop and NO LONGER marks the muxer wedged (matching the
        // RGBA owned workers); the bounded reap + shared salvage decision below then
        // finalize the sound temp instead of deleting the user's recording.
        {
            let _nb = super::owned::NonblockingStdin::new(&stdin);
            if !muxer_wedged && frames > 0 {
                let extra = trailing_frames_needed(frames, fps, pump_out.final_media);
                let last_target = &targets[last_idx.wrapping_sub(1) % POOL];
                for _ in 0..extra {
                    match enc.encode_dmabuf(fourcc, modifier, &last_target.encode_planes()) {
                        Ok(bytes) => {
                            if !bytes.is_empty() && stdin.write_all(&bytes).is_err() {
                                break;
                            }
                            frames += 1;
                        }
                        Err(e) => {
                            log::warn!(
                                "zero-copy owned session: trailing coverage encode failed ({e})"
                            );
                            break;
                        }
                    }
                }
            }

            if !muxer_wedged
                && let Ok(tail) = enc.finish()
            {
                let _ = stdin.write_all(&tail);
            }
        }
        drop(stdin); // EOF -> ffmpeg flushes and exits
        if muxer_wedged {
            let _ = child.kill();
        }
        let wait_result = super::wait_or_kill(&mut child, std::time::Duration::from_secs(30));

        if frames == 0 {
            let _ = std::fs::remove_file(&temp);
            return Err("zero-copy produced no frames".to_string());
        }
        // Shared salvage decision (DRAGON-161): a stop-tail-only death that left a
        // sound temp is salvaged; a wedged muxer stays fatal. `salvage_decision`
        // removes nothing, so mirror the RGBA workers' log + explicit temp removal.
        let temp_alive = super::muxer_alive(&temp);
        match super::owned::salvage_decision(&wait_result, muxer_wedged, temp_alive) {
            Ok(()) => {
                if !matches!(&wait_result, Ok(s) if s.success() && !muxer_wedged) {
                    log::warn!(
                        "zero-copy muxer had to be killed at stop ({wait_result:?}); salvaging the \
                         written temp into a finalized recording"
                    );
                }
            }
            Err(e) => {
                let _ = std::fs::remove_file(&temp);
                return Err(e);
            }
        }
        Ok((pump_out.mic_off, pump_out.sys_off))
    });

    let (mic_off, sys_off) = match scope_result {
        Ok(v) => v,
        Err(e) => return ZcOutcome::Done(Err(e)),
    };
    ZcOutcome::Done(super::finalize::finalize_with_intervals(
        &temp, out_path, &mic_off, &sys_off, hevc, metadata,
    ))
}

#[cfg(feature = "zero-copy")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_frames_needed_covers_a_gap_and_rounds_up() {
        // 100 frames @ 30fps = 3.333...s covered; media ran to 3.5s -> need
        // ceil((3.5 - 3.3333) * 30) = ceil(5.0...) = 5 more frames.
        assert_eq!(trailing_frames_needed(100, 30, 3.5), 5);
    }

    #[test]
    fn trailing_frames_needed_is_zero_when_already_covered_or_over() {
        // Exactly covered.
        assert_eq!(trailing_frames_needed(90, 30, 3.0), 0);
        // Video already covers MORE than the audio's media length (never negative).
        assert_eq!(trailing_frames_needed(120, 30, 3.0), 0);
    }

    #[test]
    fn trailing_frames_needed_handles_zero_frames_encoded() {
        // Nothing encoded yet, but the session still measured some media length —
        // covers the whole thing from frame 0.
        assert_eq!(trailing_frames_needed(0, 30, 1.0), 30);
    }
}
