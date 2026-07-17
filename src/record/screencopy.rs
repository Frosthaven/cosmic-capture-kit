//! Region/monitor screencopy recording (CPU/readback path): we own the capture —
//! grab the region's output each frame, crop it, and pipe raw frames to ffmpeg.

use cosmic_client_toolkit::screencopy::{CaptureOptions, CaptureSource, ScreencopySessionData};
use cosmic_client_toolkit::sctk::shm::slot::SlotPool;
use crate::screencopy::{ScreencopyClient, connect, grab_cropped, grab_frame, outputs, pick_format};
use super::ToggleEvent;
use super::owned::{OwnedAudioStart, make_frame_writer, run_video_stop_tail, try_start_owned_audio, MuteIntervals};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wayland_client::{Connection, EventQueue};
#[cfg(feature = "zero-copy")]
use super::zero_copy::{ZcOutcome, record_screencopy_zero_copy};

// ---------------------------------------------------------------------------
// Media-clock OWNED path (DRAGON-127; the ONLY recording path — the legacy
// wallclock+CFR+segments fallback was retired here): `record_screencopy` (the
// entry point below) tries the audio-side pre-flight check FIRST; only on
// success does it commit to the owned single-session loop. If the pre-flight
// check fails, recording fails outright with a named, actionable reason instead
// of falling back. Structural difference from the PipeWire worker: screencopy
// GRABS frames on demand rather than receiving a pushed stream, so there is no
// separate video-consumer thread/channel to fork around — the owned loop owns
// its video capture directly on the calling thread.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_screencopy(
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    fps: u32,
    cursor: bool,
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
    dims: &Mutex<Option<(u32, u32)>>,
    metadata: &str,
    zero_copy: bool,
) -> Result<PathBuf, String> {
    match try_start_owned_audio() {
        Ok(owned) => {
            log::info!("recording pipeline: media-clock owned path (DRAGON-127)");
            record_screencopy_owned(
                x, y, w, h, fps, cursor, preferred_encoder, presets, mic, system_audio,
                bitrate_kbps, audio_offset_ms, auto_device_compensation, max_res, out_path,
                stop, paused, events, dims, metadata, zero_copy, owned,
            )
        }
        Err(reason) => {
            log::error!("recording pipeline: audio pre-flight failed ({reason}); cannot record");
            Err(format!("could not start recording audio: {reason}"))
        }
    }
}

