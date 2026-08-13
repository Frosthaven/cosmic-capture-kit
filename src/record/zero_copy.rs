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
/// THREE things happen before any audio exists, in this order:
///
/// 1. The NODE pre-flight ([`preflight_vaapi_node`], DRAGON-425): can this box's VAAPI
///    hardware import what this session renders? A no means zero-copy is impossible here,
///    and declining now costs nothing at all, nothing has been started.
/// 2. The portal stream is connected and a genuinely real first dmabuf FRAME is confirmed
///    (DRAGON-658), which is also where the frame's own modifier gets the second opinion
///    on which GPU produced it. A stream that never delivers, or delivers something this
///    box cannot import, declines here, still free.
/// 3. Only then is the warm instant reported ([`super::owned::mark_first_frame`]) and the
///    AUDIO pre-flight ([`super::owned::try_start_owned_audio`]) run, inline and exactly
///    once. A failure there fails the recording outright with a named, actionable reason.
///
/// The other end of the bring-up, where the encoder and the muxer are built and the session
/// goes READY, is reported separately ([`super::owned::mark_settled`], DRAGON-661); that
/// later signal is the one the UI calls "live".
///
/// The order matters twice over: the audio pre-flight starts real capture streams, so
/// asking the cheap, answerable questions first is what keeps a box that can never do
/// zero-copy from paying for a session it cannot run; and it anchors media 0, so starting
/// it before a pixel has been seen means the app says "recording" with no proof that
/// anything is (DRAGON-657's reason for the whole reordering).
///
/// Returns a [`ZcOutcome`], like its screencopy sibling
/// ([`record_screencopy_zero_copy`]) — NOT a bare `Result` (DRAGON-422). The caller has
/// to be able to tell "zero-copy declined and nothing was recorded, use the CPU path"
/// apart from "this session ran and failed"; while both looked like `Err`, EVERY failure
/// here — including a failed audio pre-flight, which is documented above as fatal — sent
/// the caller off to start a whole second recording session.
///
/// Both siblings classify a failed audio pre-flight the same way since DRAGON-658, as
/// `Done(Err(..))`: neither caller holds an `OwnedAudioStart` any more, and the CPU path
/// would only run the same pre-flight against the same sound server and fail the same way,
/// one whole recording later.
#[cfg(feature = "zero-copy")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_pipewire_zero_copy(
    fd: OwnedFd,
    node_id: u32,
    fps: u32,
    codec: &str,
    max_res: (u32, u32),
    // The countdown gate (DRAGON-673): the app raises it when it promotes the recording, and
    // media 0 waits for it, so a countdown warms the whole pipeline behind its own digits and
    // the file still starts where the promotion does.
    start_gate: Option<Arc<AtomicBool>>,
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
    warm_at: &Mutex<Option<std::time::Instant>>,
    settled_at: &Mutex<Option<std::time::Instant>>,
    metadata: &str,
) -> ZcOutcome {
    // DRAGON-425 PRE-FLIGHT, before the stream, the audio pre-flight, the pump and media 0.
    //
    // The node this path encodes on used to be a GUESS (`default_vaapi_node`: the first
    // amdgpu/i915/xe render node, alphabetically) with no idea what the session renders on.
    // On a multi-GPU box that can be the wrong GPU, and the reported failure is the worst
    // shape of it: a compositor that DECLINES cross-vendor dmabuf never fixates a modifier,
    // so no frame is ever delivered — the session stalls through its whole first-frame
    // budget and only then falls back, having already paid for everything below.
    //
    // The compositor answers this with no frame and no capture: the `zwp_linux_dmabuf_v1`
    // default-feedback `main_device` is the protocol's own statement of which GPU clients
    // should render on. Ask it here, decide, and on a decline give the CPU path the whole
    // recording immediately — nothing has been started, so this genuinely costs nothing.
    match preflight_vaapi_node() {
        // The audio pre-flight now runs INSIDE the owned session, after its first real
        // frame is confirmed (DRAGON-658); this level only answers "can this box do it".
        PreflightNode::Use(node) => record_pipewire_zero_copy_owned(
            fd, node_id, fps, codec, max_res, start_gate, mic, system_audio, bitrate_kbps,
            audio_offset_ms, auto_device_compensation, out_path, stop, paused, events, dims,
            warm_at, settled_at, metadata, node,
        ),
        PreflightNode::Decline(reason) => ZcOutcome::Fallback(reason),
    }
}

/// What the capture thread's FIRST real dmabuf frame tells the session thread
/// (DRAGON-658). Sent once, through a bounded channel, because the frame's own planes die
/// with its callback: only these cheap facts can outlive it.
#[cfg(feature = "zero-copy")]
struct FirstDmabuf {
    width: u32,
    height: u32,
    /// The render node the frame's own DRM modifier confirmed, which may differ from the
    /// one [`preflight_vaapi_node`] chose (see the DRAGON-425 belt-two comment).
    node: String,
    /// The wall instant the frame was delivered, for `owned::mark_first_frame`.
    at: std::time::Instant,
}

/// The confirmation message, or a classified decline: `true` means the CPU path can still
/// cover the whole recording ([`ZcOutcome::Fallback`]), `false` that this is the
/// recording's own failure.
#[cfg(feature = "zero-copy")]
type ConfirmMsg = Result<FirstDmabuf, (bool, String)>;

/// What the DRAGON-425 pre-flight decided, before any recording machinery is started.
#[cfg(feature = "zero-copy")]
enum PreflightNode {
    /// Encode on this render node. Either the default was already right, or the session's
    /// GPU moved us to a different one.
    Use(String),
    /// Zero-copy cannot work on this box at all; the carried string is the USER-facing
    /// reason, since a fallback reason can reach a failure dialog verbatim.
    Decline(String),
}

/// Choose the VAAPI render node for this session BEFORE anything is started (DRAGON-425).
///
/// Asks the compositor which GPU the session renders on (`main_device` of the default
/// `zwp_linux_dmabuf_v1` feedback, via `crate::compositor_main_device`), resolves that to a
/// kernel driver, and runs the shared decision against this box's render nodes. Learning
/// nothing — no Wayland, a compositor too old for feedback, an unknown driver — keeps the
/// historical default, so no single-GPU box changes behaviour.
///
/// A `SwitchTo` also names the libva driver for the new node: libva often cannot auto-pick
/// one when an nvidia node is present, which is exactly the box a switch happens on, so an
/// encoder opened without it can fail on the very machine the switch was made for.
#[cfg(feature = "zero-copy")]
fn preflight_vaapi_node() -> PreflightNode {
    let Some(default_node) = crate::encode::gpu::default_vaapi_node() else {
        return PreflightNode::Decline("no VAAPI device for zero-copy".to_string());
    };
    let nodes = crate::encode::gpu::vaapi_nodes_with_drivers();
    let session_driver = crate::compositor_main_device().and_then(crate::drm_driver_for_dev);
    let decision =
        crate::encode::resolve_vaapi_node_for_session(&nodes, session_driver.as_deref());
    let vendor = session_driver.as_deref().and_then(crate::encode::vendor_from_driver);
    log::info!(
        "{}",
        crate::encode::decision_log_line(std::path::Path::new(&default_node), vendor, &decision)
    );
    match decision {
        crate::encode::NodeDecision::KeepDefault => PreflightNode::Use(default_node),
        crate::encode::NodeDecision::SwitchTo(path) => {
            let node = path.to_string_lossy().into_owned();
            set_libva_driver_for(&nodes, &path);
            PreflightNode::Use(node)
        }
        // The USER half, not the technical one: this string can reach a failure dialog.
        crate::encode::NodeDecision::Decline { user, .. } => PreflightNode::Decline(user),
    }
}

