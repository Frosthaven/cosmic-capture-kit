//! PipeWire portal recording (CPU/readback path): consume a portal ScreenCast
//! stream and pipe its frames to ffmpeg.

use super::owned::{
    make_frame_writer, run_video_stop_tail, try_start_owned_audio, MuteIntervals, OwnedAudioStart,
};
use super::{ToggleEvent, median_offset_ms, monotonic_ns};
use crate::audio::capture::MonitorCapture;
use crate::audio::clean_mic::MicTapHandle;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// One frame as the PipeWire consumer delivers it into the bounded channel:
/// `(width_px, height_px, tightly-packed RGBA, pts_ns, pw_delay_ns, recv_ns)`. The
/// last three are the per-frame timing state the auto-cal lag measurement reads (see
/// [`frame_lag_ms`]); the pixels are what the video ticker feeds ffmpeg.
type Frame = (u32, u32, Vec<u8>, i64, i64, i64);

/// The A/V lag (ms) a single delivered PipeWire `frame` implies — how long it took to
/// reach the encoder, measured EXACTLY as the in-loop path always has (the
/// capture-timestamp form when the frame carried a monotonic `pts`, else PipeWire's
/// reported source delay plus the channel transit since `recv_ns`). Factored out so the
/// drain below can sample EVERY frame it consumes, not just the one whose pixels it
/// keeps: each arrived frame is an independent, equally-valid latency observation for
/// the median (`median_offset_ms`), so draining stale frames must not silently drop
/// their lag samples. Returns `None` for a sample outside the trusted `1..=2000` ms band
/// (the same filter the loop applied inline).
fn frame_lag_ms(pts: i64, pw_delay: i64, recv_ns: i64) -> Option<i64> {
    let lag_ms = if pts > 0 {
        (monotonic_ns() - pts) / 1_000_000
    } else {
        (pw_delay + (monotonic_ns() - recv_ns)) / 1_000_000
    };
    (lag_ms > 0 && lag_ms <= 2000).then_some(lag_ms)
}

/// Drain `rx` to the FRESHEST currently-queued frame, updating `last` in place to its
/// dimensions + pixels and pushing EVERY consumed frame's lag sample into `lag_samples`
/// (see [`frame_lag_ms`]); report whether ANY frame was consumed (DRAGON-167). This is
/// the CFR contract's "feed the freshest frame" made explicit and headlessly testable,
/// generalizing the mac SCK `latch_freshest` (DRAGON-164) to the PipeWire tuple's
/// per-frame timing state:
///
/// The capture channel is bounded (8 deep). When a slow encoder backpressures the
/// blocking video write, the loop parks IN that write while the PipeWire consumer thread
/// keeps pushing — the channel fills and the consumer DROPS the newest frames
/// (`try_send` fails), leaving only stale ones. Consuming a single frame per tick (the
/// loop's bare `recv_timeout`) would then feed the OLDEST stale frame (the channel is
/// FIFO), so the video shows seconds-old content while the newest changes are discarded
/// upstream — the reported near-total freeze on a loaded capture (the Linux twin of the
/// mac SCK bug). Draining to the freshest holds `last` current every tick and keeps the
/// channel from staying full (so the consumer stops dropping fresh frames).
///
/// pts/lag semantics are PRESERVED, not corrupted: the intermediate frames' PIXELS are
/// the only thing discarded (they're stale by definition — a newer frame supersedes
/// them); their TIMING each contributes a lag sample exactly as if the old single-recv
/// loop had processed them one by one, so the median offset is unchanged (arguably more
/// robust — more samples). The media clock itself is index-stamped and never reads these
/// timestamps, so dropping stale pixels cannot skew it.
///
/// Returns `false` only when the channel was empty (nothing new / not yet delivered),
/// leaving `last` untouched so the caller re-feeds it to hold CFR.
fn latch_freshest(
    rx: &std::sync::mpsc::Receiver<Frame>,
    last: &mut (u32, u32, Vec<u8>),
    lag_samples: &mut Vec<i64>,
) -> bool {
    let mut got = false;
    while let Ok((w, h, rgba, pts, pw_delay, recv_ns)) = rx.try_recv() {
        if let Some(ms) = frame_lag_ms(pts, pw_delay, recv_ns) {
            lag_samples.push(ms);
        }
        *last = (w, h, rgba);
        got = true;
    }
    got
}

