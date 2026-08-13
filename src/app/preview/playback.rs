//! Inline video playback for the preview overlay, driven by the `ffmpeg` binary (no
//! GStreamer, no `iced_video_player` — so no second copy of iced to keep version-matched
//! with libcosmic).
//!
//! A reader thread decodes scaled raw-RGBA frames from one `ffmpeg` process *ahead* of
//! playback into a bounded buffer (backpressured when full). The UI [`Playback::poll`]s on
//! a timer and presents the frame due NOW against the audio clock — dropping late frames and
//! holding when ahead — so motion is smooth regardless of decode/timer jitter (a single
//! latest-frame slot dropped/repeated frames and flickered).
//!
//! **The audio clock is the TRUE playout position, the way real players do it** (DRAGON
//! preview-hitch). When the file has audio, a second `ffmpeg` decodes it to raw f32 PCM on a
//! pipe and we play it through an output sink WE OWN, then anchor the picture to where that
//! sink says the audio actually IS: on macOS an AudioQueue (`platform::mac::audio_out`,
//! `AudioQueueGetCurrentTime`), on Linux a `pa_simple` PulseAudio stream (`audio::pulse_out`,
//! `pa_simple_get_latency`). No estimating a sink buffer and correcting the estimate, which
//! was the old approach and which froze the picture ~1s in when the correction landed. The
//! [`PreviewAudioSink`] trait is the seam; [`audio_pump`] drives it. On Windows the bundled
//! ffmpeg has no audio-output muxer at all, so the soundtrack is still an `ffplay` sidecar
//! (SDL2 → the default endpoint) with no position stream, so the Windows picture rides the
//! bootstrap epoch clock like a no-audio file (DRAGON-285) until it gets its own WASAPI sink.
//! The [`AUDIO_LATENCY_MS`] bootstrap now only holds the poster for the moments before the
//! first real anchor lands (mac/linux), or the whole session (Windows).
//!
//! Large recordings (4K+) get three defenses, modelled on what real players do:
//! * **Hardware decode, software-safe**: every decode passes `-hwaccel auto`, which is
//!   best-effort BY DESIGN in the ffmpeg CLI — device types that fail to create are
//!   skipped, a codec/profile the driver can't do falls back to software decode with a
//!   warning, and (without `-hwaccel_output_format`) decoded frames are auto-downloaded
//!   to system memory — so the `scale`+RGBA pipe below is identical either way and the
//!   portable baseline (ffmpeg alone) always stands. Verified in ffmpeg's
//!   `fftools/ffmpeg_dec.c` (`HWACCEL_AUTO` device loop → "Auto hwaccel disabled" on
//!   total failure) and `libavcodec/decode.c` (failed hwaccel setup retries
//!   `get_format()` without it). An explicit `-hwaccel vaapi` would NOT be safe: device
//!   *creation* failure is fatal there.
//! * **Aligned start**: the soundtrack (and the presentation clock) start only once the
//!   video prebuffer has filled, so a slow decode startup can't leave the picture
//!   permanently trailing the realtime audio.
//! * **Catch-up jump**: when decode falls hopelessly behind the clock (buffer dry and
//!   more than [`CATCHUP_GAP_SECS`] late), [`Playback::wants_catchup`] tells the UI to
//!   restart the stream at the clock — the "skip forward and resync" every player does
//!   on a machine that can't decode realtime, so A/V meet again instead of drifting.
//!
//! [`decode_frame_at`] is the single-frame scrub/step primitive the editor builds on.
//! Pause/resume is kill + respawn-with-`-ss`.

use super::VideoMeta;
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Cap the playback frame height for preview smoothness (the full-res file still saves).
const MAX_PREVIEW_H: u32 = 720;
/// PulseAudio playback buffer REQUESTED for the soundtrack (ms, via `-buffer_duration`), the
/// picture delay of the BOOTSTRAP clock, and the calibration FALLBACK. Small enough to keep
/// A/V sync tight, large enough to play a decoded file without underruns. Until the soundtrack
/// reports progress the video clock is delayed by this amount (the requested startup latency)
/// so the picture lines up with the buffered sound. But the request is only a HINT — measured
/// on pipewire-pulse, `-buffer_duration 200` yields an EFFECTIVE buffer of roughly double — so
/// the anchor lands almost immediately (the sink reports its true position) so this is only
/// the poster-hold before the first PCM plays. macOS and Linux own the output and read that
/// true position; Windows rides this bootstrap for the whole session (its ffplay sidecar
/// reports no position), which is why it never had the anchor step or the hitch.
const AUDIO_LATENCY_MS: u64 = 200;
/// Decode at most this many frames ahead of the playhead (the smoothing buffer).
const BUFFER_FRAMES: usize = 16;
/// Runway (seconds of video) to buffer before the clock + soundtrack start — covers
/// decode-startup latency and absorbs jitter. Converted to frames per the file's fps.
const PREBUFFER_SECS: f32 = 0.15;
/// The presented picture falling this far behind the clock — with the buffer dry and
/// stream left to decode — means the machine can't decode this file at realtime; the
/// UI then restarts the stream at the clock ([`Playback::wants_catchup`]).
const CATCHUP_GAP_SECS: f32 = 1.0;
/// Minimum running time before a stream declares itself hopeless — spaces catch-up
/// jumps out (each restart pays decode startup again) and lets a fresh stream settle.
const CATCHUP_COOLDOWN_SECS: f32 = 3.0;