/// Name the libva driver for `node` in this process's environment, so the in-process VAAPI
/// encoder loads the right one (DRAGON-425).
///
/// `plan.rs` does the equivalent for its ffmpeg CHILD through `EncodePlan::env`, which needs
/// no process-wide mutation. The zero-copy path has no child — libva is loaded into THIS
/// process — so the process variable is the only equivalent, and `gpu.rs`'s own tests already
/// set it for the same reason ("driver name matters when an nvidia node is also present").
///
/// A value the user set themselves is never overwritten.
#[cfg(feature = "zero-copy")]
fn set_libva_driver_for(nodes: &[(std::path::PathBuf, String)], node: &std::path::Path) {
    if std::env::var_os("LIBVA_DRIVER_NAME").is_some() {
        return; // the user chose; never write over it
    }
    let Some(libva) = nodes
        .iter()
        .find(|(p, _)| p == node)
        .and_then(|(_, drv)| crate::encode::libva_driver_for(drv))
    else {
        return;
    };
    log::info!("zero-copy: LIBVA_DRIVER_NAME={libva} for {}", node.display());
    // SAFETY: `set_var` is unsound only if another thread may read or write the environment
    // concurrently. This runs on the recording worker thread, and the process IS
    // multi-threaded here — so this is the one genuinely uncomfortable line in the change.
    // It is done as early as a recording can do it (before the audio pre-flight, before the
    // pump, before any encoder thread exists), it writes a variable nothing else in this
    // program reads or writes, and it is skipped entirely when the user has set one. The
    // alternative — opening the encoder without naming the driver — is the documented
    // failure mode on exactly the multi-GPU boxes this code path exists for.
    unsafe { std::env::set_var("LIBVA_DRIVER_NAME", libva) };
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
/// reconciliation math needed.
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
///
/// ## Two threads, and why (DRAGON-658)
///
/// `consume_dmabuf` runs the PipeWire mainloop on the thread that calls it and only
/// returns at stop, so this used to BE the capture loop: the whole session ran inside that
/// one blocking call, and everything else (the audio pre-flight, the pump, the encoder,
/// the muxer) had to be up before it started. The reordering needs the opposite: the
/// stream must be RUNNING and have delivered a frame before the audio pre-flight may
/// start. So the capture now gets a dedicated thread (the same shape
/// `record::pipewire`'s CPU path already uses for `consume_frames`), this thread waits a
/// BOUNDED [`FIRST_FRAME_BUDGET`] for its confirmation, and the session state moved from
/// `Rc<RefCell<..>>` to `Arc<Mutex<..>>` because it is now genuinely shared.
///
/// The callback's first invocation sends the cheap facts a `DmabufFrame` can outlive
/// ([`FirstDmabuf`]) and returns WITHOUT encoding: there is no encoder yet, and its planes
/// die with the call. Every frame after that, until this thread has built the encoder and
/// the muxer, is dropped and counted (`dropped_warmup`). That widens a gap this path
/// already had and already documents: the video stream carries nothing for the warmup
/// span, so the content after it leads the audio by that much. Before the reordering the
/// gap was the stream connect + negotiate + encoder build (which sat after media 0);
/// now it is the audio pre-flight + encoder build. Same constant lead, different cause,
/// and `-shortest` still keeps both streams the same length.
///
/// ## The muxer is spawned LAZILY, and what that costs (DRAGON-422)
///
/// Unlike the screencopy sibling — which knows its frame size from `formats.buffer_size`
/// and so spawns encoder + muxer BEFORE the pump — a PipeWire dmabuf frame's dimensions
/// are only known when the first frame arrives, so both are created after the pump.
/// The pump is therefore already waiting at its FIFO write-end rendezvous for an ffmpeg
/// that does not exist yet, and if the encoder or muxer cannot come up it never will. That
/// is fine, both sides are bounded, but it means the pump's timeout here is NOT evidence
/// of a wedged ffmpeg, and this attempt failing is NOT evidence that the session should be
/// restarted. See [`super::pump::spawn`]'s rendezvous arm for the first, and [`ZcOutcome`]
/// for the second.
#[cfg(feature = "zero-copy")]
#[allow(clippy::too_many_arguments)]
fn record_pipewire_zero_copy_owned(
    fd: OwnedFd,
    node_id: u32,
    fps: u32,
    codec: &str,
    max_res: (u32, u32),
    // The countdown gate (DRAGON-673): the app raises it when it promotes the recording, and
    // media 0 waits for it, so a countdown warms the whole pipeline behind its own digits and
    // the file still starts where the promotion does.
    start_gate: Option<Arc<AtomicBool>>,
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
    warm_at: &Mutex<Option<std::time::Instant>>,
    settled_at: &Mutex<Option<std::time::Instant>>,
    metadata: &str,
    // DRAGON-425: chosen by `preflight_vaapi_node` before the stream was touched, so a box
    // where zero-copy cannot work never reaches this function at all.
    node: String,
) -> ZcOutcome {
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let temp = super::recording_temp_path(out_path);

    // State shared between the PipeWire capture thread's callback and this one. Much
    // simpler than the legacy path's `Zc`: ONE encoder + ONE muxer for the whole
    // session (no segment restart on pause — see the fn doc), and no
    // mic/system-relay/monitor-probe bookkeeping at all (the pump owns all of
    // that now). `enc` being `Some` is also the READY flag: until this thread has built
    // the encoder and the muxer, the callback drops what arrives.
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
        /// Frames the stream delivered while the session was still warming up (the audio
        /// pre-flight and the encoder build), which there was nothing to encode with.
        /// Diagnostic only; see the fn doc for what their absence costs.
        dropped_warmup: u64,
        /// The session's fatal error, and whether it is a zero-copy DECLINE the CPU
        /// path can still cover (`true` → [`ZcOutcome::Fallback`]) or this recording's
        /// own failure (`false` → [`ZcOutcome::Done`]). Only consulted when nothing was
        /// encoded; once frames exist the session owns the outcome either way.
        error: Option<(bool, String)>,
    }

    /// Build the session's encoder and muxer from the confirmed first frame, and publish
    /// them into `z`, which is also what tells the capture thread's callback it may start
    /// encoding. Runs on the session thread, once, with the lock held, so the callback can
    /// never observe a half-built session. `Err` carries the same (can-fall-back, reason)
    /// classification the callback's own failures use.
    fn start_encoding(
        z: &mut Session,
        first: &FirstDmabuf,
        mic_fifo: &std::path::Path,
        sys_fifo: &std::path::Path,
        paused: &Arc<AtomicBool>,
    ) -> Result<(), (bool, String)> {
        z.node = first.node.clone();
        let (mw, mh) = crate::encode::codec_capped_resolution(z.max_res, &z.codec);
        let (dw, dh) = crate::encode::fit_within(first.width, first.height, mw, mh);
        let hevc = match z.codec.as_str() {
            "h264" => false,
            "hevc" => true,
            _ => dw.max(dh) > 4096,
        };
        // Cross-vendor import is exactly what the CPU readback path exists for, and
        // nothing has been encoded: a decline.
        let enc = crate::encode::gpu::Encoder::new(
            &z.node, hevc, first.width, first.height, dw, dh, z.fps, z.bitrate,
        )
        .map_err(|e| (true, format!("encoder init: {e}")))?;
        // ffmpeg itself, not the GPU path: the CPU path would meet the same failure
        // (mirrors the screencopy sibling).
        let mut child =
            crate::encode::spawn_ffmpeg_encoded_media_clock(hevc, z.fps, &z.temp, mic_fifo, sys_fifo)
                .map_err(|e| (false, format!("muxer spawn: {e}")))?;
        let Some(mut stdin) = child.stdin.take() else {
            return Err((false, "muxer stdin unavailable".to_string()));
        };
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let writer = std::thread::spawn(move || {
            while let Ok(bytes) = rx.recv() {
                if stdin.write_all(&bytes).is_err() {
                    break;
                }
            }
        });
        // Armed BEFORE the muxer can be handed any bytes: a startup scheduler wedge
        // (DRAGON-118/123) never reads stdin, and the pause gate matches the CPU owned
        // paths' reasoning (this session-long muxer is fed nothing while paused).
        z.watchdog =
            Some(super::MuxerWatchdog::arm_gated(child.id(), z.temp.clone(), paused.clone()));
        z.tx = Some(tx);
        z.writer = Some(writer);
        z.child = Some(child);
        z.is_hevc = hevc;
        z.enc = Some(enc); // LAST: this is the ready flag
        Ok(())
    }

    /// The shared session, recovering rather than panicking on a poisoned lock: a
    /// recording never dies of a lock (the same rule every slot write here follows).
    fn lock(sess: &Arc<Mutex<Session>>) -> std::sync::MutexGuard<'_, Session> {
        sess.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    let sess = Arc::new(Mutex::new(Session {
        node: node.clone(),
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
        dropped_warmup: 0,
        error: None,
    }));

    // This attempt's OWN stop flag (DRAGON-422). The session's `stop` belongs to the
    // USER — `App::stop_recording` sets it, and it is the only record that a stop was
    // ever asked for. Everything internal here (the first-frame watchdog below, an
    // encoder or muxer failure, the stop tail) trips `zc_stop` instead, so after we
    // return the caller can still tell "the user stopped" apart from "zero-copy gave up".
    // It could not before: the internal watchdog set the shared flag, so the caller had to
    // CLEAR it to retry on the CPU path, which erased a stop the user had already made
    // and started a second recording behind the preview spinner, with nothing left in the
    // UI able to stop it.
    let zc_stop = Arc::new(AtomicBool::new(false));

    // First-frame watchdog AND the inward mirror of the session's stop, in one thread:
    // if no dmabuf frame arrives within the budget, trip `zc_stop` so `consume_dmabuf`
    // returns and we report a decline; and for as long as the session runs, propagate a
    // user stop into `zc_stop` (the only way it reaches the capture loop and the pump
    // now that they read the private flag). Exits as soon as `zc_stop` is set — which
    // the stop tail always does — so it never outlives the attempt.
    //
    // It is also what unblocks the confirmation wait below: either trip drops the
    // callback (and with it the confirm channel's sender), so a `recv_timeout` still
    // waiting sees `Disconnected` promptly.
    let got_frame = Arc::new(AtomicBool::new(false));
    {
        let session_stop = stop.clone();
        let zc = zc_stop.clone();
        let got = got_frame.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + FIRST_FRAME_BUDGET;
            loop {
                if zc.load(Ordering::Relaxed) {
                    return; // the session is ending; nothing left to mirror
                }
                if session_stop.load(Ordering::Relaxed) {
                    zc.store(true, Ordering::Relaxed);
                    return;
                }
                if !got.load(Ordering::Relaxed) && std::time::Instant::now() >= deadline {
                    log::warn!(
                        "zero-copy: no dmabuf frame within {}s; giving up on the GPU path",
                        FIRST_FRAME_BUDGET.as_secs()
                    );
                    zc.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(MIRROR_POLL);
            }
        });
    }

    // The capture thread. It owns the PipeWire mainloop for the whole session (see the fn
    // doc's "Two threads"); `consume_dmabuf` returns within `MIRROR_POLL` of `zc_stop`.
    let (confirm_tx, confirm_rx) = std::sync::mpsc::sync_channel::<ConfirmMsg>(1);
    let capture = {
        let cb_sess = sess.clone();
        let cb_stop = zc_stop.clone();
        let cb_got = got_frame.clone();
        let cb_paused = paused.clone();
        let cb_zc_stop = zc_stop.clone();
        let pre_node = node;
        let mut confirm_tx = Some(confirm_tx);
        std::thread::spawn(move || {
            crate::platform::pipewire::consume_dmabuf(fd, node_id, cb_zc_stop, move |frame| {
                cb_got.store(true, Ordering::Relaxed);
                // Paused (DRAGON-111/127): feed the encoder NOTHING — never close
                // or restart the session (unlike the legacy per-segment model);
                // the pump's clock freezes for the identical span (both read the
                // same `paused` flag), so the two naturally agree on media length
                // with no reconciliation needed (see the fn doc).
                if cb_paused.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(tx) = confirm_tx.take() {
                    // DRAGON-425 belt two — CONFIRMATION. The node was already chosen by
                    // `preflight_vaapi_node`, from the compositor's own statement of which
                    // GPU this session renders on. This frame is the second opinion: its DRM
                    // modifier's top byte names the GPU that actually produced it, so a
                    // compositor whose frames contradict its own feedback is still caught.
                    //
                    // What each belt saves is different, and only the pre-flight saves the
                    // expensive part. Declining THERE costs nothing: no stream, no audio
                    // pre-flight, no pump, no media-0 anchor. Declining HERE now costs only
                    // the stream's own bring-up, because since DRAGON-658 no audio has been
                    // started at this point either.
                    //
                    // Runs exactly once per session: the sender is taken here, and this
                    // frame is NOT encoded: there is no encoder yet, and its planes die
                    // with this call.
                    let vendor = crate::encode::vendor_from_modifier(frame.modifier);
                    let nodes = crate::encode::gpu::vaapi_nodes_with_drivers();
                    let decision = crate::encode::resolve_vaapi_node_for_vendor(&nodes, vendor);
                    // The DRAGON-419 line for this session: one log entry naming which GPU
                    // the frames came from and what that meant for the encoder.
                    log::info!(
                        "{}",
                        crate::encode::decision_log_line(
                            std::path::Path::new(&pre_node),
                            vendor,
                            &decision
                        )
                    );
                    let node = match decision {
                        crate::encode::NodeDecision::KeepDefault => pre_node.clone(),
                        crate::encode::NodeDecision::SwitchTo(path) => {
                            set_libva_driver_for(&nodes, &path);
                            path.to_string_lossy().into_owned()
                        }
                        crate::encode::NodeDecision::Decline { user, .. } => {
                            // A zero-copy DECLINE (`true`), not this recording's failure:
                            // nothing has been encoded and the CPU path can do it all. The
                            // USER half of the reason, because a fallback reason can reach a
                            // failure dialog verbatim.
                            let _ = tx.send(Err((true, user)));
                            cb_stop.store(true, Ordering::Relaxed);
                            return;
                        }
                    };
                    let _ = tx.send(Ok(FirstDmabuf {
                        width: frame.width,
                        height: frame.height,
                        node,
                        at: std::time::Instant::now(),
                    }));
                    return;
                }
                let mut z = lock(&cb_sess);
                if z.error.is_some() {
                    return;
                }
                if z.enc.is_none() {
                    // Still warming up (audio pre-flight / encoder build): there is nothing
                    // to encode with. Counted, not silent.
                    z.dropped_warmup += 1;
                    return;
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
                        // A live encode/muxer failure. Only consulted when frames == 0,
                        // where nothing was recorded and the CPU path is worth a try.
                        z.error = Some((true, e));
                        cb_stop.store(true, Ordering::Relaxed);
                    }
                }
            })
        })
    };

    /// Stop the attempt and join its capture thread, for the paths that give up before the
    /// session's own stop tail exists.
    fn abandon_capture(
        zc_stop: &Arc<AtomicBool>,
        capture: std::thread::JoinHandle<Result<(), String>>,
    ) {
        zc_stop.store(true, Ordering::Relaxed);
        let _ = capture.join();
    }

    // BOUNDED confirmation wait (DRAGON-118). The watchdog above independently trips
    // `zc_stop` on its own timeout and on a user stop, which drops the callback and
    // disconnects this channel, so the two bounds agree rather than stacking.
    let first = match confirm_rx.recv_timeout(FIRST_FRAME_BUDGET) {
        Ok(Ok(f)) => f,
        Ok(Err((can_fall_back, reason))) => {
            abandon_capture(&zc_stop, capture);
            return if can_fall_back {
                ZcOutcome::Fallback(reason)
            } else {
                ZcOutcome::Done(Err(reason))
            };
        }
        // THE field case (DRAGON-422): the compositor never handed us a dmabuf frame. With
        // the DRAGON-658 ordering nothing was recorded AND no audio was ever started, so
        // this is a clean decline and the CPU readback path is exactly the answer.
        Err(_) => {
            abandon_capture(&zc_stop, capture);
            return ZcOutcome::Fallback(
                "zero-copy: no dmabuf frames (compositor declined dmabuf?)".to_string(),
            );
        }
    };
    if let Ok(mut g) = dims.lock() {
        *g = Some((first.width, first.height));
    }

    // WARM (DRAGON-657/658): the stream has provably delivered a real frame. Report it,
    // then start audio, and nothing may come between the two.
    super::owned::mark_first_frame(warm_at, first.at);
    let owned = match super::owned::try_start_owned_audio() {
        Ok(o) => {
            log::info!("zero-copy pipeline: media-clock owned path (DRAGON-127)");
            o
        }
        Err(reason) => {
            log::error!("zero-copy pipeline: audio pre-flight failed ({reason}); cannot record");
            abandon_capture(&zc_stop, capture);
            // Fatal, not `Fallback` (see the fn doc): the CPU path would only run the
            // SAME pre-flight again and fail the same way, one whole recording later.
            return ZcOutcome::Done(Err(format!("could not start recording audio: {reason}")));
        }
    };
    let super::owned::OwnedAudioStart {
        capture_start, mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx,
    } =
        owned;

    let pump_cfg = super::pump::PumpConfig {
        fps: fps.max(1),
        audio_offset_ms,
        auto_device_compensation,
        mic_on0: mic,
        sys_on0: system_audio,
        duck_system: crate::audio::config::recording_duck_system(),
    };
    // Media 0 is HERE, not at the audio pre-flight (DRAGON-672): the file begins where
    // the warming spinner clears, which is the same instant the user is told recording
    // started. See `owned::media_zero` for the whole argument.
    let session_start =
        super::owned::media_zero(capture_start, start_gate.as_deref(), &mic_rx, &sys_rx);

    // The pump thread borrows `events`/`stop`/`paused` for its own lifetime (see
    // `pump::spawn`'s doc) — `std::thread::scope` is what makes that sound, exactly
    // like the CPU owned paths.
    /// One channel's mute intervals plus the session's frame/codec facts — named
    /// so the `std::thread::scope` return type below doesn't trip clippy's
    /// `type_complexity` (mirrors `pipewire::MuteIntervals`'s same reason).
    type OwnedZcResult = (Vec<(f64, f64)>, Vec<(f64, f64)>, u64, bool);
    /// A failed session, and whether it is a zero-copy decline the caller may still
    /// cover with the CPU path (`true`) or this recording's own failure (`false`) —
    /// the scope's error half of the [`ZcOutcome`] split.
    type OwnedZcError = (bool, String);
    let scope_result: Result<OwnedZcResult, OwnedZcError> = std::thread::scope(|scope| {
            let (pump_handle, _ticker) = match super::pump::spawn(
                scope, session_start, pump_cfg, mic_fifo_path.clone(), sys_fifo_path.clone(),
                mic_tap, mic_rx, monitor, sys_rx, &zc_stop, &paused, events,
            ) {
                Ok(v) => v,
                // The audio captures are gone with the failed spawn (see `pump::spawn`'s
                // own note) and nothing was recorded; the CPU path starts clean.
                Err(e) => {
                    abandon_capture(&zc_stop, capture);
                    return Err((true, e));
                }
            };

            // Now legally on the scope-owning thread: build the encoder and the muxer from
            // the confirmed metadata, which also flips the session to READY. A failure is
            // recorded and stopped rather than returned, so the shared stop tail below
            // still tears the pump and the capture thread down exactly as it always did.
            let build = {
                let mut z = lock(&sess);
                start_encoding(&mut z, &first, &mic_fifo_path, &sys_fifo_path, &paused)
            };
            if let Err(err) = build {
                lock(&sess).error = Some(err);
                zc_stop.store(true, Ordering::Relaxed);
            } else {
                // SETTLED (DRAGON-661). This arm has no opening catch-up phase to end (see
                // `record_screencopy_zero_copy_owned`'s doc for why that was deliberately
                // left alone), so its equivalent instant is the one right here: the encoder
                // and the muxer exist, the session is READY, and the capture callback stops
                // dropping what arrives. Everything the UI's live declaration is waiting on
                // has happened, and nothing further is bring-up.
                super::owned::mark_settled(settled_at, std::time::Instant::now());
            }

            // The capture thread runs the session from here; it returns once `zc_stop` is
            // set (by the user's stop through the mirror thread, by the build failure
            // above, or by a live encode failure in the callback).
            let run = capture
                .join()
                .unwrap_or_else(|_| Err("the zero-copy capture thread panicked".to_string()));

            // Stop: the pump IS the audio drain — join its control thread first
            // (mirrors every other owned path's stop tail exactly). The PRIVATE flag —
            // the session's `stop` is the user's to set, never ours (DRAGON-422).
            zc_stop.store(true, Ordering::Relaxed);
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
            let mut z = lock(&sess);
            let covered = z.frames as f64 / z.fps.max(1) as f64;
            let residual = pump_out.final_media - covered;
            if residual > 0.0 {
                log::info!(
                    "zero-copy owned session: video covers {covered:.3}s vs audio media \
                     {:.3}s (residual {residual:.3}s, expected ≤ ~1/{} frame period plus the \
                     warmup gap, see record_pipewire_zero_copy_owned's doc)",
                    pump_out.final_media, z.fps.max(1),
                );
            }
            if z.dropped_warmup > 0 {
                log::info!(
                    "zero-copy owned session: {} frame(s) arrived while the session was still \
                     warming up (audio pre-flight + encoder build) and had nothing to be \
                     encoded with",
                    z.dropped_warmup
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
                    return Err((true, format!("zero-copy capture: {e}")));
                }
                log::warn!("zero-copy capture ended early ({e}); keeping what's recorded");
            }
            if let Some((can_fall_back, e)) = error {
                if frames == 0 {
                    return Err((can_fall_back, e));
                }
                log::warn!("zero-copy error after recording started ({e}); keeping what's recorded");
            }
            if frames == 0 {
                // A frame WAS confirmed (this session got past the wait above), so the
                // stream declining outright is no longer what this means: nothing survived
                // the warmup. Nothing was recorded either way: a decline, and the CPU
                // readback path is exactly the answer, IF the session is still wanted.
                return Err((
                    true,
                    "zero-copy: no frames were encoded after the first one was confirmed"
                        .to_string(),
                ));
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
                        // Frames WERE encoded and the mux failed anyway: this session's
                        // own failure, not something a retry would fix.
                        return Err((false, "zero-copy muxer failed".to_string()));
                    }
                }
            }
            Ok((pump_out.mic_off, pump_out.sys_off, frames, is_hevc))
        });

    let (mic_off, sys_off, frames, is_hevc) = match scope_result {
        Ok(v) => v,
        Err((true, e)) => return ZcOutcome::Fallback(e),
        Err((false, e)) => return ZcOutcome::Done(Err(e)),
    };
    let _ = frames;
    ZcOutcome::Done(super::finalize::finalize_with_intervals(
        &temp, out_path, &mic_off, &sys_off, is_hevc, metadata,
    ))
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

