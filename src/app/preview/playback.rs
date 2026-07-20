//! Inline video playback for the preview overlay, driven by the `ffmpeg` binary (no
//! GStreamer, no `iced_video_player` — so no second copy of iced to keep version-matched
//! with libcosmic).
//!
//! A reader thread decodes scaled raw-RGBA frames from one `ffmpeg` process *ahead* of
//! playback into a bounded buffer (backpressured when full). The UI [`Playback::poll`]s on
//! a timer and presents the frame due at the current wall-clock time — dropping late
//! frames and holding when ahead — so motion is smooth regardless of decode/timer jitter
//! (a single latest-frame slot dropped/repeated frames and flickered). When the file has
//! audio, a second `ffmpeg` plays it straight to PulseAudio (the same `-f pulse` path the
//! recorder uses) — on macOS to the default output device via `-f audiotoolbox` instead —
//! on its own real-time clock; its `-progress` stream first self-calibrates
//! the sink's EFFECTIVE buffer (servers treat `-buffer_duration` as a hint — pipewire-pulse
//! grants roughly double; audiotoolbox has no buffer knob at all), then anchors the picture
//! to the audio's own reported position
//! minus that measured buffer — A/V locked on any Pulse server (and CoreAudio), no assumed
//! latency (see [`AUDIO_LATENCY_MS`] / [`calibrate_effective_buffer`]). On Windows the bundled
//! ffmpeg has no pulse muxer and no audio-output device at all, so the soundtrack is an
//! `ffplay` sidecar instead (SDL2 → the default output endpoint); ffplay emits no `-progress`,
//! so the Windows picture rides the bootstrap epoch clock exactly like a no-audio file
//! (DRAGON-285) — the `spawn_audio` closure's per-platform arms cover all three.
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
use std::io::{BufRead, BufReader, Read};
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
/// the progress reader measures what the sink actually buffers (the burst-to-paced knee,
/// [`calibrate_effective_buffer`]) and [`Playback::poll`] then anchors the picture to the
/// audio's OWN reported position (audible ≈ muxed − measured buffer), which tracks the real
/// per-server latency and any sink-clock drift for free. See [`spawn_progress_reader`].
const AUDIO_LATENCY_MS: u64 = 200;
/// Trailing silence padded onto the soundtrack (ms). On end-of-stream ffmpeg writes the last
/// samples into PulseAudio's buffer and exits WITHOUT draining it, so exactly `AUDIO_LATENCY_MS`
/// of audio is discarded. Padding a bit MORE than that makes the discarded tail silence, never
/// the recording's real audio. It must exceed `AUDIO_LATENCY_MS`; the small excess only delays
/// when the player reports "finished" (the last frame holds that much longer), so keep it tight.
// Windows renders the soundtrack via an ffplay sidecar with no `-progress` stream, so the
// whole progress-calibration path below (this pad, the stats/pacing constants, and the
// reader/calibrator fns) is Linux/macOS-only — honestly gated dead on Windows (DRAGON-285).
#[cfg_attr(windows, allow(dead_code))]
const AUDIO_PAD_MS: u64 = AUDIO_LATENCY_MS + 60;
/// `-stats_period` of the soundtrack's `-progress` stream (seconds): how often ffmpeg reports
/// its muxed position. Doubles as calibration's mux-start estimate — ffmpeg prints the first
/// block one period after muxing starts.
#[cfg_attr(windows, allow(dead_code))]
const PROGRESS_STATS_PERIOD: f32 = 0.1;
/// A progress interval advancing `out_time` slower than this multiple of wall time is "paced"
/// (the sink buffer is full; writes throttle to playout). The startup burst runs at several ×
/// realtime (audio-only decode is cheap), so the margin is generous in both directions.
#[cfg_attr(windows, allow(dead_code))]
const PACED_RATIO_MAX: f32 = 1.3;
/// Give up calibrating this long after the first progress block (seconds) and settle on the
/// requested [`AUDIO_LATENCY_MS`] instead — exactly the old fixed-assumption behavior.
#[cfg_attr(windows, allow(dead_code))]
const CALIBRATION_DEADLINE_SECS: f32 = 2.5;
/// Plausibility floor for the calibrated effective buffer (seconds).
#[cfg_attr(windows, allow(dead_code))]
const EFFECTIVE_BUFFER_MIN: f32 = 0.05;
/// Plausibility ceiling for the calibrated effective buffer (seconds).
#[cfg_attr(windows, allow(dead_code))]
const EFFECTIVE_BUFFER_MAX: f32 = 1.0;
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
    /// Latest audio-clock anchor parsed from the soundtrack ffmpeg's `-progress` stream:
    /// `(wall instant the progress block arrived, audible SOURCE position at that instant)`.
    /// Once present, [`Playback::poll`] extrapolates the picture target as `pos +
    /// instant.elapsed()`, locking the picture to the real audio clock instead of the
    /// bootstrap epoch model. `None` until the progress reader has calibrated the sink's
    /// effective buffer (a few blocks in) — and forever for a file with no audio, which
    /// rides the bootstrap clock exactly as before.
    anchor: Option<(Instant, f32)>,
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
                // audio output is the sink device (`-f pulse`/`-f audiotoolbox`), not stdout.
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                {
                    let mut acmd = crate::util::ffmpeg_command();
                    acmd.args(["-v", "error", "-progress", "pipe:1", "-stats_period"])
                        .arg(PROGRESS_STATS_PERIOD.to_string())
                        .args(["-ss", &format!("{start_sec:.3}"), "-i"])
                        .arg(&path)
                        .args(["-vn", "-af", &format!("apad=pad_dur={:.3}", AUDIO_PAD_MS as f32 / 1000.0)]);
                    // Audio sink: PulseAudio on Linux; ffmpeg's `audiotoolbox` output device
                    // on macOS (the trailing arg is the muxer's required-but-unused
                    // "filename" — playback goes to the default output device). audiotoolbox
                    // exposes NO buffer-size knob (`-h muxer=audiotoolbox`: only device
                    // selection), so the `-buffer_duration` REQUEST has no macOS equivalent —
                    // but the request was only ever a hint anyway: the progress reader's
                    // burst-to-paced calibration measures whatever CoreAudio actually buffers
                    // (~0.19s observed, conveniently near the AUDIO_LATENCY_MS
                    // bootstrap/fallback), so the A/V-sync model below is unchanged.
                    #[cfg(target_os = "linux")]
                    acmd.args(["-f", "pulse", "-buffer_duration", &AUDIO_LATENCY_MS.to_string()])
                        .arg("cosmic-capture-kit");
                    #[cfg(target_os = "macos")]
                    acmd.args(["-f", "audiotoolbox", "default"]);
                    let mut child = acmd
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .spawn()
                        .ok()?;
                    // Its own thread drains the progress pipe (so a full pipe can't block ffmpeg's
                    // audio writes) and keeps the clock's anchor current.
                    if let Some(progress) = child.stdout.take() {
                        spawn_progress_reader(progress, start_sec, clock.clone());
                    }
                    Some(child)
                }
                // Windows: the bundled ffmpeg has NO pulse muxer and no audio-output device at
                // all, so it can't be the soundtrack sink (it would exit code 8 instantly). An
                // `ffplay` sidecar renders to the default output endpoint via SDL2 instead.
                // ffplay emits no `-progress`, so we wire NO progress reader: `clock.anchor`
                // stays `None` and the picture rides the bootstrap epoch clock — the exact path
                // a no-audio file uses, which `poll` already tolerates. A missing ffplay
                // degrades to silent-but-playing with one warning, never a crash (DRAGON-285).
                #[cfg(windows)]
                {
                    let ffplay = crate::util::ffplay_path();
                    if !crate::util::tool_available(&ffplay) {
                        log::warn!(
                            "ffplay not found ({}) — preview video plays without sound; install \
                             ffplay or set CCK_FFPLAY to enable preview audio",
                            ffplay.display()
                        );
                        return None;
                    }
                    // -nodisp: no SDL window; -vn: audio only; -autoexit: quit at EOF (so the
                    // generic try_wait drain below reaps it naturally). Routed through
                    // quiet_command so the console child never flashes/leaves a pane (DRAGON-236).
                    crate::util::quiet_command(&ffplay)
                        .args(["-nodisp", "-autoexit", "-vn", "-loglevel", "error", "-ss"])
                        .arg(format!("{start_sec:.3}"))
                        .arg("-i")
                        .arg(&path)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .ok()
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
        // Read the shared clock once. No epoch yet → the reader is still prebuffering; keep the
        // current frame. `anchor` is the audio-progress anchor (used below), `None` until it lands.
        let (t0, anchor) = {
            let g = self.clock.lock().ok()?;
            (g.epoch?, g.anchor)
        };
        // Bootstrap/fallback clock: epoch + elapsed − the REQUESTED buffer latency. It holds
        // the poster until the buffer fills (target < start_sec early on), and is the whole
        // story for a no-audio file (no anchor ever) or until the soundtrack's progress
        // stream has calibrated the effective buffer.
        let epoch_target = self.start_sec + t0.elapsed().as_secs_f32() - self.audio_latency;
        // Once the progress reader has calibrated the sink's effective buffer and reports the
        // muxed position, lock the picture to the REAL audio clock: extrapolate from the last
        // anchor (audible position + wall time since). This picks up the true per-server
        // latency (the buffer request is only a hint) and any sink-clock drift for free.
        // Sanity-gate the anchor against garbage (a bogus out_time / a seek): only trust it
        // within a window around the bootstrap clock — never > 1s behind or > 2s ahead. When
        // the FIRST anchor lands (calibration settles a few hundred ms in), `target` may step
        // once by the requested-vs-effective buffer error (~200ms on pipewire-pulse); the
        // frame-drop/hold loop below self-corrects and we deliberately do NOT rewind (buffered
        // frames are already consumed) — the one-time step is intentional. After `progress=end`
        // the anchor stops updating but stays valid: `pos + elapsed` keeps advancing by wall
        // time, matching the apad silence tail draining out of the buffer.
        let target = match anchor {
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

/// Read the soundtrack ffmpeg's `-progress` stream and keep the shared clock's anchor current
/// with the latest audio-clock anchor `(now, audible source position)`. Its own thread (off the
/// frame path), mirroring the reader's style. First it calibrates the sink's EFFECTIVE buffer
/// from the opening blocks ([`calibrate_effective_buffer`]), publishing NO anchors until that
/// settles — the bootstrap epoch clock covers the opening moments — then every block becomes an
/// anchor. Drains stdout to EOF so a full progress pipe can never block ffmpeg's audio writes,
/// and stops UPDATING at `progress=end` — the padded tail then plays out under the last
/// anchor's wall-time extrapolation (see [`Playback::poll`]).
#[cfg_attr(windows, allow(dead_code))]
fn spawn_progress_reader(stdout: ChildStdout, start_sec: f32, clock: Arc<Mutex<Clock>>) {
    std::thread::spawn(move || {
        let mut cur_us: Option<i64> = None;
        let mut ended = false;
        // Calibration history — block arrivals (seconds since the first block) and muxed
        // positions (seconds) — only grown until the effective buffer settles.
        let mut first_block: Option<Instant> = None;
        let (mut wall_offsets, mut out_times) = (Vec::new(), Vec::new());
        let mut buffer_secs: Option<f32> = None;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break }; // pipe closed → ffmpeg gone, we're done
            if let Some(us) = parse_progress_out_time_us(&line) {
                cur_us = Some(us);
            } else if let Some(state) = line.trim().strip_prefix("progress=") {
                // Block terminator: this block's out_time is complete.
                if !ended && let Some(us) = cur_us.take() {
                    let now = Instant::now();
                    if buffer_secs.is_none() {
                        let t0 = *first_block.get_or_insert(now);
                        wall_offsets.push(now.duration_since(t0).as_secs_f32());
                        out_times.push(us as f32 / 1_000_000.0);
                        buffer_secs = calibrate_effective_buffer(&wall_offsets, &out_times);
                    }
                    // No anchor until the buffer estimate settles — the bootstrap clock
                    // (epoch − AUDIO_LATENCY_MS, see `poll`) covers the opening moments.
                    if let Some(buffer) = buffer_secs
                        && let Ok(mut g) = clock.lock()
                    {
                        g.anchor = Some((now, audible_pos_from_out_time(us, start_sec, buffer)));
                    }
                }
                // Keep draining after `end` to reach EOF; just stop updating the anchor.
                ended |= state == "end";
            }
        }
    });
}