/// How fast the presentation clock may run away from realtime while unwinding a one-time
/// anchor correction (a fraction of realtime): the clock advances at 1±this. 0.25 unwinds
/// the largest correction seen (a -369ms macOS anchor step) in ~1.5s, imperceptibly, where
/// SNAPPING it froze the picture for the whole 369ms. Must stay < 1.0 so the clock is always
/// forward: `slew_clock` returns >= its input, so a frame once shown is never re-shown.
const MAX_SLEW_RATE: f32 = 0.25;

/// Advance the presentation clock ONE poll toward `raw_target` instead of snapping to it.
/// `pres` is last poll's position, `raw_target` where the (bootstrap or audio-anchored)
/// clock now says the picture belongs, `dt` wall seconds since the last poll. The result is
/// always `>= pres` (never rewinds, never freezes) and closes the error at up to
/// [`MAX_SLEW_RATE`] beyond realtime, so the one-time step when the audio anchor first lands
/// (measured up to -369ms on macOS) unwinds smoothly over ~1.5s rather than in a single poll
/// that froze or skipped the picture. Pure, unit-tested.
fn slew_clock(pres: f32, raw_target: f32, dt: f32) -> f32 {
    let dt = dt.max(0.0);
    let advanced = pres + dt; // realtime baseline: the clock always moves forward
    let err = raw_target - advanced;
    let corr = err.clamp(-MAX_SLEW_RATE * dt, MAX_SLEW_RATE * dt);
    advanced + corr
}

/// Frames to have buffered before the clock + audio start: [`PREBUFFER_SECS`] of video,
/// floored for low-fps files, capped under [`BUFFER_FRAMES`] so it always fills.
fn prebuffer_frames(fps: f32) -> usize {
    ((fps.max(1.0) * PREBUFFER_SECS).ceil() as usize).clamp(4, BUFFER_FRAMES - 2)
}

/// A decoded preview frame: raw RGBA at `w`x`h`, tagged with its position in seconds.
pub struct Frame {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub pos: f32,
}

/// Shared presentation-clock state, behind ONE mutex — both fields are tiny and co-locating
/// them keeps `Playback` small (a second `Arc` here tips `PreviewKind`'s enum-size lint).
#[derive(Default)]
struct Clock {
    /// The presentation epoch, set by the READER the moment the prebuffer fills and the
    /// soundtrack spawns — so audio and the video clock start together no matter how slow the
    /// decode startup was. `None` while the prebuffer is still filling.
    epoch: Option<Instant>,
    /// Latest audio-clock anchor: `(wall instant it was taken, audible SOURCE position at that
    /// instant)`. On Linux the progress reader calibrates it from ffmpeg's `-progress`; on
    /// macOS the audio pump publishes the AudioQueue's TRUE playout position (DRAGON
    /// preview-hitch). Once present, [`Playback::poll`] extrapolates the picture target as
    /// `pos + instant.elapsed()`, locking the picture to the audio clock instead of the
    /// bootstrap epoch. `None` until it lands, and forever for a no-audio file (bootstrap
    /// clock) or the Windows ffplay path (no progress stream).
    anchor: Option<(Instant, f32)>,
    /// The SLEWED presentation clock (source seconds): what [`Playback::poll`] selects frames
    /// against. `None` until the first poll seeds it. Converges to the anchor at up to
    /// [`MAX_SLEW_RATE`] via [`slew_clock`], so a one-time anchor correction never freezes or
    /// jumps the picture. Lives here (behind the clock mutex) rather than on `Playback` so it
    /// does not grow `PreviewKind`'s size past the enum-variant lint.
    pres: Option<f32>,
    /// Wall instant of the previous poll, for the slew's `dt`. `None` before the first poll.
    last_poll: Option<Instant>,
    /// Observability: set once poll has logged the raw anchor step the slew is absorbing.
    anchor_step_logged: bool,
}