/// DRM modifier sentinel meaning "implicit / driver's choice". DRAGON-425 unified the three
/// copies of this literal behind one ungated const; the alias keeps the use sites unchanged.
#[cfg(feature = "zero-copy")]
const DRM_MOD_INVALID: u64 = crate::encode::DRM_FORMAT_MOD_INVALID;

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

/// Which in-process GPU encoder a zero-copy session is running on.
///
/// VAAPI is the historical path and stays the default everywhere it works. NVENC
/// exists because it is the ONLY one that works on a session rendering on NVIDIA:
/// the proprietary driver exposes no VAAPI encode node, so `encode::vaapi_node`
/// has nothing to pick and DRAGON-425 declines. Without this arm the recording
/// falls back to reading every frame back to the CPU (29.5 MB per frame at
/// 5120x1440) only to re-upload it to the same GPU for NVENC anyway.
#[cfg(all(target_os = "linux", feature = "zero-copy"))]
enum ZcEncoder {
    Vaapi(crate::encode::gpu::Encoder),
    /// NVENC fed from CUDA. The imports are cached per capture buffer: screencopy
    /// cycles a small pool of them, and importing is ~45ms of setup that must happen
    /// ONCE per buffer, not once per frame (a refresh is ~0.25ms).
    Nvenc {
        enc: crate::encode::gpu::NvencEncoder,
        imports: std::collections::HashMap<i32, crate::encode::cuda::CudaImport>,
        /// Encode size. The GL blit inside the import scales the captured frame to
        /// this, which is how the max-resolution cap is honoured without a CPU step.
        ew: u32,
        eh: u32,
        /// How many opening frames have been logged (see `encode`).
        seen: u32,
    },
}

