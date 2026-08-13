//! Region/monitor screencopy recording (CPU/readback path): we own the capture —
//! grab the region's output each frame, crop it, and pipe raw frames to ffmpeg.

use cosmic_client_toolkit::screencopy::{
    CaptureOptions, CaptureSession, CaptureSource, ScreencopySessionData,
};
use cosmic_client_toolkit::sctk::shm::slot::{Buffer, SlotPool};
use crate::screencopy::{ScreencopyClient, connect, grab_cropped, grab_frame, outputs, pick_format};
use super::ToggleEvent;
use super::owned::{
    OwnedAudioStart, make_frame_writer, mark_first_frame, mark_settled, media_zero,
    run_video_stop_tail,
    try_start_owned_audio, MuteIntervals,
};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wayland_client::{Connection, EventQueue, QueueHandle};
#[cfg(feature = "zero-copy")]
use super::zero_copy::{ZcOutcome, record_screencopy_zero_copy};

// ---------------------------------------------------------------------------
// Media-clock OWNED path (DRAGON-127; the ONLY recording path — the legacy
// wallclock+CFR+segments fallback was retired here). Structural difference from
// the PipeWire worker: screencopy GRABS frames on demand rather than receiving a
// pushed stream, so there is no separate video-consumer thread/channel to fork
// around: the owned loop owns its video capture directly on the calling thread.
//
// ORDER (DRAGON-658). `record_screencopy` brings the VIDEO side up first:
// connect, settle, pick the output, open the capture session, negotiate formats.
// Only once something has provably captured a real frame does it report that
// (`owned::mark_first_frame` → `RecordHandle::warm_at`) and start the audio
// pre-flight, inline, exactly once. If the pre-flight fails, recording fails
// outright with a named, actionable reason; there is no fallback. The OTHER end of the
// bring-up, where the opening phase has paid its media debt and the session is steady, is
// reported separately (`owned::mark_settled` → `RecordHandle::settled_at`, DRAGON-661);
// that later signal is the one the UI calls "live".
//
// That order also COLLAPSES A DOUBLE PRE-FLIGHT this path used to run. The audio
// pre-flight was the entry point's first statement, before Wayland was even
// connected, and the GPU zero-copy attempt then ran a SECOND, independent one of
// its own; every zero-copy DECLINE therefore threw one whole pre-flight away
// (two mic ffmpegs, two pulse clients, two pairs of FIFOs) before the CPU path
// carried on with the first. Now each branch confirms its own first frame and
// starts audio once, so one user action starts exactly one audio session on
// every route through this file (`owned::preflights_started` counts them, and
// `zc_fallback_live_tests` asserts on that count).
// ---------------------------------------------------------------------------

/// A CONFIRMED CPU readback capture: the live Wayland session and buffer state, the crop
/// rect the first real frame fixed, and that frame's own pixels.
///
/// Bundled so the session runner takes one parameter instead of fourteen, and so the
/// confirm step (everything that must succeed BEFORE any audio exists, DRAGON-658) has
/// one obvious return value.
struct CpuCapture {
    conn: Connection,
    queue: EventQueue<ScreencopyClient>,
    data: ScreencopyClient,
    qh: QueueHandle<ScreencopyClient>,
    /// ONE persistent capture session for the whole recording (per-frame sessions are
    /// ~4x slower and cannot hit the target fps).
    session: CaptureSession,
    /// ONE reused shm buffer, for the same reason.
    buffer: Buffer,
    pool: SlotPool,
    /// Full capture-buffer width in px (the crop below indexes into it).
    cw: u32,
    swizzle: bool,
    force_opaque: bool,
    /// The crop rect in buffer px, fixed by the first frame for the whole run.
    bx: u32,
    by: u32,
    bw: u32,
    bh: u32,
    /// The first frame, already cropped to the recorded region.
    first: Vec<u8>,
}