/// A running playback worker. A reader thread fills `buffer` ahead of the playhead; the
/// UI [`poll`]s for the frame due now. [`stop`](Self::stop) (or drop) kills ffmpeg.
pub struct Playback {
    /// Frames decoded ahead of the playhead, oldest first.
    buffer: Arc<Mutex<VecDeque<Frame>>>,
    /// Set to ask the reader to stop; it then kills ffmpeg and exits.
    stop: Arc<AtomicBool>,
    /// Set by the reader once ffmpeg reaches end-of-stream (no more frames will arrive).
    eof: Arc<AtomicBool>,
    /// Set once the audio playback has finished (or there was none). On a natural end the
    /// reader lets PulseAudio drain the queued tail before setting this, so `finished()`
    /// (and thus the UI's auto-stop) waits for the whole soundtrack instead of cutting it.
    audio_done: Arc<AtomicBool>,
    /// Where this stream started, in source seconds.
    start_sec: f32,
    /// How far to delay the picture (seconds) so it lines up with the buffered audio — the
    /// audio's startup latency when the file has sound, else 0.
    audio_latency: f32,
    /// Shared presentation clock: the epoch (set when the soundtrack starts) and the latest
    /// audio-progress anchor (see [`Clock`]). `poll` reads both; the reader/progress threads
    /// set them.
    clock: Arc<Mutex<Clock>>,
    /// Position of the newest frame presented (source seconds) — the catch-up detector's
    /// notion of "where the picture is".
    last_pos: f32,
    /// Set by [`poll`](Self::poll) when the buffer ran dry with the picture far behind
    /// the clock while the stream still has frames — decode can't keep realtime.
    starved_behind: bool,
    /// The clock's current position (source seconds) as of the last poll — where a
    /// catch-up restart should resume.
    catchup_pos: f32,
}

impl Playback {
    /// Probe `path` for dimensions/fps/duration/audio (returns `None` if ffprobe fails or
    /// the file has no readable video stream).
    pub fn probe(path: &Path) -> Option<VideoMeta> {
        let out = crate::util::ffprobe_command()
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,width,height,r_frame_rate",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(path)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (mut w, mut h, mut fps, mut duration, mut has_audio) = (0u32, 0u32, 0.0f32, 0.0f32, false);
        for line in text.lines() {
            let Some((key, val)) = line.split_once('=') else { continue };
            match key.trim() {
                "width" => w = val.trim().parse().unwrap_or(w),
                "height" => h = val.trim().parse().unwrap_or(h),
                "r_frame_rate" => {
                    let r = parse_ratio(val.trim());
                    if r > 0.0 {
                        fps = r;
                    }
                }
                "duration" => duration = val.trim().parse().unwrap_or(duration),
                "codec_type" if val.trim() == "audio" => has_audio = true,
                _ => {}
            }
        }
        if w == 0 || h == 0 {
            return None;
        }
        if !(1.0..=240.0).contains(&fps) {
            fps = 30.0;
        }
        Some(VideoMeta { duration, fps, w, h, has_audio })
    }