#[cfg(all(target_os = "linux", feature = "zero-copy"))]
impl ZcEncoder {
    /// Open the encoder for this session, preferring NVENC when the capture buffers
    /// live on an NVIDIA node (where VAAPI cannot encode them at all).
    #[allow(clippy::too_many_arguments)]
    fn new(
        node: &str,
        hevc: bool,
        cw: u32,
        ch: u32,
        ew: u32,
        eh: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, String> {
        let nvidia = crate::encode::list_render_nodes()
            .iter()
            .any(|(p, drv)| drv == "nvidia" && p.to_string_lossy() == node);
        if nvidia && crate::encode::cuda::available() {
            // A decline here is honest and specific (an H.264 request too wide to
            // encode without a downscale we cannot do), so it goes to the caller as
            // the fallback reason rather than being retried as VAAPI, which on this
            // node cannot work either.
            let enc = crate::encode::gpu::NvencEncoder::new(hevc, cw, ch, ew, eh, fps, bitrate_kbps)?;
            log::info!(
                "zero-copy: NVENC on {node} (frames stay on the GPU; no CPU readback)"
            );
            return Ok(ZcEncoder::Nvenc { enc, imports: std::collections::HashMap::new(), ew, eh, seen: 0 });
        }
        crate::encode::gpu::Encoder::new(node, hevc, cw, ch, ew, eh, fps, bitrate_kbps)
            .map(ZcEncoder::Vaapi)
    }

    /// Encode one captured frame from `target`, returning the compressed bytes.
    fn encode(&mut self, target: &DmabufTarget) -> Result<Vec<u8>, String> {
        match self {
            ZcEncoder::Vaapi(enc) => {
                enc.encode_dmabuf(target.fourcc, target.modifier, &target.encode_planes())
            }
            ZcEncoder::Nvenc { enc, imports, ew, eh, seen } => {
                use std::os::fd::{AsFd, AsRawFd};
                let (fd, offset, stride) =
                    target.planes.first().ok_or("dmabuf target has no planes")?;
                // Keyed by fd: screencopy hands back the same buffers round-robin, so
                // this imports each one once and then only refreshes it.
                let key = fd.as_raw_fd();
                if let std::collections::hash_map::Entry::Vacant(slot) = imports.entry(key) {
                    if !crate::encode::cuda::CudaDevice::format_supported(target.fourcc) {
                        return Err(format!(
                            "the compositor's format 0x{:08x} is not a packed 32-bit RGB \
                             format NVENC can take without a conversion",
                            target.fourcc
                        ));
                    }
                    let dev = crate::encode::cuda::CudaDevice::get()
                        .ok_or("CUDA import went away mid-session")?;
                    let desc = crate::encode::cuda::DmabufDesc {
                        fd: fd.as_fd(),
                        fourcc: target.fourcc,
                        modifier: target.modifier,
                        width: target.width,
                        height: target.height,
                        offset: *offset,
                        stride: *stride,
                        dst_width: *ew,
                        dst_height: *eh,
                    };
                    slot.insert(dev.import(desc)?);
                }
                // Guaranteed present: inserted directly above when absent.
                let import = imports.get(&key).expect("just inserted");
                let t0 = std::time::Instant::now();
                let out = enc.encode_import(import);
                // The muxer watchdog gives the first frame a bounded budget, so the
                // opening frames are exactly where a stall would hide. Log a few and
                // then stop: this is the difference between "we fed it nothing" and
                // "we fed it something it would not write".
                if *seen < 3 {
                    *seen += 1;
                    match &out {
                        Ok(b) => log::info!(
                            "zero-copy NVENC: frame {} -> {} bytes in {:.1}ms",
                            *seen,
                            b.len(),
                            t0.elapsed().as_secs_f64() * 1000.0
                        ),
                        Err(e) => log::error!("zero-copy NVENC: frame {} failed: {e}", *seen),
                    }
                }
                out
            }
        }
    }

    /// Flush the encoder at end of session and return its remaining packets.
    fn finish(&mut self) -> Result<Vec<u8>, String> {
        match self {
            ZcEncoder::Vaapi(enc) => enc.finish(),
            ZcEncoder::Nvenc { enc, .. } => enc.flush(),
        }
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

/// Diagnostic: the DRAGON-457 proof — take one real compositor dmabuf and import it
/// into CUDA, so an NVIDIA-rendered session can be encoded by NVENC where it lies
/// instead of paying a readback.
///
/// Separate from [`screencopy_dmabuf_test`] rather than folded into it: that probe
/// answers "does dmabuf capture work at all", which is a question every vendor has,
/// and its output is quoted in tickets. This one answers the NVIDIA-only follow-up
/// and is expected to decline on AMD and Intel boxes.
#[cfg(all(target_os = "linux", feature = "zero-copy"))]
pub fn cuda_import_test() -> String {
    use std::os::fd::AsFd;

    if crate::encode::cuda::CudaDevice::get().is_none() {
        return "cuda-import-test: no CUDA/EGL import on this machine (no NVIDIA driver, or \
                EGL_EXT_image_dma_buf_import missing). Expected on AMD/Intel — VAAPI is their \
                path."
            .into();
    }
    let Some((conn, mut queue, mut data)) = connect(false) else {
        return "cuda-import-test: wayland connect failed".into();
    };
    let qh = queue.handle();
    let Some((output, name, _, _)) = outputs(&data).into_iter().next() else {
        return "cuda-import-test: no outputs".into();
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
        return "cuda-import-test: screencopy session failed".into();
    };
    let _ = conn.flush();
    let mut guard = 0;
    while data.formats.is_none() {
        if queue.blocking_dispatch(&mut data).is_err() {
            return "cuda-import-test: dispatch failed".into();
        }
        guard += 1;
        if guard > 200 {
            return "cuda-import-test: capture formats never arrived".into();
        }
    }
    let formats = data.formats.clone().expect("format wait loop above exits only when Some");
    let target = match DmabufTarget::new(&formats, &data.dmabuf_state, &qh) {
        Ok(t) => t,
        Err(e) => return format!("cuda-import-test: could not allocate a dmabuf target: {e}"),
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
    if !matches!(data.result.clone(), Some(Ok(_))) {
        return format!("cuda-import-test: the compositor never filled the buffer on {name}");
    }

    if !crate::encode::cuda::CudaDevice::format_supported(target.fourcc) {
        return format!(
            "cuda-import-test: the compositor's format 0x{:08x} is not a packed 32-bit RGB \
             format, so this import path declines it (NVENC would need a conversion first).",
            target.fourcc
        );
    }
    let Some((fd, offset, stride)) = target.planes.first() else {
        return "cuda-import-test: the buffer reported no planes".into();
    };
    // Guaranteed Some by the `get().is_none()` check at the top.
    let dev = crate::encode::cuda::CudaDevice::get().expect("checked above");
    let t0 = std::time::Instant::now();
    let desc = crate::encode::cuda::DmabufDesc {
        fd: fd.as_fd(),
        fourcc: target.fourcc,
        modifier: target.modifier,
        width: target.width,
        height: target.height,
        offset: *offset,
        stride: *stride,
        dst_width: (target.width / 2) & !1,
        dst_height: (target.height / 2) & !1,
    };
    match dev.import(desc) {
        Ok(import) => {
            let (ptr, pitch) = import.device_ptr();
            let import_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let t1 = std::time::Instant::now();
            let refresh = import.refresh();
            let refresh_ms = t1.elapsed().as_secs_f64() * 1000.0;
            // The half that matters: can NVENC actually read what we imported? Encode a
            // few frames and report the compressed bytes, so "the import worked" cannot
            // be mistaken for "a recording would work".
            // The probe picks HEVC for a wide capture because H.264 cannot encode it
            // and this path cannot downscale. A real session takes that decision from
            // the user's codec setting via `plan.rs`, not from here.
            let ew = (target.width / 2) & !1;
            let eh = (target.height / 2) & !1;
            let encode = match crate::encode::gpu::NvencEncoder::new(
                ew > 4096,
                target.width,
                target.height,
                ew,
                eh,
                30,
                8000,
            ) {
                Ok(mut enc) => {
                    let t2 = std::time::Instant::now();
                    let mut bytes = 0usize;
                    let mut stream: Vec<u8> = Vec::new();
                    let mut err = None;
                    for _ in 0..30 {
                        match enc.encode_import(&import) {
                            Ok(pkt) => {
                                bytes += pkt.len();
                                stream.extend_from_slice(&pkt);
                            }
                            Err(e) => {
                                err = Some(e);
                                break;
                            }
                        }
                    }
                    if let Ok(tail) = enc.flush() {
                        bytes += tail.len();
                        stream.extend_from_slice(&tail);
                    }
                    // Write the elementary stream out so it can be fed to ffprobe: the
                    // muxer downstream takes raw Annex-B on stdin, and "NVENC produced
                    // bytes" is not the same claim as "those bytes are a parseable
                    // stream".
                    let dump = std::env::temp_dir().join("cck-nvenc-probe.h265");
                    let _ = std::fs::write(&dump, &stream);
                    log::info!("cuda-import-test: elementary stream written to {}", dump.display());
                    let per_frame = t2.elapsed().as_secs_f64() * 1000.0 / 30.0;
                    match err {
                        Some(e) => format!("NVENC encode FAILED: {e}"),
                        None => format!(
                            "NVENC encoded 30 frames -> {bytes} bytes, {per_frame:.2}ms per \
                             frame (import + encode, no CPU copy)"
                        ),
                    }
                }
                Err(e) => format!("NVENC encoder would not open: {e}"),
            };
            format!(
                "cuda-import-test: SUCCESS on output {name}\n  \
                 device={} size={}x{} format=0x{:08x} modifier=0x{:016x}\n  \
                 imported to CUDA device memory: ptr=0x{ptr:x} pitch={pitch}\n  \
                 import={import_ms:.2}ms, per-frame refresh={refresh_ms:.2}ms \
                 (device-to-device blit){}\n  \
                 {encode}",
                target.render_node,
                target.width,
                target.height,
                target.fourcc,
                target.modifier,
                match refresh {
                    Ok(()) => String::new(),
                    Err(e) => format!(" (refresh FAILED: {e})"),
                },
            )
        }
        Err(e) => format!(
            "cuda-import-test: import FAILED on output {name}: {e}\n  \
             device={} size={}x{} format=0x{:08x} modifier=0x{:016x}",
            target.render_node, target.width, target.height, target.fourcc, target.modifier,
        ),
    }
}

/// Result of attempting zero-copy recording (both workers — see
/// [`record_screencopy_zero_copy`] and [`record_pipewire_zero_copy`]).
///
/// The distinction is load-bearing, not cosmetic (DRAGON-422): `Fallback` is a promise
/// that NOTHING was recorded and the session is still free to be started over on the CPU
/// path, and it is the ONLY outcome a caller may respond to by starting one. Anything
/// that ran and then failed is this recording's result and must be reported as such —
/// a second session started behind a user who has already stopped records what they
/// never asked for and hands back a result nobody is waiting for any more.
#[cfg(feature = "zero-copy")]
pub(crate) enum ZcOutcome {
    /// Zero-copy couldn't start (no dmabuf / cross-vendor encoder / spawn failed);
    /// the caller should use the CPU path. No frames were recorded.
    Fallback(String),
    /// Zero-copy ran to completion — this is the final recording result.
    Done(Result<PathBuf, String>),
}

/// How long [`record_pipewire_zero_copy_owned`] waits for the portal stream's FIRST
/// dmabuf frame before declining the GPU path. Unchanged since the path was written;
/// named here because the failure it produces is now classified rather than guessed at.
#[cfg(feature = "zero-copy")]
const FIRST_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_secs(4);

/// How many capture buffers the screencopy zero-copy session cycles, so the compositor
/// never overwrites one the GPU is still encoding (single-buffer reuse would tear). Named
/// (DRAGON-658) because the confirm step and the steady loop now live in different
/// functions and must agree on it.
#[cfg(feature = "zero-copy")]
const ZC_POOL: usize = 3;

/// Poll interval of the watchdog/mirror thread — the most a user's stop can be delayed
/// on its way into the zero-copy session's private stop flag. Far below one frame at any
/// supported rate, so a stop still costs at most a frame of extra recording.
#[cfg(feature = "zero-copy")]
const MIRROR_POLL: std::time::Duration = std::time::Duration::from_millis(20);

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
/// ORDER (DRAGON-658): this CONFIRMS its own capture first (allocate the pool, open the
/// encoder, capture ONE real frame and encode it), and only then reports the warm instant
/// ([`super::owned::mark_first_frame`]) and runs the audio pre-flight
/// ([`super::owned::try_start_owned_audio`]), inline, exactly once. Every decline is
/// therefore free: nothing audio-side has been started on either side of it, and the
/// caller (`record::screencopy`) simply carries on with its CPU readback path, which then
/// runs the session's ONE pre-flight itself.
///
/// That is a change of shape from the arrangement this replaces, where the caller already
/// held an `OwnedAudioStart` before this was called and this ran a SECOND one: a decline
/// then threw a whole pre-flight away (two mic ffmpegs, two pulse clients, two pairs of
/// FIFOs) on every zero-copy fallback. Both zero-copy siblings now classify a failed
/// pre-flight the same way, as this recording's FAILURE ([`ZcOutcome::Done`]) rather than
/// a fallback, because the CPU path would only run the same pre-flight against the same
/// sound server and fail the same way, one whole recording later.
///
/// `stop`/`paused` take owned `Arc<AtomicBool>` (not a bare `&AtomicBool`, unlike this
/// file's other zero-copy caller convention) because the owned variant needs a genuine
/// `Arc` to hand to [`super::MuxerWatchdog::arm_gated`] (its own background thread isn't
/// scoped). `record::screencopy`'s caller already holds a real `Arc`, so this costs it
/// nothing.
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
    // The countdown gate (DRAGON-673): the app raises it when it promotes the recording, and
    // media 0 waits for it, so a countdown warms the whole pipeline behind its own digits and
    // the file still starts where the promotion does.
    start_gate: Option<Arc<AtomicBool>>,
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
    warm_at: &Mutex<Option<std::time::Instant>>,
    settled_at: &Mutex<Option<std::time::Instant>>,
    metadata: &str,
) -> ZcOutcome {
    let confirmed = match confirm_screencopy_zero_copy(
        conn, queue, data, qh, session, formats, fps, codec, max_res, bitrate_kbps, dims,
    ) {
        Ok(c) => c,
        // Nothing was started, on either side: the caller's CPU path can have the whole
        // recording, including the session's one and only audio pre-flight.
        Err(e) => return ZcOutcome::Fallback(e),
    };
    // WARM (DRAGON-657/658): a real frame has been captured AND encoded, so the GPU path
    // is proven end to end. Report it, then start audio, and nothing may come between.
    super::owned::mark_first_frame(warm_at, confirmed.first_at);
    match super::owned::try_start_owned_audio() {
        Ok(owned) => {
            log::info!("screencopy zero-copy pipeline: media-clock owned path (DRAGON-127)");
            record_screencopy_zero_copy_owned(
                conn, queue, data, qh, session, start_gate, mic, system_audio, audio_offset_ms,
                auto_device_compensation, out_path, stop, paused, events, settled_at, metadata,
                confirmed, owned,
            )
        }
        Err(reason) => {
            log::error!(
                "zero-copy pipeline: audio pre-flight failed ({reason}); cannot record"
            );
            ZcOutcome::Done(Err(format!("could not start recording audio: {reason}")))
        }
    }
}

/// A CONFIRMED screencopy zero-copy capture (DRAGON-658): the buffer pool, the open
/// encoder, and the FIRST real frame already captured and encoded.
///
/// The first frame's compressed bytes are carried as a plain `Vec<u8>`, not as a borrowed
/// handle, which is what makes the ordering safe: they sit here across the whole audio
/// pre-flight and are written to the muxer the moment it exists.
#[cfg(feature = "zero-copy")]
struct ZcConfirmed {
    targets: Vec<DmabufTarget>,
    enc: ZcEncoder,
    hevc: bool,
    fps: u32,
    /// The compressed bytes of the first real captured frame (possibly empty: the encoder
    /// may hold the packet back, exactly as in the steady loop).
    first_bytes: Vec<u8>,
    /// The instant the compositor handed back that first frame.
    first_at: std::time::Instant,
    /// The pool index the steady loop starts from (the confirm used index 0).
    next_idx: usize,
}

/// Bring the GPU zero-copy capture up and PROVE it works, before any audio exists: the
/// dmabuf pool, the encode geometry, the encoder itself, and one real capture+encode
/// round trip. Every failure is a `Err(reason)` the caller turns into
/// [`ZcOutcome::Fallback`], and every one of them is free, because this touches nothing
/// but the compositor and the GPU.
///
/// Split out of the owned session (DRAGON-658) precisely so the confirm can happen BEFORE
/// the audio pre-flight rather than after it.
#[cfg(feature = "zero-copy")]
#[allow(clippy::too_many_arguments)]
fn confirm_screencopy_zero_copy(
    conn: &Connection,
    queue: &mut EventQueue<ScreencopyClient>,
    data: &mut ScreencopyClient,
    qh: &QueueHandle<ScreencopyClient>,
    session: &CaptureSession,
    formats: &Formats,
    fps: u32,
    codec: &str,
    max_res: (u32, u32),
    bitrate_kbps: u32,
    dims: &Mutex<Option<(u32, u32)>>,
) -> Result<ZcConfirmed, String> {
    // We cycle a small pool of buffers so the compositor never overwrites one the GPU is
    // still encoding (single-buffer reuse would tear).
    let mut targets: Vec<DmabufTarget> = Vec::with_capacity(ZC_POOL);
    for _ in 0..ZC_POOL {
        targets.push(DmabufTarget::new(formats, &data.dmabuf_state, qh)?);
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
    let mut enc = ZcEncoder::new(&targets[0].render_node, hevc, cw, ch, ew, eh, fps, bitrate_kbps)
        .map_err(|e| format!("encoder init: {e}"))?;

    // ONE real capture + encode. This is the confirmation the whole ordering rests on: an
    // encoder that opened but cannot import the compositor's buffers (cross-vendor) fails
    // HERE, where declining still costs nothing.
    let damage = [Rect { x: 0, y: 0, width: cw as i32, height: ch as i32 }];
    data.result = None;
    session.capture(&targets[0].buffer, &damage, qh, ScreencopyFrameData::default());
    conn.flush().map_err(|e| format!("zero-copy first capture: {e}"))?;
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
    if !matches!(data.result.clone(), Some(Ok(_))) {
        return Err("the compositor never filled the first zero-copy buffer".to_string());
    }
    // The pixels became real when the compositor handed the buffer back, which is what the
    // warmup signal must name.
    let first_at = std::time::Instant::now();
    let first_bytes = enc.encode(&targets[0]).map_err(|e| format!("gpu encode: {e}"))?;
    Ok(ZcConfirmed { targets, enc, hevc, fps, first_bytes, first_at, next_idx: 1 })
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
///
/// Called with a `confirmed` capture that has already produced and encoded a real frame
/// and an `owned` audio pre-flight that has already succeeded, in that order (DRAGON-658).
/// `confirmed.first_bytes` is written to the muxer as soon as it is spawned.
///
/// One consequence of that order is worth naming: the confirmed frame is captured just
/// BEFORE media 0 and the next one only after the pre-flight and the muxer spawn, so the
/// video stream carries no frame for the pre-flight's span and the content after it leads
/// the audio by that much. This is the same constant lead the path had before the
/// reordering (where the whole GPU bring-up sat AFTER media 0 instead), not a new one, and
/// `-shortest` plus the trailing coverage below still keep the two streams the same
/// length. Covering the span properly would mean an opening burst of re-encoded copies of
/// the confirmed frame, the dmabuf analogue of the RGBA workers' opening phase; that is
/// deliberately NOT done here (DRAGON-658 kept its scope to the ordering).
///
/// With no such phase to end, DRAGON-661's `settled_at` reports at the first write below,
/// where the confirmed frame reaches the muxer and the steady grab loop takes over. That is
/// this path's equivalent instant, and every successful session must reach it, so the UI's
/// live declaration cannot be left waiting on a signal a whole worker never sends.
#[cfg(feature = "zero-copy")]
#[allow(clippy::too_many_arguments)]
fn record_screencopy_zero_copy_owned(
    conn: &Connection,
    queue: &mut EventQueue<ScreencopyClient>,
    data: &mut ScreencopyClient,
    qh: &QueueHandle<ScreencopyClient>,
    session: &CaptureSession,
    // The countdown gate (DRAGON-673): the app raises it when it promotes the recording, and
    // media 0 waits for it, so a countdown warms the whole pipeline behind its own digits and
    // the file still starts where the promotion does.
    start_gate: Option<Arc<AtomicBool>>,
    mic: bool,
    system_audio: bool,
    audio_offset_ms: i32,
    auto_device_compensation: bool,
    out_path: &std::path::Path,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    events: &Mutex<Vec<ToggleEvent>>,
    settled_at: &Mutex<Option<std::time::Instant>>,
    metadata: &str,
    confirmed: ZcConfirmed,
    owned: super::owned::OwnedAudioStart,
) -> ZcOutcome {
    let ZcConfirmed { targets, mut enc, hevc, fps, first_bytes, first_at: _, next_idx } =
        confirmed;
    // The pool's buffers ARE the capture size (see `DmabufTarget::new`).
    let (cw, ch) = (targets[0].width, targets[0].height);
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // --- committed: run the capture+encode session to completion ---
    // The format/modifier travel with each target now: `ZcEncoder::encode` reads them
    // off the buffer it is handed, since the NVENC arm keys its imports per buffer.
    let damage = [Rect { x: 0, y: 0, width: cw as i32, height: ch as i32 }];
    let frame_dur = std::time::Duration::from_secs_f64(1.0 / fps as f64);

    let super::owned::OwnedAudioStart {
        capture_start, mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx,
    } =
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
    // Media 0 is HERE, not at the audio pre-flight (DRAGON-672): the file begins where
    // the warming spinner clears, which is the same instant the user is told recording
    // started. See `owned::media_zero` for the whole argument.
    let session_start =
        super::owned::media_zero(capture_start, start_gate.as_deref(), &mic_rx, &sys_rx);
    // The confirm used pool index 0, so the steady loop picks up from the next one.
    let mut last_idx: usize = next_idx;

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
        // The confirmed first frame (DRAGON-658): captured and encoded before the audio
        // pre-flight, held as bytes across it, and written the moment the muxer exists.
        // Empty bytes still count as a frame, exactly as in the steady loop below: the
        // encoder may hold the packet back, but the frame WAS fed.
        let mut frames: u64 = 1;
        let opening_ok = first_bytes.is_empty() || stdin.write_all(&first_bytes).is_ok();

        if opening_ok {
            // SETTLED (DRAGON-661). This arm has no opening catch-up phase to end (see this
            // function's own doc for why that was deliberately left alone), so its
            // equivalent instant is the one right here: the confirmed frame has reached the
            // muxer and the steady grab loop takes over from the next line. Everything the
            // UI's live declaration is waiting on has happened. Inside the `opening_ok` test
            // for the same reason as every other worker: a muxer that would not take the
            // first write is dying, not settling.
            super::owned::mark_settled(settled_at, std::time::Instant::now());
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
                let idx = last_idx % ZC_POOL;
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
                    Some(Ok(_)) => match enc.encode(target) {
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
                let last_target = &targets[last_idx.wrapping_sub(1) % ZC_POOL];
                for _ in 0..extra {
                    match enc.encode(last_target) {
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