/// How long the opening phase may spend getting level with the media clock before it hands
/// over to the steady-state loop. A BOUND, not a target (DRAGON-118: nothing in the record
/// path waits unboundedly): the phase normally ends within a few hundred milliseconds, the
/// moment the clock owes it nothing. Deliberately a LOCAL constant with the same value as
/// the other workers' own: these are independent capture shapes, and hoisting one shared
/// number would tie their tuning together for nothing.
const OPENING_COVER_BUDGET: Duration = Duration::from_secs(2);

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_screencopy(
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    fps: u32,
    cursor: bool,
    preferred_encoder: &str,
    encoder_hint: Option<&str>,
    presets: &crate::encode::Presets,
    mic: bool,
    system_audio: bool,
    bitrate_kbps: u32,
    audio_offset_ms: i32,
    auto_device_compensation: bool,
    max_res: (u32, u32),
    // The countdown gate (DRAGON-673): the app raises it when it promotes the recording, and
    // media 0 waits for it, so a countdown warms the whole pipeline behind its own digits and
    // the file still starts where the promotion does.
    start_gate: Option<Arc<AtomicBool>>,
    out_path: &std::path::Path,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    events: &Mutex<Vec<ToggleEvent>>,
    dims: &Mutex<Option<(u32, u32)>>,
    warm_at: &Mutex<Option<Instant>>,
    settled_at: &Mutex<Option<Instant>>,
    metadata: &str,
    zero_copy: bool,
) -> Result<PathBuf, String> {
    let (conn, mut queue, mut data) =
        connect(false).ok_or_else(|| "wayland connect failed".to_string())?;

    // Let the overlay finish switching into its recording state (dim only, no
    // border lines) before the first frame, so the recorded region is clean.
    std::thread::sleep(Duration::from_millis(250));

    // Output the region overlaps most (region recording stays on one output).
    let (sx0, sy0, sx1, sy1) = (x, y, x + w as i32, y + h as i32);
    let Some((output, _name, (ox, oy), (ow, _oh))) = outputs(&data)
        .into_iter()
        .max_by_key(|(_, _, (ox, oy), (ow, oh))| {
            let ix = sx1.min(ox + ow) - sx0.max(*ox);
            let iy = sy1.min(oy + oh) - sy0.max(*oy);
            (ix.max(0) as i64) * (iy.max(0) as i64)
        })
    else {
        return Err("no output for region".to_string());
    };
    let src = CaptureSource::Output(output);
    let qh = queue.handle();

    // ONE persistent capture session + ONE reused buffer for the whole recording —
    // this is what lets us hit the target fps (per-frame sessions are ~4x slower).
    let options = if cursor {
        CaptureOptions::PaintCursors
    } else {
        CaptureOptions::empty()
    };
    data.formats = None;
    let Ok(session) =
        data.screencopy_state.capturer().create_session(&src, options, &qh, ScreencopySessionData::default())
    else {
        return Err("screencopy session failed".to_string());
    };
    if let Err(e) = conn.flush() {
        return Err(e.to_string());
    }
    let mut guard = 0;
    while data.formats.is_none() {
        if let Err(e) = queue.blocking_dispatch(&mut data) {
            return Err(e.to_string());
        }
        guard += 1;
        if guard > 200 {
            return Err("capture formats never arrived".to_string());
        }
    }
    let Some(formats) = data.formats.clone() else {
        return Err("no capture formats".to_string());
    };
    let (cw, ch) = formats.buffer_size;

    // Full-output (monitor) GPU zero-copy: an opt-in attempt. It confirms its OWN first
    // captured frame and starts its OWN (single) audio pre-flight, so a decline here costs
    // nothing but the encoder probe: no audio has been started on either side of it.
    #[cfg(feature = "zero-copy")]
    if zero_copy {
        match record_screencopy_zero_copy(
            &conn,
            &mut queue,
            &mut data,
            &qh,
            &session,
            &formats,
            fps,
            &presets.codec,
            max_res,
            start_gate.clone(),
            mic,
            system_audio,
            bitrate_kbps,
            audio_offset_ms,
            auto_device_compensation,
            out_path,
            stop.clone(),
            paused.clone(),
            events,
            dims,
            warm_at,
            settled_at,
            metadata,
        ) {
            ZcOutcome::Done(r) => return r,
            ZcOutcome::Fallback(e) => {
                eprintln!(
                    "screencopy zero-copy unavailable ({e}); using the readback path, \
                     still hardware-encoded by ffmpeg"
                );
            }
        }
    }
    #[cfg(not(feature = "zero-copy"))]
    let _ = zero_copy;

    let Some((format, swizzle, force_opaque)) = pick_format(&formats.shm_formats) else {
        return Err("no usable shm format".to_string());
    };
    let stride = cw * 4;
    let Ok(mut pool) = SlotPool::new((stride * ch) as usize, &data.shm) else {
        return Err("shm pool allocation failed".to_string());
    };
    let Ok((buffer, _)) = pool.create_buffer(cw as i32, ch as i32, stride as i32, format) else {
        return Err("shm buffer allocation failed".to_string());
    };

    // First frame fixes the scale + crop rect (in buffer px) for the whole run.
    let Some(first) = grab_frame(
        &conn, &mut queue, &mut data, &qh, &session, &buffer, &mut pool, cw, ch, swizzle,
        force_opaque,
    ) else {
        return Err("initial frame capture failed".to_string());
    };
    // Stamped AFTER the grab returns, not before it: this is a synchronous on-demand
    // capture, so the pixels became real when the compositor handed them back, and that is
    // what the warmup signal must name.
    let first_at = Instant::now();
    let scale = first.width() as f32 / (ow.max(1)) as f32;
    let gx0 = sx0.max(ox);
    let gy0 = sy0.max(oy);
    let gx1 = sx1.min(ox + ow);
    let gy1 = sy1.min(oy + (first.height() as f32 / scale) as i32);
    if gx1 <= gx0 || gy1 <= gy0 {
        return Err("region is off-screen".to_string());
    }
    let bx = (((gx0 - ox) as f32) * scale).round().max(0.0) as u32;
    let by = (((gy0 - oy) as f32) * scale).round().max(0.0) as u32;
    let even = |v: u32| v - (v % 2); // h264 needs even dimensions
    let bw = even((((gx1 - gx0) as f32) * scale).round() as u32).min(even(first.width().saturating_sub(bx)));
    let bh = even((((gy1 - gy0) as f32) * scale).round() as u32).min(even(first.height().saturating_sub(by)));
    if bw < 2 || bh < 2 {
        return Err("region is too small to record".to_string());
    }

    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Cap the ENCODE size to the user's max resolution, the chosen codec's side limit
    // (H.264 ≤ 4096), and the encoder hard max, rescaling the captured region down
    // (aspect preserved) when it exceeds the box, so no oversized frame reaches the
    // encoder regardless of backend.
    let (mw, mh) = crate::encode::codec_capped_resolution(max_res, &presets.codec);
    let (ew, eh) = crate::encode::fit_within(bw, bh, mw, mh);
    // Publish the CAPTURED footprint (pre-cap) so the UI can open the preview
    // sized to the on-screen area, not the possibly-smaller encode
    // (see `RecordHandle::dims`).
    if let Ok(mut g) = dims.lock() {
        *g = Some((bw, bh));
    }
    let plan = super::resolve_session_plan(preferred_encoder, encoder_hint, ew, eh, presets);

    // WARM (DRAGON-657/658). `first` is a real captured frame, so the compositor has
    // provably handed us pixels: report that instant, then start audio.
    //
    // This is the LAST thing before the pre-flight and nothing may come between them:
    // everything above is video bring-up that can still fail with no audio started (so
    // those paths just return), and everything below needs the pre-flight's FIFO paths.
    mark_first_frame(warm_at, first_at);
    // The pre-flight runs INLINE here, exactly once per recording. It is bounded by its
    // own internal budgets (FIFO creation, the capture-chain starts, two
    // `OWNED_AUDIO_SMOKE_BUDGET` smoke checks), so it adds no unbounded wait
    // (DRAGON-118). Its entry instant is media 0.
    let owned = match try_start_owned_audio() {
        Ok(o) => {
            log::info!("recording pipeline: media-clock owned path (DRAGON-127)");
            o
        }
        Err(reason) => {
            log::error!("recording pipeline: audio pre-flight failed ({reason}); cannot record");
            return Err(format!("could not start recording audio: {reason}"));
        }
    };

    let video = CpuCapture {
        conn,
        queue,
        data,
        qh,
        session,
        buffer,
        pool,
        cw,
        swizzle,
        force_opaque,
        bx,
        by,
        bw,
        bh,
        first: image::imageops::crop_imm(&first, bx, by, bw, bh).to_image().into_raw(),
    };
    record_screencopy_owned(
        video, fps, plan, ew, eh, bitrate_kbps, audio_offset_ms, auto_device_compensation,
        start_gate, mic, system_audio, out_path, stop, paused, events, settled_at, metadata, owned,
    )
}