/// Self-calibrate the sink's EFFECTIVE playback buffer (seconds) from the opening progress
/// blocks. `wall_offsets[i]` / `out_times[i]` are block `i`'s arrival (seconds since block 0)
/// and muxed position (seconds); pure over those observables so it's unit-testable.
///
/// The `-buffer_duration` request is only a hint (pipewire-pulse grants roughly double), so
/// the amount to subtract from `out_time` must be measured. At startup the audio-only decode
/// outruns playout, so `out_time` bursts ahead of wall time until the sink buffer fills; from
/// then on writes are paced by playout and the ratio drops to ≈1. The knee is the first block
/// whose interval ratio is below [`PACED_RATIO_MAX`], sustained by the next block. At (and
/// after) the knee the buffer is full, so `buffered = written − played`:
/// `B̂ = out_time(knee) − (knee_wall − mux_start)`, with `mux_start ≈ first_block_wall −
/// [`PROGRESS_STATS_PERIOD`]` (ffmpeg prints its first block one period into muxing) and
/// playout starting ≈ with muxing (the burst fills any prebuffer in a few tens of ms — true
/// on pipewire-pulse and classic pulse alike, so the estimate is server-agnostic). Clamped to
/// [`EFFECTIVE_BUFFER_MIN`]..=[`EFFECTIVE_BUFFER_MAX`].
///
/// `None` = knee unresolved, keep feeding blocks. Past [`CALIBRATION_DEADLINE_SECS`] without
/// one (a weird stream) it settles on the requested [`AUDIO_LATENCY_MS`] — exactly the
/// previous fixed-assumption design, never worse than it.
#[cfg_attr(windows, allow(dead_code))]
fn calibrate_effective_buffer(wall_offsets: &[f32], out_times: &[f32]) -> Option<f32> {
    let n = wall_offsets.len().min(out_times.len());
    // Pacing ratio of the interval ENDING at block `k` (wall stamps are measured arrivals,
    // so guard the degenerate zero-width interval: treat it as still bursting).
    let ratio = |k: usize| {
        let dw = wall_offsets[k] - wall_offsets[k - 1];
        if dw > f32::EPSILON { (out_times[k] - out_times[k - 1]) / dw } else { f32::INFINITY }
    };
    for knee in 1..n.saturating_sub(1) {
        if ratio(knee) < PACED_RATIO_MAX && ratio(knee + 1) < PACED_RATIO_MAX {
            let est = out_times[knee] - wall_offsets[knee] - PROGRESS_STATS_PERIOD;
            return Some(est.clamp(EFFECTIVE_BUFFER_MIN, EFFECTIVE_BUFFER_MAX));
        }
    }
    (n > 0 && wall_offsets[n - 1] >= CALIBRATION_DEADLINE_SECS)
        .then_some(AUDIO_LATENCY_MS as f32 / 1000.0)
}