// ---------------------------------------------------------------------------
// Media-clock OWNED path (DRAGON-125 chunk B1; the legacy wallclock+CFR+segments
// fallback retired in DRAGON-127 — this is now the ONLY recording path):
// `record_pipewire` (the public entry point) runs a cheap, self-contained
// pre-flight check of the owned path's audio components FIRST (before touching
// the portal stream at all — see `try_start_owned_audio`); only if that succeeds
// does it commit to the single-session owned loop below. Any failure from that
// point on (no video frames, ffmpeg won't spawn, a wedged muxer) is an ordinary
// recording failure. If the pre-flight check itself fails, `fd` was never
// touched, and the recording fails outright with a named, actionable reason
// (pulse connection / FIFO / mic chain) instead of falling back.
// ---------------------------------------------------------------------------

/// Fold an owned-session startup failure into a single `Err` message, tearing
/// everything already-committed down first: stop the video capture (`stop` +
/// joining the PipeWire consumer thread), the mic tap, and the system monitor, and
/// remove the FIFOs. Used for every failure between "the pre-flight check passed"
/// and "the pump took ownership of the FIFOs/tap/monitor" — past that point the
/// pump's own teardown (via `PumpHandle`/`Drop`) takes over instead.
fn owned_start_failure<T>(
    stop: &Arc<AtomicBool>,
    pw: std::thread::JoinHandle<T>,
    mic_tap: MicTapHandle,
    monitor: MonitorCapture,
    mic_fifo_path: &std::path::Path,
    sys_fifo_path: &std::path::Path,
    msg: String,
) -> String {
    stop.store(true, Ordering::Relaxed);
    let _ = pw.join();
    drop(mic_tap);
    let _ = monitor.stop();
    let _ = std::fs::remove_file(mic_fifo_path);
    let _ = std::fs::remove_file(sys_fifo_path);
    msg
}

/// Record a portal/PipeWire stream through the media-clock OWNED pipeline
/// (DRAGON-125; the ONLY recording path since DRAGON-127) — the public entry point
/// `record::mod`'s `start_pipewire_recording` calls. If the audio pre-flight check
/// can't start, the recording fails outright with a named, actionable reason.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_pipewire(
    fd: OwnedFd,
    node_id: u32,
    crop: Option<(u32, u32, u32, u32)>,
    fps: u32,
    preferred_encoder: &str,
    presets: &crate::encode::Presets,
    mic: bool,
    system_audio: bool,
    bitrate_kbps: u32,
    audio_offset_ms: i32,
    auto_device_compensation: bool,
    max_res: (u32, u32),
    out_path: &std::path::Path,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    events: &Mutex<Vec<ToggleEvent>>,
    measured: &Mutex<Option<i32>>,
    dims: &Mutex<Option<(u32, u32)>>,
    metadata: &str,
) -> Result<PathBuf, String> {
    match try_start_owned_audio() {
        Ok(owned) => {
            log::info!("recording pipeline: media-clock owned path (DRAGON-125)");
            record_pipewire_owned(
                fd, node_id, crop, fps, preferred_encoder, presets, mic, system_audio,
                bitrate_kbps, audio_offset_ms, auto_device_compensation, max_res, out_path, stop,
                paused, events, measured, dims, metadata, owned,
            )
        }
        Err(reason) => {
            log::error!("recording pipeline: audio pre-flight failed ({reason}); cannot record");
            Err(format!("could not start recording audio: {reason}"))
        }
    }
}