/// The media-clock owned screencopy session (DRAGON-127): ONE continuous ffmpeg
/// for the whole recording (index-stamped video via
/// [`crate::encode::spawn_ffmpeg_media_clock`], audio rendered by [`super::pump`]'s
/// `Mixer`-backed engine) — the exact same shape as `pipewire::record_pipewire_owned`,
/// adapted for on-demand grabs (see the section doc above).
///
/// Called with a `video` capture that has ALREADY produced a real frame and an `owned`
/// audio pre-flight that has already succeeded, in that order (DRAGON-658). `owned` is
/// consumed here: its FIFOs/tap/monitor become the pump's. `settled_at` is filled where
/// the opening phase below hands over to the steady-state loop (DRAGON-661).
#[allow(clippy::too_many_arguments)]
fn record_screencopy_owned(
    video: CpuCapture,
    fps: u32,
    plan: crate::encode::EncodePlan,
    ew: u32,
    eh: u32,
    bitrate_kbps: u32,
    audio_offset_ms: i32,
    auto_device_compensation: bool,
    // The countdown gate (DRAGON-673): the app raises it when it promotes the recording, and
    // media 0 waits for it, so a countdown warms the whole pipeline behind its own digits and
    // the file still starts where the promotion does.
    start_gate: Option<Arc<AtomicBool>>,
    mic: bool,
    system_audio: bool,
    out_path: &std::path::Path,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    events: &Mutex<Vec<ToggleEvent>>,
    settled_at: &Mutex<Option<Instant>>,
    metadata: &str,
    owned: OwnedAudioStart,
) -> Result<PathBuf, String> {
    let CpuCapture {
        conn, mut queue, mut data, qh, session, buffer, mut pool, cw, swizzle, force_opaque,
        bx, by, bw, bh, first,
    } = video;
    let nv12 = plan.nv12;
    let is_hevc = plan.is_hevc();
    let frame_dur = Duration::from_secs_f64(1.0 / fps as f64);

    let OwnedAudioStart {
        capture_start, mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx,
    } = owned;
    let temp = super::recording_temp_path(out_path);
    let mut child = match crate::encode::spawn_ffmpeg_media_clock(
        bw, bh, ew, eh, fps.max(1), &plan, bitrate_kbps, &temp, &mic_fifo_path, &sys_fifo_path,
    ) {
        Ok(c) => c,
        Err(e) => {
            drop(mic_tap);
            let _ = monitor.stop();
            let _ = std::fs::remove_file(&mic_fifo_path);
            let _ = std::fs::remove_file(&sys_fifo_path);
            return Err(e);
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        drop(mic_tap);
        let _ = monitor.stop();
        let _ = std::fs::remove_file(&mic_fifo_path);
        let _ = std::fs::remove_file(&sys_fifo_path);
        return Err("ffmpeg stdin unavailable".to_string());
    };

    let mut write_frame = make_frame_writer(bw, bh, nv12);
    // The frame that fixed the crop, already cropped to the recorded region: the frame
    // the CFR contract re-feeds until a fresh grab replaces it.
    let mut last: Vec<u8> = first;
    // Media 0 is HERE, not at the audio pre-flight (DRAGON-672): the file begins where
    // the warming spinner clears, which is the same instant the user is told recording
    // started. See `owned::media_zero` for the whole argument.
    let session_start = media_zero(capture_start, start_gate.as_deref(), &mic_rx, &sys_rx);
    let pump_cfg = super::pump::PumpConfig {
        fps: fps.max(1),
        audio_offset_ms,
        auto_device_compensation,
        mic_on0: mic,
        sys_on0: system_audio,
        duck_system: crate::audio::config::recording_duck_system(),
    };

    // The pump thread borrows `events` for its own lifetime (see
    // `pump::PumpHandle`'s doc) — `std::thread::scope` is what makes that sound.
    let scope_result: Result<(MuteIntervals, MuteIntervals), String> = std::thread::scope(|scope| {
        let (pump_handle, mut ticker) = match super::pump::spawn(
            scope, session_start, pump_cfg, mic_fifo_path.clone(), sys_fifo_path.clone(), mic_tap,
            mic_rx, monitor, sys_rx, &stop, &paused, events,
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = child.kill();
                return Err(e);
            }
        };

        // Muxer liveness: identical shape/thresholds to the legacy path
        // (DRAGON-118), pause-aware exactly like `pipewire::record_pipewire_owned`'s
        // (see its doc for why: this single session-long ffmpeg is fed NOTHING on
        // any input while paused, by design, so paused time can never prove — or
        // disprove — muxer liveness).
        let mut liveness_left =
            Some(std::time::Duration::from_secs(super::MUXER_LIVENESS_SECS));
        let mut liveness_tick = std::time::Instant::now();
        let mut muxer_wedged = false;

        // MuxerWatchdog armed BEFORE the first write, pause-gated for the same
        // reason as the in-loop liveness budget above (DRAGON-118/123/125).
        let watchdog = super::MuxerWatchdog::arm_gated(child.id(), temp.clone(), paused.clone());
        // The OPENING PHASE (DRAGON-658, this path's form of the mac worker's DRAGON-656).
        // It covers media [0, level] and it is a PHASE, not a fixed-length burst: it writes
        // a tick, re-asks the clock, and ends only when the clock owes it nothing.
        //
        // Why it cannot be a fixed count. The count used to be decided once, up front, and
        // writing those frames takes real wall time (the first frames of a session are what
        // ffmpeg's encoder init absorbs). The clock keeps running through that, so the
        // burst finished owing more ticks, and the steady-state loop paid all of them on
        // one iteration with the single image it held: a hitch a third of a second in.
        //
        // Why it RE-GRABS instead of repeating one buffer. This capture is PULL-model:
        // nothing was captured during the opening span, so unlike the pushed-stream workers
        // there is no queue of already-arrived frames to place at their own ticks
        // (`pipewire::advance_to_tick`). The only two choices are to repeat the one frame
        // that fixed the crop, which is the frozen opening this ticket exists to remove, or
        // to grab live for every owed tick, which is what this does. The trade is honest
        // and worth naming: each opening tick shows content from when it was GRABBED, not
        // from the media position it occupies, so the opening plays the last few hundred
        // milliseconds of real motion compressed into the ticks the pre-flight owed. The
        // tick COUNT is the contract and is unchanged; the pixels are the worker's own call
        // (see `owned::OwnedAudioStart::capture_start`'s doc).
        let opening_deadline = Instant::now() + OPENING_COVER_BUDGET;
        // `.max(1)` keeps the historical guarantee that at least one frame is written here:
        // ffmpeg must get past its input-0 probe before it will open the FIFOs.
        let mut owed = ticker.due_video_ticks(Instant::now()).max(1);
        let mut opening_ticks: u64 = 0;
        let mut opening_ok = true;
        'opening: loop {
            while owed > 0 {
                // A genuinely fresh capture, not a repeat. A failed grab keeps `last`,
                // matching the steady-state loop's own handling below.
                if let Some((buf, _had_damage)) = grab_cropped(
                    &conn, &mut queue, &mut data, &qh, &session, &buffer, &mut pool, cw, swizzle,
                    force_opaque, bx, by, bw, bh,
                ) {
                    last = buf;
                }
                if !write_frame(bw, bh, &last, &mut stdin) {
                    opening_ok = false;
                    break 'opening;
                }
                opening_ticks += 1;
                owed -= 1;
            }
            // Bounded on both ends (DRAGON-118): the user stopping, and a budget for a
            // capture or encoder that simply cannot keep up, in which case falling through
            // to the steady-state loop is the right answer anyway.
            if stop.load(Ordering::Relaxed) || Instant::now() >= opening_deadline {
                break;
            }
            owed = ticker.due_video_ticks(Instant::now());
            if owed == 0 {
                break; // level with the clock: the steady-state loop inherits no debt
            }
        }
        if crate::util::timing_on() {
            crate::util::timing_mark(&format!("rec/sc: opening covered {opening_ticks} ticks"));
        }

        // Damage-aware skipping (mirrors the legacy path's identical optimization):
        // when the compositor reports no damage, the region is unchanged, so don't
        // bother re-converting + re-piping it — the tick loop below re-feeds `last`
        // as many times as ticks are due, which is what holds the timeline instead
        // of the legacy path's `-fps_mode cfr` duplication. Two guards keep it
        // safe: only trust empty damage once damage has actually been SEEN (so a
        // compositor that never reports it never makes us skip), and always
        // refresh at least every `keepalive` (so a stale region can't linger).
        let keepalive = std::time::Duration::from_millis(500);
        let mut damage_seen = false;
        let mut last_refreshed = std::time::Instant::now();
        let mut next = std::time::Instant::now() + frame_dur;
        // Paused-idle roundtrip cadence (mirrors the legacy path's pause loop): a
        // periodic wayland roundtrip keeps the otherwise-quiet socket drained
        // while nothing is captured.
        let mut idle_ticks: u32 = 0;

        if opening_ok {
            // SETTLED (DRAGON-661), the same placement as the mac/Windows workers. The
            // opening phase is over and the clock owes nothing, so this is the first instant
            // the whole pipeline is steady, and it is what the UI's live declaration waits
            // for. Inside the `opening_ok` test on purpose: a failed write means ffmpeg is
            // not taking frames, so that session is dying rather than settling.
            mark_settled(settled_at, Instant::now());
            'grab: loop {
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
                // Paused: the media clock is frozen (zero ticks due regardless of
                // wall time), so skip grabbing entirely and idle exactly like the
                // legacy path's pause loop — a periodic roundtrip keeps the wayland
                // socket drained; if the connection dies mid-pause, salvage what's
                // recorded (matching the legacy path's own pause-loop failure mode).
                if paused.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(15));
                    idle_ticks = idle_ticks.wrapping_add(1);
                    if idle_ticks.is_multiple_of(64) && queue.roundtrip(&mut data).is_err() {
                        break 'grab;
                    }
                    // Reset the pacing schedule so resuming doesn't see a burst of
                    // "overdue" grabs from the paused stretch.
                    next = std::time::Instant::now() + frame_dur;
                    continue;
                }
                let now = std::time::Instant::now();
                if now < next {
                    std::thread::sleep(next - now);
                }
                next += frame_dur;
                // Cap drift to one frame so a slow grab can't spiral the pacing
                // into a busy-spin trying to "catch up" (matches the legacy loop).
                if next < now {
                    next = now + frame_dur;
                }
                if stop.load(Ordering::Relaxed) || paused.load(Ordering::Relaxed) {
                    continue;
                }
                if let Some((buf, had_damage)) = grab_cropped(
                    &conn, &mut queue, &mut data, &qh, &session, &buffer, &mut pool, cw, swizzle,
                    force_opaque, bx, by, bw, bh,
                ) {
                    if had_damage {
                        damage_seen = true;
                    }
                    // Refresh `last` only when the frame actually changed (or a
                    // keepalive refresh is due) — an unchanged frame still
                    // satisfies however many ticks are due below by re-feeding the
                    // bytes already held.
                    if !damage_seen || had_damage || last_refreshed.elapsed() >= keepalive {
                        last = buf;
                        last_refreshed = std::time::Instant::now();
                    }
                }
                let due = ticker.due_video_ticks(std::time::Instant::now());
                for _ in 0..due {
                    if !write_frame(bw, bh, &last, &mut stdin) {
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

        // Stop: the pump IS the audio drain — join its control thread before
        // touching the video stdin, so the audio's true media end
        // (`final_media`) is known before deciding how many covering ticks the
        // video needs (mirrors `pipewire::record_pipewire_owned`'s stop tail
        // exactly).
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

        // Shared stop tail (DRAGON-161): NON-BLOCKING covering-tick writes (see
        // `owned::run_video_stop_tail` / `NonblockingStdin` for the DRAGON-160 wedge
        // this closes) + the bounded reap + the shared salvage decision — the SAME
        // implementation the SCK and PipeWire workers run. A covering-write failure is
        // benign (the video is already long enough) and no longer marks the muxer
        // wedged or deletes the temp.
        let more = ticker.ticks_to_cover(pump_out.final_media);
        let outcome = run_video_stop_tail(stdin, &mut child, &temp, muxer_wedged, more, |sd| {
            write_frame(bw, bh, &last, sd)
        });
        stop.store(true, Ordering::Relaxed);
        outcome.map(|()| (pump_out.mic_off, pump_out.sys_off))
    });

    let (mic_off, sys_off) = scope_result?;
    // No `measured` lag update here (unlike the PipeWire owned path): screencopy's
    // owned loop grabs and writes synchronously on one thread with no delivery
    // channel, so there is no meaningful "how late did this frame reach the
    // encoder" quantity left to sample.
    super::finalize::finalize_with_intervals(&temp, out_path, &mic_off, &sys_off, is_hevc, metadata)
}