/// The media-clock owned screencopy session (DRAGON-127): ONE continuous ffmpeg
/// for the whole recording (index-stamped video via
/// [`crate::encode::spawn_ffmpeg_media_clock`], audio rendered by [`super::pump`]'s
/// `Mixer`-backed engine) — the exact same shape as `pipewire::record_pipewire_owned`,
/// adapted for on-demand grabs (see the section doc above). Called only once
/// [`try_start_owned_audio`] has already confirmed both audio sources are alive;
/// `owned` is consumed here (its FIFOs/tap/monitor become the pump's) UNLESS the
/// GPU zero-copy attempt below claims the recording first, in which case it's
/// torn down unused.
#[allow(clippy::too_many_arguments)]
fn record_screencopy_owned(
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    fps: u32,
    cursor: bool,
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
    dims: &Mutex<Option<(u32, u32)>>,
    metadata: &str,
    zero_copy: bool,
    owned: OwnedAudioStart,
) -> Result<PathBuf, String> {
    let (conn, mut queue, mut data) =
        connect(false).ok_or_else(|| "wayland connect failed".to_string())?;

    // Let the overlay finish switching into its recording state (dim only, no
    // border lines) before the first frame, so the recorded region is clean.
    std::thread::sleep(std::time::Duration::from_millis(250));

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
        owned.cleanup();
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
        owned.cleanup();
        return Err("screencopy session failed".to_string());
    };
    if let Err(e) = conn.flush() {
        owned.cleanup();
        return Err(e.to_string());
    }
    let mut guard = 0;
    while data.formats.is_none() {
        if let Err(e) = queue.blocking_dispatch(&mut data) {
            owned.cleanup();
            return Err(e.to_string());
        }
        guard += 1;
        if guard > 200 {
            owned.cleanup();
            return Err("capture formats never arrived".to_string());
        }
    }
    let Some(formats) = data.formats.clone() else {
        owned.cleanup();
        return Err("no capture formats".to_string());
    };
    let (cw, ch) = formats.buffer_size;

    // Full-output (monitor) GPU zero-copy: an opt-in attempt — if it claims the
    // recording, the owned audio pre-flight above was for nothing; tear it down.
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
            metadata,
        ) {
            ZcOutcome::Done(r) => {
                owned.cleanup();
                return r;
            }
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
        owned.cleanup();
        return Err("no usable shm format".to_string());
    };
    let stride = cw * 4;
    let Ok(mut pool) = SlotPool::new((stride * ch) as usize, &data.shm) else {
        owned.cleanup();
        return Err("shm pool allocation failed".to_string());
    };
    let Ok((buffer, _)) = pool.create_buffer(cw as i32, ch as i32, stride as i32, format) else {
        owned.cleanup();
        return Err("shm buffer allocation failed".to_string());
    };
    let grab = |conn: &Connection, queue: &mut EventQueue<ScreencopyClient>, data: &mut ScreencopyClient, pool: &mut SlotPool| {
        grab_frame(conn, queue, data, &qh, &session, &buffer, pool, cw, ch, swizzle, force_opaque)
    };

    // First frame fixes the scale + crop rect (in buffer px) for the whole run.
    let Some(first) = grab(&conn, &mut queue, &mut data, &mut pool) else {
        owned.cleanup();
        return Err("initial frame capture failed".to_string());
    };
    let scale = first.width() as f32 / (ow.max(1)) as f32;
    let gx0 = sx0.max(ox);
    let gy0 = sy0.max(oy);
    let gx1 = sx1.min(ox + ow);
    let gy1 = sy1.min(oy + (first.height() as f32 / scale) as i32);
    if gx1 <= gx0 || gy1 <= gy0 {
        owned.cleanup();
        return Err("region is off-screen".to_string());
    }
    let bx = (((gx0 - ox) as f32) * scale).round().max(0.0) as u32;
    let by = (((gy0 - oy) as f32) * scale).round().max(0.0) as u32;
    let even = |v: u32| v - (v % 2); // h264 needs even dimensions
    let bw = even((((gx1 - gx0) as f32) * scale).round() as u32).min(even(first.width().saturating_sub(bx)));
    let bh = even((((gy1 - gy0) as f32) * scale).round() as u32).min(even(first.height().saturating_sub(by)));
    if bw < 2 || bh < 2 {
        owned.cleanup();
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
    let plan = crate::encode::EncodePlan::resolve(preferred_encoder, ew, eh, presets);
    let nv12 = plan.nv12;
    let is_hevc = plan.is_hevc();
    let frame_dur = std::time::Duration::from_secs_f64(1.0 / fps as f64);

    let OwnedAudioStart { mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx } = owned;
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
    // The frame that fixed the crop, cropped to the recorded region — the opening
    // frame both the legacy path's segment 0 and this owned session start with.
    let mut last: Vec<u8> = image::imageops::crop_imm(&first, bx, by, bw, bh).to_image().into_raw();
    // Media time 0 starts here, right before real ticks/audio start flowing — same
    // anchor placement as `pipewire::record_pipewire_owned`.
    let session_start = std::time::Instant::now();
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
        let opening_ticks = ticker.due_video_ticks(std::time::Instant::now()).max(1);
        let mut opening_ok = true;
        for _ in 0..opening_ticks {
            if !write_frame(bw, bh, &last, &mut stdin) {
                opening_ok = false;
                break;
            }
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