/// The media-clock owned PipeWire session (DRAGON-125 chunk B1): ONE continuous
/// ffmpeg for the whole recording — index-stamped video
/// ([`crate::encode::spawn_ffmpeg_media_clock`]), audio rendered by
/// [`super::pump`]'s `Mixer`-backed engine — instead of the legacy
/// wallclock+CFR+per-pause-segment model. Called only once [`try_start_owned_audio`]
/// has already confirmed both audio sources are alive; `owned` is consumed here
/// (its FIFOs/tap/monitor become the pump's).
#[allow(clippy::too_many_arguments)]
fn record_pipewire_owned(
    fd: OwnedFd,
    node_id: u32,
    crop: Option<(u32, u32, u32, u32)>,
    fps: u32,
    preferred_encoder: &str,
    presets: &crate::encode::Presets,
    mic: bool,
    system_audio: bool,
    bitrate_kbps: u32,
    audio_offset_ms: i32,
    auto_device_compensation: bool,
    max_res: (u32, u32),
    out_path: &std::path::Path,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    events: &Mutex<Vec<ToggleEvent>>,
    measured: &Mutex<Option<i32>>,
    dims: &Mutex<Option<(u32, u32)>>,
    metadata: &str,
    owned: OwnedAudioStart,
) -> Result<PathBuf, String> {
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let OwnedAudioStart { mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx } = owned;

    // Same PipeWire consumer shape as the legacy path (untouched): a small
    // jitter-buffered channel, dropping (never blocking) when the encoder side is
    // behind, discarding frames while paused — nothing is captured then; the
    // pump's video ticks independently freeze for the same span (see
    // `super::pump::VideoTicker`).
    let (ftx, frx) = std::sync::mpsc::sync_channel::<(u32, u32, Vec<u8>, i64, i64, i64)>(8);
    let pw_stop = stop.clone();
    let pw_paused = paused.clone();
    let pw = std::thread::spawn(move || {
        let mut dropped: u64 = 0;
        crate::platform::pipewire::consume_frames(fd, node_id, crop, pw_stop, move |rgba, w, h, pts, pw_delay| {
            if pw_paused.load(Ordering::Relaxed) {
                return;
            }
            if ftx.try_send((w, h, rgba, pts, pw_delay, monotonic_ns())).is_err() {
                dropped += 1;
                if dropped == 1 || dropped.is_multiple_of(60) {
                    log::warn!(
                        "pipewire capture (media-clock): encoder backlog — {dropped} frames \
                         dropped so far (video will look choppy if this keeps up)"
                    );
                }
            }
        })
    });
    let mut all_lag_samples: Vec<i64> = Vec::new();

    // The first frame fixes the encode size — identical bounded wait to the
    // legacy path (paused waits don't count against the 10s budget).
    let (w0, h0, first, _pts0, _delay0, _recv0) = {
        let mut waited_s = 0u32;
        loop {
            match frx.recv_timeout(std::time::Duration::from_secs(1)) {
                Ok(f) => break f,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    if paused.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    waited_s += 1;
                    if waited_s >= 10 || stop.load(Ordering::Relaxed) {
                        return Err(owned_start_failure(
                            &stop, pw, mic_tap, monitor, &mic_fifo_path, &sys_fifo_path,
                            "no frames from the PipeWire stream".to_string(),
                        ));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(owned_start_failure(
                        &stop, pw, mic_tap, monitor, &mic_fifo_path, &sys_fifo_path,
                        "no frames from the PipeWire stream".to_string(),
                    ));
                }
            }
        }
    };
    let (mw, mh) = crate::encode::codec_capped_resolution(max_res, &presets.codec);
    let (ew, eh) = crate::encode::fit_within(w0, h0, mw, mh);
    if let Ok(mut g) = dims.lock() {
        *g = Some((w0, h0));
    }
    let plan = crate::encode::EncodePlan::resolve(preferred_encoder, ew, eh, presets);
    let nv12 = plan.nv12;
    let is_hevc = plan.is_hevc();

    let temp = super::recording_temp_path(out_path);
    let mut child = match crate::encode::spawn_ffmpeg_media_clock(
        w0, h0, ew, eh, fps.max(1), &plan, bitrate_kbps, &temp, &mic_fifo_path, &sys_fifo_path,
    ) {
        Ok(c) => c,
        Err(e) => {
            return Err(owned_start_failure(
                &stop, pw, mic_tap, monitor, &mic_fifo_path, &sys_fifo_path, e,
            ));
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return Err(owned_start_failure(
            &stop, pw, mic_tap, monitor, &mic_fifo_path, &sys_fifo_path,
            "ffmpeg stdin unavailable".to_string(),
        ));
    };

    let mut write_frame = make_frame_writer(w0, h0, nv12);
    let mut last: (u32, u32, Vec<u8>) = (w0, h0, first);
    // ffmpeg's own startup (encoder + FIFO opens) didn't read the video pipe, so
    // frames captured meanwhile were dropped at the channel — skip to the
    // freshest one available before the first write (same reasoning as the
    // legacy path's identical step), crediting each skipped frame's lag sample.
    latch_freshest(&frx, &mut last, &mut all_lag_samples);
    // Media time 0 starts here — right before we start feeding real ticks/audio —
    // so startup jitter above doesn't skew the clock's anchor.
    let session_start = std::time::Instant::now();
    let pump_cfg = super::pump::PumpConfig {
        fps: fps.max(1),
        audio_offset_ms,
        auto_device_compensation,
        mic_on0: mic,
        sys_on0: system_audio,
        duck_system: crate::audio::config::recording_duck_system(),
    };

    // The pump thread borrows `events` (a plain `&Mutex`, not an `Arc` — see
    // `super::pump::PumpHandle`'s doc) for its own lifetime, which is what
    // `std::thread::scope` makes sound: every thread spawned inside it is joined
    // before this block returns (and `pump_handle.join()` below does so
    // explicitly, well before that). `pump::spawn` returns IMMEDIATELY (its FIFO
    // rendezvous happens on the pump thread) — the opening video write a few lines
    // down must follow promptly, because those stdin bytes are what let ffmpeg get
    // past its input-0 probe and reach the FIFOs the pump is waiting to open (see
    // `pump::open_fifo_write_end`'s doc for the wedge this ordering prevents).
    let scope_result: Result<(MuteIntervals, MuteIntervals), String> = std::thread::scope(|scope| {
        let (pump_handle, mut ticker) = match super::pump::spawn(
            scope, session_start, pump_cfg, mic_fifo_path.clone(), sys_fifo_path.clone(), mic_tap,
            mic_rx, monitor, sys_rx, &stop, &paused, events,
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = child.kill();
                stop.store(true, Ordering::Relaxed);
                let _ = pw.join();
                return Err(e);
            }
        };

        // Stall detection + muxer liveness: identical shape/thresholds to the
        // legacy path (DRAGON-118) — except PAUSE-AWARE (unlike the legacy path,
        // whose pauses end the segment outright): this single session-long ffmpeg
        // is fed NOTHING on any input while paused, by design, so paused time can
        // never prove — or disprove — muxer liveness. The budget is spent only
        // while unpaused; a user pausing before the encoder's first output packet
        // (lookahead not yet filled) must not read as a wedge and get the
        // recording killed with its temp deleted (the DRAGON-122-integration
        // pause crash).
        const STALL_WARN_SECS: f32 = 4.0;
        let mut stalled_secs = 0.0f32;
        let mut stall_logged = false;
        let mut liveness_left =
            Some(std::time::Duration::from_secs(super::MUXER_LIVENESS_SECS));
        let mut liveness_tick = std::time::Instant::now();
        let mut muxer_wedged = false;

        // MuxerWatchdog armed BEFORE the first write (the DRAGON-118/123 wedge
        // strikes there): an ffmpeg whose scheduler wedged at start never reads
        // stdin, so a plain blocking write_all could hang forever. Pause-gated,
        // for the same reason as the in-loop liveness budget above.
        let watchdog = super::MuxerWatchdog::arm_gated(child.id(), temp.clone(), paused.clone());
        let opening_ticks = ticker.due_video_ticks(std::time::Instant::now()).max(1);
        let mut opening_ok = true;
        for _ in 0..opening_ticks {
            if !write_frame(last.0, last.1, &last.2, &mut stdin) {
                opening_ok = false;
                break;
            }
        }
        if opening_ok {
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if liveness_left.is_some() {
                    let now = std::time::Instant::now();
                    let elapsed = now.saturating_duration_since(liveness_tick);
                    liveness_tick = now;
                    let left = liveness_left.as_mut().expect("checked is_some above");
                    if !paused.load(Ordering::Relaxed) {
                        *left = left.saturating_sub(elapsed);
                    }
                    if left.is_zero() {
                        if super::muxer_alive(&temp) {
                            liveness_left = None;
                            watchdog.disarm();
                        } else {
                            log::error!(
                                "recording muxer (media-clock) wrote no output in {}s while \
                                 being fed frames — wedged ffmpeg; aborting (DRAGON-118)",
                                super::MUXER_LIVENESS_SECS
                            );
                            muxer_wedged = true;
                            break;
                        }
                    }
                }
                match frx.recv_timeout(std::time::Duration::from_millis(200)) {
                    Ok((w, h, rgba, pts, pw_delay, recv_ns)) => {
                        if stall_logged {
                            log::warn!(
                                "pipewire capture (media-clock) resumed after a \
                                 {stalled_secs:.0}s stall"
                            );
                        }
                        stalled_secs = 0.0;
                        stall_logged = false;
                        // Same auto-cal lag measurement as before (now via the shared
                        // `frame_lag_ms`): the median feeds `measured_offset_ms`.
                        if let Some(ms) = frame_lag_ms(pts, pw_delay, recv_ns) {
                            all_lag_samples.push(ms);
                        }
                        last = (w, h, rgba);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        stalled_secs += 0.2;
                        if stalled_secs >= STALL_WARN_SECS && !stall_logged {
                            stall_logged = true;
                            log::error!(
                                "pipewire capture (media-clock) stalled: no video frame for \
                                 {stalled_secs:.0}s while recording — the video track will \
                                 appear frozen from here (audio continues)"
                            );
                        }
                    }
                    // The portal stream ended: the session is over regardless of
                    // the app's own `stop` (there is only ever one "segment"
                    // here, unlike the legacy path's per-pause restart).
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                // DRAGON-167: DRAIN to the freshest queued frame before feeding ticks.
                // The single `recv_timeout` above took the OLDEST queued frame (FIFO);
                // under encoder backpressure the channel backs up and that is stale by
                // seconds while the newest changes sit behind it (and get dropped as the
                // channel stays full). `latch_freshest` advances `last` to the newest
                // available frame and credits each skipped frame's lag sample, so the
                // video tracks the screen and the auto-cal median is unchanged. A quiet
                // channel leaves `last` alone (the CFR re-feed holds the timeline).
                latch_freshest(&frx, &mut last, &mut all_lag_samples);
                // Feed however many video ticks the media clock owes since the
                // last feed — 0 most cycles (paused, or not yet due), a small
                // burst after a scheduling hiccup or a slow-arriving frame.
                let due = ticker.due_video_ticks(std::time::Instant::now());
                for _ in 0..due {
                    if !write_frame(last.0, last.1, &last.2, &mut stdin) {
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
        } else {
            muxer_wedged = true;
        }
        if watchdog.fired() {
            muxer_wedged = true;
        }
        watchdog.disarm();

        // Stop: the pump IS the audio drain (DRAGON-78's mic/system
        // drain-before-EOF, generalized) — join its control thread before
        // touching the video stdin, so the audio's true media end
        // (`final_media`) is known and every sample is queued to the writer
        // (whose EOFs follow as it flushes; `-shortest` ends the mux at the
        // shorter STREAM, so video EOF landing first is fine — both streams are
        // fed to the same media end either way).
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

        // Feed enough FINAL ticks to cover AT LEAST the audio's true media end —
        // the mirror-image of the legacy path's stop-tail last-frame rewrite:
        // there, the video was extended PAST the drained audio so `-shortest`
        // couldn't cut it; here the audio (FIFOs) already closed with its own
        // true end, so the video must instead be extended to at least that same
        // media position, so `-shortest` trims video down to audio's end rather
        // than truncating audio.
        //
        // Shared stop tail (DRAGON-161): the covering-tick writes are NON-BLOCKING
        // now — an ffmpeg whose audio was the shorter `-shortest` stream stops
        // draining the video pipe without closing it, so a plain blocking write of a
        // full-monitor frame could park in the kernel forever (the DRAGON-160 wedge,
        // reproduced headlessly). A covering-write failure is BENIGN (the video is
        // already long enough), so — unlike this path's OLD blocking loop, which set
        // `muxer_wedged` on a covering-write failure and then DELETED the temp — it
        // just stops the loop. Same reap + salvage decision the SCK and screencopy
        // workers run, one implementation.
        let more = ticker.ticks_to_cover(pump_out.final_media);
        let outcome = run_video_stop_tail(stdin, &mut child, &temp, muxer_wedged, more, |sd| {
            write_frame(last.0, last.1, &last.2, sd)
        });
        stop.store(true, Ordering::Relaxed); // release the PipeWire consumer thread
        let _ = pw.join();
        outcome.map(|()| (pump_out.mic_off, pump_out.sys_off))
    });

    let (mic_off, sys_off) = scope_result?;
    if let Ok(mut g) = measured.lock() {
        *g = median_offset_ms(all_lag_samples);
    }
    // The intervals are already on the file's OWN media timeline (the pump's FIFO
    // output IS the final audio) — no delay shift, no segment-offset mapping.
    super::finalize::finalize_with_intervals(&temp, out_path, &mic_off, &sys_off, is_hevc, metadata)
}

#[cfg(test)]
mod tests {
    use super::{latch_freshest, Frame};
    use std::sync::mpsc::sync_channel;

    /// Build a PipeWire-shaped frame whose first pixel byte is `marker` (so we can tell
    /// which one was latched) and whose `pts` is 0 (so `frame_lag_ms` uses the
    /// delay+transit form; here delay/recv are 0, giving no valid sample — kept out of the
    /// way of the pixel-latch assertions).
    fn frame(marker: u8) -> Frame {
        (4, 4, vec![marker; 4 * 4 * 4], 0, 0, 0)
    }

    /// The DRAGON-167 invariant, headless (the Linux twin of the mac SCK
    /// `latch_freshest_takes_the_newest_queued_frame`): given several frames already
    /// queued in the bounded capture channel — as happens the moment a slow encoder
    /// backpressures the video write and the PipeWire consumer keeps pushing — the worker
    /// MUST latch the FRESHEST one, not the oldest. A bare single-recv consumes the oldest
    /// (the channel is FIFO), which is exactly the stale-frame feed that froze the
    /// recording. Pins the fix with no portal, PipeWire, or ffmpeg.
    #[test]
    fn latch_freshest_takes_the_newest_queued_frame() {
        let (tx, rx) = sync_channel::<Frame>(8);
        for i in 0u8..8 {
            tx.try_send(frame(i)).expect("channel has room for 8");
        }
        let mut last: (u32, u32, Vec<u8>) = (4, 4, vec![255u8; 4 * 4 * 4]);
        let mut lag: Vec<i64> = Vec::new();
        assert!(latch_freshest(&rx, &mut last, &mut lag), "consumed at least one frame");
        assert_eq!(last.2[0], 7, "must latch the FRESHEST (last-pushed) frame, not the oldest");
        // Drained, so the consumer can push fresh frames again instead of hitting a full
        // channel and dropping the newest.
        assert!(!latch_freshest(&rx, &mut last, &mut lag), "empty channel leaves `last` untouched");
        assert_eq!(last.2[0], 7, "an empty drain must not disturb the held frame");
    }

    /// An empty channel (a static screen / not-yet-delivered: the PipeWire consumer sent
    /// nothing) must leave the held frame alone so the CFR loop re-feeds it — the "a tick
    /// with no fresh frame re-feeds the last" half of the contract.
    #[test]
    fn latch_freshest_holds_last_when_nothing_queued() {
        let (_tx, rx) = sync_channel::<Frame>(8);
        let mut last: (u32, u32, Vec<u8>) = (2, 2, vec![9u8; 2 * 2 * 4]);
        let mut lag: Vec<i64> = Vec::new();
        assert!(!latch_freshest(&rx, &mut last, &mut lag), "nothing to consume");
        assert_eq!(last.2[0], 9, "held frame is unchanged");
        assert!(lag.is_empty(), "no frames means no lag samples");
    }

    /// pts/lag semantics are PRESERVED across a multi-frame drain (the DRAGON-167 care
    /// point): every DRAINED frame — not just the freshest whose pixels we keep — must
    /// still contribute its own lag sample, because each arrived frame is an independent,
    /// equally-valid latency observation for the median. A drain that silently dropped the
    /// intermediate frames' timing would starve `median_offset_ms` (needs ≥5 samples) and
    /// skew auto-calibration. Uses `pts > 0` frames with a monotonic-past timestamp so
    /// `frame_lag_ms` yields a valid (positive, ≤2000ms) sample for each.
    #[test]
    fn latch_freshest_credits_every_drained_frames_lag_sample() {
        let (tx, rx) = sync_channel::<Frame>(8);
        // A pts ~100ms in the past (relative to `monotonic_ns()` at drain time) yields a
        // ~100ms lag sample per frame; well inside the trusted 1..=2000ms band.
        let now = super::monotonic_ns();
        for i in 0u8..6 {
            // (w, h, rgba, pts, pw_delay, recv_ns): a valid past pts, so the lag form is
            // `monotonic_ns() - pts` — positive and small.
            tx.try_send((4, 4, vec![i; 4 * 4 * 4], now - 100_000_000, 0, 0))
                .expect("room");
        }
        let mut last: (u32, u32, Vec<u8>) = (4, 4, vec![0u8; 4 * 4 * 4]);
        let mut lag: Vec<i64> = Vec::new();
        assert!(latch_freshest(&rx, &mut last, &mut lag));
        assert_eq!(last.2[0], 5, "freshest pixels latched");
        assert_eq!(
            lag.len(),
            6,
            "EVERY drained frame must contribute a lag sample (got {}), not just the kept one",
            lag.len()
        );
        assert!(
            lag.iter().all(|&m| (1..=2000).contains(&m)),
            "each sample is a plausible latency: {lag:?}"
        );
    }

    /// The freeze dynamic in miniature, deterministic and headless (the Linux twin of the
    /// mac SCK `draining_keeps_the_consumed_frame_recent_under_backpressure`): a producer
    /// fills the bounded channel faster than a SLOW consumer drains it — the loaded-encoder
    /// case where the blocking video write throttles the loop. The stale-feed bug consumed
    /// ONE frame per iteration (the oldest), so the "video" lagged far behind the producer;
    /// `latch_freshest` keeps the consumed frame close to the newest sent. Pins the exact
    /// staleness the fix removes, with no portal/PipeWire/ffmpeg.
    #[test]
    fn draining_keeps_the_consumed_frame_recent_under_backpressure() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        fn seq_frame(seq: u32) -> Frame {
            let mut px = vec![0u8; 4 * 4 * 4];
            px[0..4].copy_from_slice(&seq.to_le_bytes());
            (4, 4, px, 0, 0, 0)
        }
        fn seq_of(f: &(u32, u32, Vec<u8>)) -> u32 {
            u32::from_le_bytes([f.2[0], f.2[1], f.2[2], f.2[3]])
        }

        // Run the SAME producer against two consumers: `drain=false` mimics the old
        // single-recv (consume one, oldest first); `drain=true` uses `latch_freshest`.
        // Return the worst gap between what the producer had sent and what the consumer
        // holds (staleness).
        let worst_gap = |drain: bool| -> u32 {
            let (tx, rx) = sync_channel::<Frame>(8);
            let sent = Arc::new(AtomicU32::new(0));
            let sent_p = sent.clone();
            let prod = std::thread::spawn(move || {
                for seq in 0..400u32 {
                    if tx.send(seq_frame(seq)).is_err() {
                        break;
                    }
                    sent_p.store(seq, Ordering::Relaxed);
                }
            });
            let mut last: (u32, u32, Vec<u8>) = (4, 4, vec![0u8; 4 * 4 * 4]);
            let mut lag: Vec<i64> = Vec::new();
            let mut worst = 0u32;
            for _ in 0..60 {
                if let Ok((w, h, rgba, ..)) = rx.recv() {
                    last = (w, h, rgba);
                }
                if drain {
                    latch_freshest(&rx, &mut last, &mut lag);
                }
                let ahead = sent.load(Ordering::Relaxed).saturating_sub(seq_of(&last));
                worst = worst.max(ahead);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            drop(rx);
            let _ = prod.join();
            worst
        };

        let stale_gap = worst_gap(false);
        let fresh_gap = worst_gap(true);
        assert!(
            fresh_gap * 4 < stale_gap.max(1),
            "draining should stay far fresher: fresh_gap={fresh_gap} stale_gap={stale_gap}"
        );
    }
}