    /// Start streaming from `start_sec`: spawns the video ffmpeg + the buffering reader.
    /// The soundtrack ffmpeg (PulseAudio) spawns from the READER once the prebuffer
    /// fills, together with the presentation epoch — so audio and picture start aligned
    /// even when the decode takes a while to produce its first frames (large files).
    pub fn start(path: PathBuf, meta: VideoMeta, start_sec: f32) -> Self {
        let out = scaled_dims(meta.w, meta.h);
        let buffer = Arc::new(Mutex::new(VecDeque::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let eof = Arc::new(AtomicBool::new(false));
        // No audio stream → nothing to wait for, so it's "done" from the start.
        let audio_done = Arc::new(AtomicBool::new(!meta.has_audio));
        // Shared presentation clock (epoch + audio-progress anchor). The reader sets the epoch
        // when the soundtrack spawns; the progress reader fills the anchor once ffmpeg reports
        // its position — until then `poll` rides the bootstrap epoch/`audio_latency` clock.
        let clock = Arc::new(Mutex::new(Clock::default()));
        let pb = Self {
            buffer: buffer.clone(),
            stop: stop.clone(),
            eof: eof.clone(),
            audio_done: audio_done.clone(),
            start_sec,
            audio_latency: if meta.has_audio { AUDIO_LATENCY_MS as f32 / 1000.0 } else { 0.0 },
            clock: clock.clone(),
            last_pos: start_sec,
            starved_behind: false,
            catchup_pos: start_sec,
        };
        let frame_bytes = (out.0 as usize) * (out.1 as usize) * 4;
        let prebuffer = prebuffer_frames(meta.fps);
        std::thread::spawn(move || {
            // Soundtrack: ffmpeg straight to PulseAudio, spawned once the prebuffer fills
            // (below). ffmpeg's `-f pulse` muxer doesn't drain its buffer on end-of-stream,
            // so it would clip the last `buffer_duration`; the `apad` trailing silence (a
            // bit longer than the buffer) means that discarded tail is silence, not the
            // recording's real audio. `-buffer_duration` keeps the latency small and known,
            // and the video clock is delayed to match it (see `poll`).
            let spawn_audio = || {
                if !meta.has_audio {
                    return None;
                }
                // Linux/macOS: ONE ffmpeg renders the soundtrack straight to the OS sink.
                // `-progress pipe:1 -stats_period ...`: ffmpeg reports its muxed-audio position
                // (`out_time_us`) to stdout every period; the progress reader below calibrates
                // the sink's effective buffer from those blocks, then turns them into the
                // picture's audio-clock anchor (see `poll`). stdout is PIPED for it — the real
                // audio output is the sink device, not stdout. macOS and Linux both OWN the
                // output now (`platform::mac::audio_out` / `audio::pulse_out`) and read the
                // TRUE playout position, which is the whole DRAGON preview-hitch fix; only
                // Windows still rides the bootstrap clock (its ffplay sidecar reports no
                // position, so it never had an anchor to step, and never had the hitch).
                //
                // Both arms are the SAME shape: decode the soundtrack to raw interleaved f32
                // PCM, then play it through an owned sink on a pump thread that publishes the
                // sink's true position as the clock anchor. Only the sink type differs. No
                // `apad`: the pump drains the sink fully, so there is no clipped tail to pad.
                // ALL THREE PLATFORMS, one shape: decode the soundtrack to raw f32 PCM (no
                // ffmpeg output device involved) and play it through the platform's owned sink
                // on a pump thread that publishes the sink's TRUE position as the clock anchor.
                // Only the sink type differs. A sink that fails to open degrades to
                // silent-but-playing (the drain path), never a crash.
                {
                    let mut acmd = crate::util::ffmpeg_command();
                    let mut child = acmd
                        .args(["-v", "error", "-ss", &format!("{start_sec:.3}"), "-i"])
                        .arg(&path)
                        .args(["-vn", "-ac", "2", "-ar", "48000", "-f", "f32le", "pipe:1"])
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .spawn()
                        .ok()?;
                    if let Some(pcm) = child.stdout.take() {
                        let clock = clock.clone();
                        let stop = stop.clone();
                        let audio_done = audio_done.clone();
                        std::thread::spawn(move || {
                            #[cfg(target_os = "macos")]
                            let sink = crate::platform::mac::audio_out::AudioQueueSink::open(48_000.0, 2);
                            #[cfg(target_os = "linux")]
                            let sink = crate::audio::pulse_out::PulseSink::open(48_000, 2);
                            #[cfg(windows)]
                            let sink = crate::platform::windows::audio_out::WasapiRenderSink::open(48_000, 2);
                            match sink {
                                Some(sink) => audio_pump(pcm, sink, start_sec, clock, &stop, &audio_done),
                                None => drain_pcm_silently(pcm, &stop, &audio_done),
                            }
                        });
                    }
                    Some(child)
                }
            };
            let mut vcmd = crate::util::ffmpeg_command();
            // `-hwaccel auto` — the difference between stalling and realtime on 4K,
            // and safe on every box (software fallback by design; module doc).
            vcmd.args(["-v", "error", "-hwaccel", "auto"]);
            // `flags=bilinear`: a preview-sized downscale on the hot path — much
            // cheaper than the default bicubic and visually fine at ≤720p (screen
            // text stays cleaner than fast_bilinear); the bake/save never touch it.
            let mut video = match vcmd
                .args(["-ss", &format!("{start_sec:.3}"), "-i"])
                .arg(&path)
                .args(["-an", "-f", "rawvideo", "-pix_fmt", "rgba", "-vf"])
                .arg(format!("scale={}:{}:flags=bilinear", out.0, out.1))
                .arg("pipe:1")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => {
                    eof.store(true, Ordering::Relaxed);
                    audio_done.store(true, Ordering::Relaxed);
                    return;
                }
            };
            let Some(mut stdout) = video.stdout.take() else {
                eof.store(true, Ordering::Relaxed);
                audio_done.store(true, Ordering::Relaxed);
                let _ = video.kill();
                return;
            };
            let mut audio: Option<std::process::Child> = None;
            let mut started = false;
            let mut n: u64 = 0;
            'read: while !stop.load(Ordering::Relaxed) {
                // Backpressure: don't decode more than BUFFER_FRAMES ahead.
                while buffer.lock().map(|b| b.len()).unwrap_or(0) >= BUFFER_FRAMES {
                    if stop.load(Ordering::Relaxed) {
                        break 'read;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                // Fresh buffer per frame, moved into the queue — no per-frame copy.
                let mut rgba = vec![0u8; frame_bytes];
                if stdout.read_exact(&mut rgba).is_err() {
                    break; // EOF / short read → end of stream
                }
                let pos = start_sec + (n as f32) / meta.fps;
                let buffered = match buffer.lock() {
                    Ok(mut b) => {
                        b.push_back(Frame { rgba, w: out.0, h: out.1, pos });
                        b.len()
                    }
                    Err(_) => 0,
                };
                n += 1;
                // Prebuffer filled → start the soundtrack and the presentation clock
                // TOGETHER, however long the decode took to get here.
                if !started && buffered >= prebuffer {
                    audio = spawn_audio();
                    if let Ok(mut g) = clock.lock() {
                        g.epoch = Some(Instant::now());
                    }
                    started = true;
                }
            }
            eof.store(true, Ordering::Relaxed);
            // End-of-stream before the prebuffer filled (a clip shorter than the
            // runway): start the clock + sound now. Not on an explicit stop.
            if !started && !stop.load(Ordering::Relaxed) {
                audio = spawn_audio();
                if let Ok(mut g) = clock.lock() {
                    g.epoch = Some(Instant::now());
                }
            }
            let _ = video.kill();
            let _ = video.wait();
            if let Some(mut a) = audio.take() {
                // On a natural end let ffmpeg run to completion: it plays the real audio plus
                // the trailing silence pad, so by the time it exits (discarding `buffer_duration`
                // of that silence) every real sample has reached the speakers. On an explicit
                // stop (pause / close / scrub) cut it immediately. Poll so a stop during the
                // wind-down still ends it promptly.
                loop {
                    if stop.load(Ordering::Relaxed) {
                        let _ = a.kill();
                        break;
                    }
                    match a.try_wait() {
                        Ok(Some(_)) => break, // played out + exited on its own
                        Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                        Err(_) => break,
                    }
                }
                let _ = a.wait();
            }
            audio_done.store(true, Ordering::Relaxed);
        });
        pb
    }

    /// The frame that should be on screen now, paced to the shared audio/video epoch —
    /// dropping frames we're late for and holding when ahead. Returns `None` (keep the
    /// current frame) before the prebuffer fills or when nothing is due.
    pub fn poll(&mut self) -> Option<Frame> {
        // Compute the presentation target under ONE clock lock. No epoch yet → the reader is
        // still prebuffering; keep the current frame. The slew state (`pres`, `last_poll`) and
        // the one-shot diagnostic flag live in the clock too, so `Playback` stays small enough
        // not to trip `PreviewKind`'s enum-variant size lint.
        let now = Instant::now();
        let (target, step_log) = {
            let mut g = self.clock.lock().ok()?;
            let t0 = g.epoch?;
            let anchor = g.anchor;
            // Bootstrap/fallback clock: epoch + elapsed − the REQUESTED buffer latency. It holds
            // the poster until the buffer fills, and is the whole story for a no-audio file or
            // the Windows ffplay path (no anchor ever), and for the moments before the anchor
            // lands.
            let epoch_target = self.start_sec + t0.elapsed().as_secs_f32() - self.audio_latency;
            // Once an anchor is present, lock the picture to the audio clock: extrapolate from
            // the anchor (audible position + wall time since). Sanity-gate it against garbage (a
            // bogus position / a seek): only trust it within a window around the bootstrap clock,
            // never > 1s behind or > 2s ahead.
            let raw_target = match anchor {
                Some((at, pos)) => {
                    let anchored = pos + at.elapsed().as_secs_f32();
                    if anchored < epoch_target - 1.0 || anchored > epoch_target + 2.0 {
                        epoch_target
                    } else {
                        anchored
                    }
                }
                None => epoch_target,
            };
            // Slew toward the raw target instead of snapping. First poll seeds `pres`; after
            // that it advances by realtime and closes any error at up to MAX_SLEW_RATE, so the
            // one-time anchor step (up to −369ms measured on the old macOS estimate) is spread
            // smoothly rather than freezing (backward step) or skipping (forward step) the
            // picture. With the macOS true-clock anchor there is little to correct, so the slew
            // stays a harmless safety net.
            let dt = g.last_poll.map(|p| now.saturating_duration_since(p).as_secs_f32());
            g.last_poll = Some(now);
            let target = match (g.pres, dt) {
                (Some(prev), Some(dt)) => slew_clock(prev, raw_target, dt),
                _ => raw_target,
            };
            g.pres = Some(target);
            // Observability (DRAGON preview-hitch): capture the FIRST anchored poll's RAW step
            // so the debug log names how far the audio clock disagreed with the bootstrap. Log
            // it AFTER releasing the lock.
            let step_log = if !g.anchor_step_logged && anchor.is_some() {
                g.anchor_step_logged = true;
                Some((raw_target - epoch_target) * 1000.0)
            } else {
                None
            };
            (target, step_log)
        };
        if let Some(step_ms) = step_log {
            log::info!(
                "preview playback: audio anchor landed, raw clock disagreed {step_ms:+.0}ms \
                 with the {:.0}ms bootstrap; slewing it in over ~{:.1}s at {:.0}% instead of \
                 snapping (a snap of this size was the visible hitch)",
                self.audio_latency * 1000.0,
                (step_ms.abs() / 1000.0) / MAX_SLEW_RATE,
                MAX_SLEW_RATE * 100.0,
            );
        }
        let mut buf = self.buffer.lock().ok()?;
        let mut chosen = None;
        while buf.front().is_some_and(|f| f.pos <= target) {
            chosen = buf.pop_front();
        }
        if let Some(f) = &chosen {
            self.last_pos = f.pos;
        }
        // Starved AND far behind with stream left = decode can't keep realtime; the
        // UI asks `wants_catchup` after each poll and jumps the stream forward.
        self.starved_behind = buf.is_empty()
            && !self.eof.load(Ordering::Relaxed)
            && target - self.last_pos > CATCHUP_GAP_SECS;
        self.catchup_pos = target;
        chosen
    }

    /// After a [`poll`](Self::poll): the position to RESTART the stream at when decode
    /// has fallen hopelessly behind the clock — the "jump forward and resync" real
    /// players do on machines that can't decode a file at realtime, so the picture
    /// meets the (realtime) audio again instead of drifting ever further apart.
    /// `None` while keeping up, or within a fresh stream's settling cooldown.
    pub fn wants_catchup(&self) -> Option<f32> {
        if !self.starved_behind {
            return None;
        }
        let t0 = self.clock.lock().ok()?.epoch?;
        (t0.elapsed().as_secs_f32() > CATCHUP_COOLDOWN_SECS).then_some(self.catchup_pos.max(0.0))
    }

    /// Stop streaming (kills ffmpeg via the flag; the reader joins itself).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Whether the stream ended, every buffered frame has been presented, and the audio has
    /// finished playing out. Waiting on the audio keeps the UI from tearing the player down
    /// (which would kill ffmpeg) while PulseAudio still has the soundtrack's tail to play.
    pub fn finished(&self) -> bool {
        self.eof.load(Ordering::Relaxed)
            && self.audio_done.load(Ordering::Relaxed)
            && self.buffer.lock().map(|b| b.is_empty()).unwrap_or(true)
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Decode a single (scaled) frame at `t` seconds — the scrub/step primitive. `accurate`
/// trades speed for exactness: a fast keyframe seek (snappy for scrubbing) vs a fast
/// seek to just before `t` then an accurate decode to the exact frame (for frame steps).
pub fn decode_frame_at(path: &Path, meta: VideoMeta, t: f32, accurate: bool) -> Option<Frame> {
    let (w, h) = scaled_dims(meta.w, meta.h);
    let t = t.clamp(0.0, meta.duration.max(0.0));
    let mut cmd = crate::util::ffmpeg_command();
    // Same safe hardware decode as playback — scrubbing a 4K file decodes a
    // keyframe run per step, so this matters just as much here.
    cmd.args(["-v", "error", "-hwaccel", "auto"]);
    if accurate {
        let fast = (t - 0.5).max(0.0);
        cmd.args(["-ss", &format!("{fast:.3}")])
            .arg("-i")
            .arg(path)
            .args(["-ss", &format!("{:.3}", t - fast)]);
    } else {
        cmd.args(["-ss", &format!("{t:.3}")]).arg("-i").arg(path);
    }
    let out = cmd
        .args(["-frames:v", "1", "-an", "-f", "rawvideo", "-pix_fmt", "rgba", "-vf"])
        .arg(format!("scale={w}:{h}:flags=bilinear"))
        .arg("pipe:1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.len() < (w as usize) * (h as usize) * 4 {
        return None;
    }
    Some(Frame { rgba: out.stdout, w, h, pos: t })
}

/// No output device: drain ffmpeg's PCM so it is not blocked on a full pipe, then report the
/// audio finished. The silent-but-playing degradation for a machine with no usable audio sink.
fn drain_pcm_silently(mut pcm: ChildStdout, stop: &AtomicBool, audio_done: &AtomicBool) {
    use std::io::Read;
    use std::sync::atomic::Ordering;
    let mut scratch = [0u8; 16384];
    while !stop.load(Ordering::Relaxed) {
        match pcm.read(&mut scratch) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    audio_done.store(true, Ordering::Relaxed);
}

/// The small seam the owned-sink audio pump drives, so ONE pump serves the macOS AudioQueue
/// sink, the Linux PulseAudio sink and the Windows WASAPI sink (DRAGON preview-hitch). Each
/// platform's sink implements it; the effectful native code stays in its own module.
trait PreviewAudioSink {
    /// Play interleaved f32 samples, blocking as backpressure so the pump paces to realtime.
    fn write(&mut self, interleaved: &[f32], stop: &AtomicBool);
    /// Seconds the device has actually PLAYED (the true audible position), `None` before start.
    fn played_secs(&self) -> Option<f64>;
    /// True once the sink has played out everything written.
    fn drained(&self) -> bool;
}

#[cfg(target_os = "macos")]
impl PreviewAudioSink for crate::platform::mac::audio_out::AudioQueueSink {
    fn write(&mut self, s: &[f32], stop: &AtomicBool) {
        crate::platform::mac::audio_out::AudioQueueSink::write(self, s, stop)
    }
    fn played_secs(&self) -> Option<f64> {
        crate::platform::mac::audio_out::AudioQueueSink::played_secs(self)
    }
    fn drained(&self) -> bool {
        crate::platform::mac::audio_out::AudioQueueSink::drained(self)
    }
}

#[cfg(target_os = "linux")]
impl PreviewAudioSink for crate::audio::pulse_out::PulseSink {
    fn write(&mut self, s: &[f32], stop: &AtomicBool) {
        crate::audio::pulse_out::PulseSink::write(self, s, stop)
    }
    fn played_secs(&self) -> Option<f64> {
        crate::audio::pulse_out::PulseSink::played_secs(self)
    }
    fn drained(&self) -> bool {
        crate::audio::pulse_out::PulseSink::drained(self)
    }
}

#[cfg(windows)]
impl PreviewAudioSink for crate::platform::windows::audio_out::WasapiRenderSink {
    fn write(&mut self, s: &[f32], stop: &AtomicBool) {
        crate::platform::windows::audio_out::WasapiRenderSink::write(self, s, stop)
    }
    fn played_secs(&self) -> Option<f64> {
        crate::platform::windows::audio_out::WasapiRenderSink::played_secs(self)
    }
    fn drained(&self) -> bool {
        crate::platform::windows::audio_out::WasapiRenderSink::drained(self)
    }
}

/// The owned-sink preview audio pump (DRAGON preview-hitch): read raw f32 PCM from ffmpeg, play
/// it through the owned `sink`, and publish the sink's TRUE playout position as the clock
/// anchor after every write. Runs on its own thread for one playback; the sink is created and
/// dropped on that thread so it never crosses threads. Shared by all three platforms; only the
/// sink type differs.
fn audio_pump<S: PreviewAudioSink>(
    mut pcm: ChildStdout,
    mut sink: S,
    start_sec: f32,
    clock: Arc<Mutex<Clock>>,
    stop: &AtomicBool,
    audio_done: &AtomicBool,
) {
    use std::io::Read;
    use std::sync::atomic::Ordering;
    let publish = |secs: f64| {
        if let Ok(mut g) = clock.lock() {
            g.anchor = Some((Instant::now(), start_sec + secs as f32));
        }
    };
    let mut bytes = [0u8; 16384];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let n = match pcm.read(&mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        // Assemble whole f32 samples, carrying any 1-3 trailing bytes into the next read.
        carry.extend_from_slice(&bytes[..n]);
        let whole = carry.len() - (carry.len() % 4);
        if whole == 0 {
            continue;
        }
        let mut samples = Vec::with_capacity(whole / 4);
        for c in carry[..whole].chunks_exact(4) {
            samples.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
        }
        carry.drain(..whole);
        sink.write(&samples, stop);
        if let Some(secs) = sink.played_secs() {
            publish(secs);
        }
    }
    // Drain: the device still holds a few enqueued buffers. Let them play out (bounded) so the
    // video tail stays in step, keeping the anchor current, then report done.
    let deadline = Instant::now() + Duration::from_secs(3);
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        match sink.played_secs() {
            Some(secs) => {
                publish(secs);
                if sink.drained() {
                    break;
                }
            }
            None => break,
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    audio_done.store(true, Ordering::Relaxed);
}

/// Parse an ffmpeg rational like `30000/1001` or `30/1` (or a bare number) to fps.
fn parse_ratio(s: &str) -> f32 {
    if let Some((num, den)) = s.split_once('/') {
        let (n, d) = (num.parse::<f32>().unwrap_or(0.0), den.parse::<f32>().unwrap_or(0.0));
        if d != 0.0 {
            return n / d;
        }
    }
    s.parse().unwrap_or(0.0)
}

/// Preview output size: scale to [`MAX_PREVIEW_H`] (even dims), never upscaling. Shared
/// with poster extraction so the poster and the playing frames are the same scale.
pub(super) fn scaled_dims(w: u32, h: u32) -> (u32, u32) {
    if h <= MAX_PREVIEW_H {
        return (w & !1, h & !1);
    }
    let sw = ((w as f32) * (MAX_PREVIEW_H as f32) / (h as f32)).round() as u32;
    (sw & !1, MAX_PREVIEW_H & !1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slew_clock_never_rewinds_or_freezes() {
        // A large BACKWARD correction (the -369ms macOS anchor step): the clock must still
        // move FORWARD, just slower than realtime, so the picture never freezes.
        let dt = 0.033; // one 30fps poll
        let out = slew_clock(1.000, 1.000 - 0.369, dt);
        assert!(out > 1.000, "clock went backward/froze: {out}");
        assert!(out < 1.000 + dt, "a backward correction must run slower than realtime");
    }

    #[test]
    fn slew_clock_converges_and_then_tracks_exactly() {
        // Once the error is within one poll's slew budget, the clock lands ON the target.
        let dt = 0.033;
        let tiny = MAX_SLEW_RATE * dt * 0.5; // half a poll's worth of correction
        let out = slew_clock(5.0, 5.0 + dt + tiny, dt);
        assert!((out - (5.0 + dt + tiny)).abs() < 1e-6, "should reach the target: {out}");
    }

    #[test]
    fn slew_clock_forward_step_runs_at_most_one_plus_rate() {
        // A forward correction is bounded to (1 + MAX_SLEW_RATE) x realtime — no jump.
        let dt = 0.033;
        let out = slew_clock(2.0, 2.0 + 1.0, dt); // 1s ahead: huge error
        let advanced = out - 2.0;
        assert!(advanced <= dt * (1.0 + MAX_SLEW_RATE) + 1e-6, "overshot the slew cap: {advanced}");
        assert!(advanced >= dt * (1.0 + MAX_SLEW_RATE) - 1e-6, "should saturate the cap: {advanced}");
    }

    #[test]
    fn scaled_dims_leaves_short_video_untouched_but_forces_even() {
        // Already ≤ MAX_PREVIEW_H: no scaling, only the even-dims rounding applies.
        assert_eq!(scaled_dims(640, 480), (640, 480));
        assert_eq!(scaled_dims(641, 481), (640, 480));
    }

    #[test]
    fn scaled_dims_is_unchanged_at_the_max_height_boundary() {
        assert_eq!(scaled_dims(1280, 720), (1280, 720));
    }

    #[test]
    fn scaled_dims_downscales_tall_video_preserving_aspect() {
        // 1920x1080 -> height clamped to 720, width scaled to match (1920*720/1080 = 1280).
        assert_eq!(scaled_dims(1920, 1080), (1280, 720));
    }

    #[test]
    fn scaled_dims_rounds_the_scaled_width_down_to_even() {
        // Portrait: 1080x1920 -> scaled width 1080*720/1920 = 405 (odd) -> 404.
        assert_eq!(scaled_dims(1080, 1920), (404, 720));
    }

    #[test]
    fn prebuffer_scales_with_fps_within_the_buffer() {
        // ~PREBUFFER_SECS of runway: more frames at higher fps…
        assert_eq!(prebuffer_frames(60.0), 9);
        assert_eq!(prebuffer_frames(30.0), 5);
        // …floored for low-fps files (a 4-frame minimum cushion)…
        assert_eq!(prebuffer_frames(10.0), 4);
        // …and always leaving room in the bounded buffer so it can actually fill.
        assert!(prebuffer_frames(240.0) <= BUFFER_FRAMES - 2);
    }

    #[test]
    fn parse_ratio_handles_ntsc_style_fraction() {
        let fps = parse_ratio("30000/1001");
        assert!((fps - 29.970_03).abs() < 0.001, "fps = {fps}");
    }

    #[test]
    fn parse_ratio_handles_whole_number_fraction() {
        assert_eq!(parse_ratio("30/1"), 30.0);
    }

    #[test]
    fn parse_ratio_handles_a_bare_number() {
        assert_eq!(parse_ratio("25"), 25.0);
    }

    #[test]
    fn parse_ratio_zero_denominator_falls_back_to_zero() {
        assert_eq!(parse_ratio("30/0"), 0.0);
    }

    #[test]
    fn parse_ratio_garbage_falls_back_to_zero() {
        assert_eq!(parse_ratio(""), 0.0);
        assert_eq!(parse_ratio("not-a-number"), 0.0);
    }

    #[test]
    fn slew_clock_is_the_bridge_from_bootstrap_to_the_true_anchor() {
        // Sanity that the surviving clock path is the slew, not the deleted estimation: a
        // realtime advance with no error just moves forward by dt.
        let dt = 0.033;
        assert!((slew_clock(1.0, 1.0 + dt, dt) - (1.0 + dt)).abs() < 1e-6);
    }
}