/// Parse an ffmpeg `-progress` line for the muxed-audio position, in MICROSECONDS. ffmpeg
/// emits `out_time_us=<n>`; some builds also/only emit `out_time_ms=<n>` carrying the SAME
/// microsecond value (a long-standing misnomer — it is NOT milliseconds), so accept either
/// key. Returns `None` for any other line, a non-integer value, or an implausible one
/// (negative — e.g. the `AV_NOPTS` sentinel before the first sample — or beyond a day).
#[cfg_attr(windows, allow(dead_code))]
fn parse_progress_out_time_us(line: &str) -> Option<i64> {
    let line = line.trim();
    let v = line
        .strip_prefix("out_time_us=")
        .or_else(|| line.strip_prefix("out_time_ms="))?;
    let us: i64 = v.trim().parse().ok()?;
    (0..=86_400_000_000).contains(&us).then_some(us)
}

/// The AUDIBLE source position for a muxed-audio timestamp: what has reached the speakers lags
/// what ffmpeg has WRITTEN by the sink's effective buffer (`buffer_secs` — measured by
/// [`calibrate_effective_buffer`], or the requested size as its fallback), because ffmpeg's
/// blocking writes pace against that buffer once it is full. Clamped at `start_sec` through the
/// startup transient (muxed < buffer ⇒ nothing audible yet).
#[cfg_attr(windows, allow(dead_code))]
fn audible_pos_from_out_time(out_time_us: i64, start_sec: f32, buffer_secs: f32) -> f32 {
    let out_time_secs = out_time_us as f32 / 1_000_000.0;
    start_sec + (out_time_secs - buffer_secs).max(0.0)
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
    fn progress_out_time_us_line_parses_microseconds() {
        assert_eq!(parse_progress_out_time_us("out_time_us=1234567"), Some(1_234_567));
        assert_eq!(parse_progress_out_time_us("out_time_us=0"), Some(0));
        // Real ffmpeg lines are unindented, but tolerate incidental whitespace.
        assert_eq!(parse_progress_out_time_us("  out_time_us=250000  "), Some(250_000));
    }

    #[test]
    fn progress_out_time_ms_key_is_really_microseconds() {
        // ffmpeg's `out_time_ms` is a misnomer — it carries microseconds, like `out_time_us`.
        assert_eq!(parse_progress_out_time_us("out_time_ms=1234567"), Some(1_234_567));
    }

    #[test]
    fn progress_unrelated_keys_are_ignored() {
        // The `out_time=HH:MM:SS.us` string form and every other progress key yield nothing.
        assert_eq!(parse_progress_out_time_us("out_time=00:00:01.234567"), None);
        assert_eq!(parse_progress_out_time_us("frame=42"), None);
        assert_eq!(parse_progress_out_time_us("bitrate= 128.0kbits/s"), None);
        assert_eq!(parse_progress_out_time_us("progress=continue"), None);
    }

    #[test]
    fn progress_garbage_and_out_of_range_values_are_rejected() {
        assert_eq!(parse_progress_out_time_us("out_time_us=notanumber"), None);
        assert_eq!(parse_progress_out_time_us("out_time_us="), None);
        // Negative (the AV_NOPTS sentinel appears before the first sample) and absurdly huge.
        assert_eq!(parse_progress_out_time_us("out_time_us=-1"), None);
        assert_eq!(parse_progress_out_time_us("out_time_us=-9223372036854775808"), None);
        assert_eq!(parse_progress_out_time_us("out_time_us=999999999999999"), None);
    }

    #[test]
    fn audible_pos_lags_muxed_by_the_buffer_and_clamps_at_start() {
        // 0.35s muxed, 0.20s buffer → 0.15s of it is audible past the start point.
        let p = audible_pos_from_out_time(350_000, 5.0, 0.2);
        assert!((p - 5.15).abs() < 1e-4, "audible = {p}");
        // Startup transient (muxed < buffer): nothing audible yet → clamp to the start.
        let p0 = audible_pos_from_out_time(50_000, 5.0, 0.2);
        assert!((p0 - 5.0).abs() < 1e-4, "audible = {p0}");
        // Exactly at the buffer boundary → still the start position.
        let pboundary = audible_pos_from_out_time(200_000, 5.0, 0.2);
        assert!((pboundary - 5.0).abs() < 1e-4, "audible = {pboundary}");
    }

    #[test]
    fn calibrate_measures_buffer_when_burst_precedes_first_block() {
        // The shape real pipewire-pulse captures show: the whole burst fits inside the first
        // stats period (out_time already 450ms at block 0), then paced +100ms per 100ms block.
        // Knee at block 1: B̂ = 0.550 − 0.100 − PROGRESS_STATS_PERIOD = 0.350.
        let b = calibrate_effective_buffer(&[0.0, 0.1, 0.2], &[0.45, 0.55, 0.65]).unwrap();
        assert!((b - 0.35).abs() < 1e-6, "B̂ = {b}");
    }

    #[test]
    fn calibrate_measures_buffer_after_visible_burst_blocks() {
        // Burst 0→450ms across three blocks (ratio 1.5), then paced (+100ms/block).
        // Knee at block 3: B̂ = 0.550 − 0.300 − PROGRESS_STATS_PERIOD = 0.150.
        let wall = [0.0, 0.1, 0.2, 0.3, 0.4];
        let out = [0.15, 0.30, 0.45, 0.55, 0.65];
        let b = calibrate_effective_buffer(&wall, &out).unwrap();
        assert!((b - 0.15).abs() < 1e-6, "B̂ = {b}");
    }

    #[test]
    fn calibrate_withholds_until_the_knee_is_confirmed() {
        // Still bursting (ratio 1.5 everywhere) → no estimate; anchors stay unpublished and
        // the bootstrap clock rules.
        assert_eq!(calibrate_effective_buffer(&[0.0, 0.1, 0.2], &[0.15, 0.30, 0.45]), None);
        // One paced ratio alone isn't enough — it must sustain for a second block.
        assert_eq!(calibrate_effective_buffer(&[0.0, 0.1], &[0.45, 0.55]), None);
        // A zero-width interval (duplicate wall stamp) counts as bursting, not paced.
        assert_eq!(calibrate_effective_buffer(&[0.0, 0.0, 0.1], &[0.1, 0.2, 0.3]), None);
        assert_eq!(calibrate_effective_buffer(&[], &[]), None);
    }

    #[test]
    fn calibrate_clamps_the_estimate_to_plausible_buffers() {
        // Paced from the very start with a near-empty buffer → raw estimate 0.0s, floored.
        let b = calibrate_effective_buffer(&[0.0, 0.1, 0.2], &[0.1, 0.2, 0.3]).unwrap();
        assert!((b - EFFECTIVE_BUFFER_MIN).abs() < 1e-6, "B̂ = {b}");
        // An absurd 1.9s estimate is capped at the ceiling.
        let b = calibrate_effective_buffer(&[0.0, 0.1, 0.2], &[2.0, 2.1, 2.2]).unwrap();
        assert!((b - EFFECTIVE_BUFFER_MAX).abs() < 1e-6, "B̂ = {b}");
    }

    #[test]
    fn calibrate_falls_back_to_the_requested_buffer_after_the_deadline() {
        // A stream that never stops bursting (ratio 2.0 throughout) past the deadline settles
        // on the requested AUDIO_LATENCY_MS — the previous fixed-assumption design.
        let wall: Vec<f32> = (0..27).map(|i| i as f32 * 0.1).collect();
        let out: Vec<f32> = (0..27).map(|i| i as f32 * 0.2).collect();
        let b = calibrate_effective_buffer(&wall, &out).unwrap();
        assert!((b - 0.2).abs() < 1e-6, "B̂ = {b}");
        // Same burst but short of the deadline → still waiting.
        assert_eq!(calibrate_effective_buffer(&wall[..20], &out[..20]), None);
    }
}
