//! Video RECORDING pipelines built on the low-level screencopy client
//! ([`crate::screencopy`]) and the PipeWire portal capture ([`crate::pwcapture`]).
//!
//! - Region/monitor screencopy recording: we own the capture — grab the region's
//!   output each frame, crop it, and pipe raw frames to ffmpeg (hardware-encode when
//!   an NVENC/VAAPI encoder is present, else software x264).
//! - PipeWire recording: consume a portal ScreenCast stream and encode it.
//! - GPU zero-copy paths (`#[cfg(feature = "zero-copy")]`): import DMA-BUF frames
//!   straight into an in-process encoder with no CPU readback.
//!
//! The still-screenshot grabs that used to live here now sit in [`crate::screenshot`];
//! the low-level Wayland screencopy client sits in [`crate::screencopy`].
//!
//! # The recording-worker contract (the platform seam)
//!
//! Every capture platform provides ONE worker (on Linux `record::screencopy` /
//! `record::pipewire`, each with a `zero_copy` GPU variant; on macOS `record::sck`; a
//! future Windows worker plugs in the same way — see the seam map in
//! [`crate::platform`]). A worker OWNS its capture
//! connection for the whole session and posts EXACTLY ONE [`RecordHandle::done`]
//! `Result`. That contract is enforced by STRUCTURE, not convention:
//!
//! - [`DoneGuard`] guarantees the single `done` post even if the worker thread panics or
//!   returns early (DRAGON-106): its `Drop` fills an error if nothing was posted.
//! - [`RecordShared`] mints the stop/paused/done/events/measured/dims Arc quartet once
//!   and hands the worker its clone — there is no other way to build a [`RecordHandle`],
//!   so every entry point wires the same lifecycle.
//! - [`wait_or_kill`] / [`MuxerWatchdog`] make every wait BOUNDED (DRAGON-118): nothing
//!   in a stop tail can hang the recorder (and the preview spinner) forever.
//! - `owned::run_video_stop_tail` is the SINGLE stop-tail
//!   implementation the RGBA workers share (DRAGON-161): the covering ticks are
//!   bounded/non-blocking, the reap is bounded, and the salvage-vs-fail decision is the
//!   pure, unit-tested `owned::salvage_decision`. A worker becomes a thin capture
//!   adapter feeding this shared core, rather than re-deriving the media-clock teardown.
//!
//! A new platform implements: a `cfg`-gated `start_region_recording`, a capture
//! connection that feeds frames to the media-clock loop, and — reusing `record::owned`
//! and `record::pump` verbatim — the shared stop tail. It does NOT reimplement the
//! media clock, the FIFO/audio pre-flight, the watchdog, or the salvage logic.

#[cfg(target_os = "linux")]
use cosmic_client_toolkit::screencopy::{CaptureOptions, CaptureSource, ScreencopySessionData};
#[cfg(target_os = "linux")]
use cosmic_client_toolkit::sctk::shm::slot::SlotPool;
#[cfg(target_os = "linux")]
use crate::screencopy::{connect, grab_cropped, outputs, pick_format};
#[cfg(target_os = "linux")]
use std::io::Write;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

mod finalize;
// The Wayland screencopy + portal PipeWire capture workers are the Linux capture
// paths; macOS records via ScreenCaptureKit (DRAGON-94 phase 3). pump/finalize/
// sync_probe are pure ffmpeg/audio and stay portable. `owned` holds the portable
// media-clock owned-path plumbing (frame writer + audio pre-flight) both the Linux
// workers and the macOS SCK worker reuse.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod owned;
#[cfg(target_os = "linux")]
mod pipewire;
mod pump;
// The SCK worker lives PHYSICALLY under `platform/mac/` (the closed-platform
// split, DRAGON-226) but stays mounted at `record::sck` — module paths are
// stable via `#[path]`, exactly like the `platform/mod.rs` mount registry.
#[cfg(target_os = "macos")]
#[path = "../platform/mac/screencapturekit/record_worker.rs"]
mod sck;
// LIVE SCK end-to-end proof — `#[ignore]`-gated (needs a display + TCC + ffmpeg).
#[cfg(all(test, target_os = "macos"))]
#[path = "../platform/mac/screencapturekit/record_worker_live_tests.rs"]
mod sck_live_tests;
#[cfg(target_os = "linux")]
mod screencopy;
// LIVE Linux screencopy anti-freeze proof (DRAGON-167) — `#[ignore]`-gated (needs a
// COSMIC session + a Pulse server + ffmpeg/ffplay). The PipeWire fixed-shape path can't
// be driven headlessly (interactive portal grant); it is unit-covered in `pipewire`.
#[cfg(all(test, target_os = "linux"))]
mod screencopy_live_tests;
mod sync_probe;
#[cfg(feature = "zero-copy")]
mod zero_copy;
#[cfg(test)]
mod av_sync_tests;
// The owned media-clock E2E tests exercise the Linux pipewire owned-audio path
// (and real ffmpeg); macOS records via SCK (DRAGON-94 phase 3), so gate to Linux.
#[cfg(all(test, target_os = "linux"))]
mod media_clock_e2e_tests;

pub(crate) use sync_probe::{measure_av_offset, write_sync_clip, SYNC_CLIP_NAME};
#[cfg(feature = "zero-copy")]
pub use zero_copy::screencopy_dmabuf_test;

/// (Debug) End-to-end capture→encode of the FULL largest output at `fps` for
/// `secs` seconds (heaviest case), forcing a specific encoder `backend`
/// (`gpu`/`nvenc`, `cpu`/`vaapi`, `sw`). `--bench-record [secs] [fps] [backend]`.
#[cfg(target_os = "linux")]
pub fn bench_record(secs: u64, fps: u32, backend: &str) {
    use std::time::{Duration, Instant};
    let Some((conn, mut queue, mut data)) = connect(false) else {
        eprintln!("bench: connect failed");
        return;
    };
    let Some((output, name, _, (ow, oh))) = outputs(&data)
        .into_iter()
        .max_by_key(|(_, _, _, (w, h))| (*w as i64) * (*h as i64))
    else {
        eprintln!("bench: no output");
        return;
    };
    let src = CaptureSource::Output(output);
    let qh = queue.handle();
    data.formats = None;
    let Ok(session) = data.screencopy_state.capturer().create_session(
        &src,
        CaptureOptions::empty(),
        &qh,
        ScreencopySessionData::default(),
    ) else {
        return;
    };
    let _ = conn.flush();
    while data.formats.is_none() {
        if queue.blocking_dispatch(&mut data).is_err() {
            return;
        }
    }
    let Some(formats) = data.formats.clone() else {
        eprintln!("bench: no capture formats reported");
        return;
    };
    let (cw, ch) = formats.buffer_size;
    let Some((format, swizzle, force_opaque)) = pick_format(&formats.shm_formats) else {
        eprintln!("bench: no usable shm format offered by the compositor");
        return;
    };
    let stride = cw * 4;
    let Ok(mut pool) = SlotPool::new((stride * ch) as usize, &data.shm) else {
        eprintln!("bench: shm pool allocation failed");
        return;
    };
    let Ok((buffer, _)) = pool.create_buffer(cw as i32, ch as i32, stride as i32, format) else {
        eprintln!("bench: shm buffer allocation failed");
        return;
    };
    let even = |v: u32| v - (v % 2);
    let (ew, eh) = (even(cw), even(ch));
    let temp = std::path::Path::new("/tmp/cck-bench-record.mkv");
    let Some(plan) = crate::encode::EncodePlan::for_backend(backend, ew, eh, &crate::encode::Presets::default()) else {
        eprintln!("bench: encoder backend '{backend}' not available on this system");
        return;
    };
    let nv12 = plan.nv12;
    // The bench measures video encode throughput only; it feeds the media-clock
    // muxer two INERT audio FIFOs (silence, written continuously by their own
    // threads) instead of wiring up the real mic/system capture chains — simplest
    // coherent equivalent to `spawn_ffmpeg_media_clock`'s two-FIFO contract that
    // keeps this benchmark's video-fps measurement meaningful (DRAGON-127 retired
    // the legacy `spawn_ffmpeg`/raw-pulse-input path this used to call).
    let dir = crate::util::runtime_dir();
    let pid = std::process::id();
    let mic_fifo_path = PathBuf::from(format!("{dir}/cosmic-capture-kit.{pid}.bench-mic.pcm"));
    let sys_fifo_path = PathBuf::from(format!("{dir}/cosmic-capture-kit.{pid}.bench-sys.pcm"));
    if !bench_mkfifo(&mic_fifo_path) || !bench_mkfifo(&sys_fifo_path) {
        eprintln!("bench: could not create the inert audio FIFOs");
        let _ = std::fs::remove_file(&mic_fifo_path);
        let _ = std::fs::remove_file(&sys_fifo_path);
        return;
    }
    let audio_stop = Arc::new(AtomicBool::new(false));
    let mic_writer = spawn_silence_writer(mic_fifo_path.clone(), 1, audio_stop.clone());
    let sys_writer = spawn_silence_writer(sys_fifo_path.clone(), 2, audio_stop.clone());
    let Ok(mut child) = crate::encode::spawn_ffmpeg_media_clock(
        ew, eh, ew, eh, fps, &plan, 50_000, temp, &mic_fifo_path, &sys_fifo_path,
    ) else {
        eprintln!("bench: ffmpeg spawn failed");
        audio_stop.store(true, Ordering::Relaxed);
        let _ = mic_writer.join();
        let _ = sys_writer.join();
        let _ = std::fs::remove_file(&mic_fifo_path);
        let _ = std::fs::remove_file(&sys_fifo_path);
        return;
    };
    let Some(stdin) = child.stdin.take() else {
        eprintln!("bench: ffmpeg stdin unavailable");
        let _ = child.kill();
        audio_stop.store(true, Ordering::Relaxed);
        let _ = mic_writer.join();
        let _ = sys_writer.join();
        let _ = std::fs::remove_file(&mic_fifo_path);
        let _ = std::fs::remove_file(&sys_fifo_path);
        return;
    };
    // Capture thread sends raw RGBA; the writer thread converts (NV12 path) or
    // passes through (software path) + writes, overlapping the capture's idle wait.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(16);
    let (ews, ehs) = (ew as usize, eh as usize);
    let nv12_len = ews * ehs * 3 / 2;
    let writer = std::thread::spawn(move || {
        let mut stdin = stdin;
        let mut nv12buf = vec![0u8; nv12_len];
        while let Ok(rgba) = rx.recv() {
            let out = if nv12 {
                crate::encode::rgba_to_nv12(&rgba, ews, ehs, &mut nv12buf);
                &nv12buf[..]
            } else {
                &rgba[..]
            };
            if stdin.write_all(out).is_err() {
                break;
            }
        }
    });
    eprintln!(
        "bench: recording {name} {ow}x{oh} (enc {ew}x{eh}, {backend}, {}) @ {fps}fps for {secs}s…",
        if nv12 { "NV12" } else { "RGBA" }
    );
    let frame_dur = Duration::from_secs_f64(1.0 / fps as f64);
    let start = Instant::now();
    let mut next = start + frame_dur;
    let mut captured = 0u64;
    while start.elapsed().as_secs() < secs {
        let now = Instant::now();
        if now < next {
            std::thread::sleep(next - now);
        }
        next += frame_dur;
        // Cap drift to one frame so a slow grab can't spiral the pacing into a
        // busy-spin (matches the live record loop).
        if next < now {
            next = now + frame_dur;
        }
        // Bench encodes every frame (ignore the damage flag).
        if let Some((rgba, _)) = grab_cropped(
            &conn, &mut queue, &mut data, &qh, &session, &buffer, &mut pool, cw, swizzle,
            force_opaque, 0, 0, ew, eh,
        ) {
            if tx.send(rgba).is_err() {
                break;
            }
            captured += 1;
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    drop(tx);
    let _ = writer.join();
    audio_stop.store(true, Ordering::Relaxed);
    let _ = mic_writer.join();
    let _ = sys_writer.join();
    let _ = child.wait();
    let _ = std::fs::remove_file(&mic_fifo_path);
    let _ = std::fs::remove_file(&sys_fifo_path);
    eprintln!(
        "bench: captured {captured} frames in {elapsed:.2}s => {:.1} fps fed to encoder",
        captured as f64 / elapsed,
    );
}

/// Create a FIFO at `path` for [`bench_record`]'s inert audio inputs (clearing any
/// stale one a prior crashed bench left behind).
#[cfg(target_os = "linux")]
fn bench_mkfifo(path: &std::path::Path) -> bool {
    let _ = std::fs::remove_file(path);
    rustix::fs::mkfifoat(rustix::fs::CWD, path, rustix::fs::Mode::from_bits_truncate(0o600)).is_ok()
}

/// Feed `path` a steady stream of silence (`channels`-interleaved f32le @ 48kHz,
/// 20ms per chunk) until `stop` — [`bench_record`]'s inert stand-in for the real
/// mic/system FIFOs `spawn_ffmpeg_media_clock` expects, so the bench can exercise
/// the real media-clock muxer command without wiring up live audio capture. Opens
/// the FIFO's write end on this thread (blocks until ffmpeg opens the read side,
/// exactly like the real pump's FIFO writers) and exits once `stop` is seen or the
/// write fails (ffmpeg gone).
#[cfg(target_os = "linux")]
fn spawn_silence_writer(
    path: PathBuf,
    channels: u8,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Ok(fd) = rustix::fs::open(
            &path,
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) else {
            return;
        };
        let mut out = std::fs::File::from(fd);
        let chunk = vec![0u8; 48_000 / 50 * 4 * channels as usize]; // 20ms of silence
        while !stop.load(Ordering::Relaxed) {
            if out.write_all(&chunk).is_err() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    })
}

// ---------------------------------------------------------------------------
// Region video recording (we own the capture: grab the region's output each
// frame, crop it, and pipe raw frames to ffmpeg). Hardware-encode when an
// NVENC/VAAPI encoder is present, else fall back to software x264 so recording
// always works.
// ---------------------------------------------------------------------------

/// A recorded audio channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AudioChannel {
    Mic,
    Sys,
}

/// A live channel toggle: when (wall-clock) the user flipped a channel, and to
/// what. Converted to a video-relative PTS at finalize time.
pub type ToggleEvent = (std::time::Instant, AudioChannel, bool);

/// A running recording. The UI sets `stop` to finish and appends channel toggles
/// to `events` (timestamped live); the worker captures video + both audio channels
/// continuously, then on stop bakes the toggles in at their exact timestamps and
/// posts the final path (or an error) into `done`.
pub struct RecordHandle {
    pub stop: Arc<AtomicBool>,
    /// Pause switch (DRAGON-111). While true the worker ends its current
    /// segment — the full stop tail runs, so the segment's audio is complete —
    /// and idles with its capture connection (screencopy session / PipeWire
    /// stream) still open; clearing it starts the next segment. Nothing is
    /// captured while paused. The final stop stitches the segments together
    /// before the finalize pass. `stop` always wins over `paused`.
    pub paused: Arc<AtomicBool>,
    pub done: Arc<Mutex<Option<Result<PathBuf, String>>>>,
    pub events: Arc<Mutex<Vec<ToggleEvent>>>,
    /// Measured A/V latency (median ms) for this recording, for auto-calibration;
    /// `None` if it couldn't be measured (e.g. no frame timestamps).
    pub measured_offset_ms: Arc<Mutex<Option<i32>>>,
    /// The recording's CAPTURED footprint `(w, h)` in physical pixels — the area the
    /// recording covers ON SCREEN, posted by the worker the moment its first frame
    /// fixes it, BEFORE the max-resolution/codec caps shrink the encode. The UI
    /// reads it at stop time to open the preview sized to what was captured (a
    /// res-capped encode then upscales back into that box for display), so a
    /// full-monitor recording previews monitor-sized even when it encodes at
    /// 1080p. `None` only if stopped before the first frame.
    pub dims: Arc<Mutex<Option<(u32, u32)>>>,
}

/// Guarantees a recording's `done` slot is filled even if the worker thread unwinds
/// (panics) or returns early without posting a result. Held for the worker's lifetime;
/// on drop it fills an error only if nothing was posted. Without this, a worker that dies
/// silently leaves `done` empty forever and the UI's `RecordingPoll` waits on it — so the
/// preview stays stuck on its spinner (DRAGON-106).
struct DoneGuard(Arc<Mutex<Option<Result<PathBuf, String>>>>);
impl Drop for DoneGuard {
    fn drop(&mut self) {
        if let Ok(mut g) = self.0.lock()
            && g.is_none()
        {
            *g = Some(Err("recording worker exited without a result".into()));
        }
    }
}

/// Reap `child`, waiting up to `timeout`; one that outlives it is SIGKILLed and
/// reaped. NOTHING in a stop tail may wait unboundedly (DRAGON-118): a wedged
/// ffmpeg — seen live on n8.1.2, its threaded scheduler deadlocked with every
/// queue full and zero packets muxed — survives stdin EOF indefinitely, and a
/// plain `wait()` then hangs the recording worker forever (the preview spinner
/// never resolves). A kill surfaces as the non-success exit status the callers'
/// salvage arms already handle.
pub(crate) fn wait_or_kill(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Ok(s),
            Ok(None) => {}
            Err(e) => return Err(e),
        }
        if std::time::Instant::now() >= deadline {
            log::warn!("ffmpeg still running {timeout:?} after its inputs closed; killing it");
            let _ = child.kill();
            return child.wait();
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// How long a segment muxer gets to show life before the recorder declares it
/// wedged, and the size its output must reach by then. A healthy ffmpeg writes
/// the container header instantly and (with `-flush_packets 1`) grows the file
/// past it within the first second or two of fed frames; one wedged at startup
/// (DRAGON-118) holds the bare ~1KB header forever while consuming frames — the
/// user would record minutes of nothing.
pub(crate) const MUXER_LIVENESS_SECS: u64 = 12;
const MUXER_LIVENESS_BYTES: u64 = 4096;

/// Whether the segment muxer has shown life: its output has grown past the bare
/// container header. Checked once per segment, [`MUXER_LIVENESS_SECS`] in.
pub(crate) fn muxer_alive(path: &std::path::Path) -> bool {
    std::fs::metadata(path).map(|m| m.len() > MUXER_LIVENESS_BYTES).unwrap_or(false)
}

/// A per-segment wedge-watchdog: SIGKILLs a just-spawned segment muxer if its output
/// file hasn't outgrown the bare container header within [`MUXER_LIVENESS_SECS`] of
/// arming — the DRAGON-118 [`muxer_alive`] check, run on its OWN thread so it fires even
/// while the worker (or its writer thread) is BLOCKED on the segment's FIRST frame write.
///
/// That first `write_all` of a raw frame dwarfs the 64K pipe, so it blocks until ffmpeg
/// drains it; an ffmpeg n8.1.2 whose threaded scheduler wedged at start (header written,
/// then zero packets, every thread asleep — it never reads stdin) hangs that write
/// forever, and the in-loop liveness check can't help because it only runs AFTER the
/// first write returns (DRAGON-123: the second FIFO's extra startup rendezvous makes that
/// wedge more reachable). Killing the muxer makes the pending write fail (EPIPE) so the
/// worker takes its normal wedge/salvage path. The DRAGON-118 rule holds: nothing in the
/// record path — including the first-frame write — waits unboundedly. Idempotent.
pub(crate) struct MuxerWatchdog {
    disarm: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MuxerWatchdog {
    /// Arm the watchdog for muxer process `pid` writing `seg_path`, pause-aware:
    /// `paused` time does not count against the budget. The media-clock owned
    /// pipeline's session-long ffmpeg is fed NOTHING on any input while paused, by
    /// design, so a user pausing before the muxer's first output packet (encoder
    /// lookahead not yet filled) must not read as a wedge and get the recording
    /// killed. Call [`disarm`](Self::disarm) once the muxer has shown life (or the
    /// session is ending); dropping the handle disarms + joins the (bounded) thread.
    pub(crate) fn arm_gated(
        pid: u32,
        seg_path: std::path::PathBuf,
        paused: Arc<AtomicBool>,
    ) -> Self {
        Self::arm_for(
            pid,
            seg_path,
            std::time::Duration::from_secs(MUXER_LIVENESS_SECS),
            Some(paused),
        )
    }

    /// [`arm_gated`](Self::arm_gated) with an explicit budget (the tests use a short
    /// one) and an optional pause gate: the budget is spent only while unpaused.
    fn arm_for(
        pid: u32,
        seg_path: std::path::PathBuf,
        budget: std::time::Duration,
        paused: Option<Arc<AtomicBool>>,
    ) -> Self {
        let disarm = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        let (d, f) = (disarm.clone(), fired.clone());
        let thread = std::thread::Builder::new()
            .name("cck-muxer-watchdog".to_string())
            .spawn(move || {
                let mut left = budget;
                let mut last = std::time::Instant::now();
                while !left.is_zero() {
                    if d.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    let now = std::time::Instant::now();
                    let elapsed = now.saturating_duration_since(last);
                    last = now;
                    // Paused time is free (see `arm_gated`): the muxer is fed
                    // nothing then, by design, so it cannot prove liveness.
                    if !paused.as_ref().is_some_and(|p| p.load(Ordering::Relaxed)) {
                        left = left.saturating_sub(elapsed);
                    }
                }
                // Deadline reached: a muxer that has shown life (or a disarm that raced
                // in) is fine — leave it. Otherwise it's wedged while we (may be) blocked
                // on the first-frame write: kill it so that write fails and unblocks.
                if d.load(Ordering::Relaxed) || muxer_alive(&seg_path) {
                    return;
                }
                log::error!(
                    "recording muxer wrote no output within {MUXER_LIVENESS_SECS}s of starting \
                     while being fed its first frame — wedged ffmpeg; killing it so the write \
                     can't hang the recorder (DRAGON-118)"
                );
                if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
                f.store(true, Ordering::Relaxed);
            })
            .ok();
        Self { disarm, fired, thread }
    }

    /// Stop the watchdog — the muxer has shown life, or the segment is ending. After this
    /// it can no longer fire. Idempotent.
    pub(crate) fn disarm(&self) {
        self.disarm.store(true, Ordering::Relaxed);
    }

    /// Whether the watchdog fired (killed a wedged muxer) — the caller folds this into
    /// its `muxer_wedged` flag so the segment takes the salvage path.
    pub(crate) fn fired(&self) -> bool {
        self.fired.load(Ordering::Relaxed)
    }
}

impl Drop for MuxerWatchdog {
    fn drop(&mut self) {
        self.disarm();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Current CLOCK_MONOTONIC time in nanoseconds (same clock as PipeWire frame pts),
/// for measuring how long a frame takes to reach the encoder.
/// Linux-only: the PipeWire worker (`record/pipewire.rs`) is the sole caller.
#[cfg(target_os = "linux")]
fn monotonic_ns() -> i64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as i64 * 1_000_000_000 + t.tv_nsec as i64
}

/// Median of latency samples (ms), clamped to a sane recording offset range. The
/// offset is applied once at finalize (per-channel `adelay`/`atrim`; DRAGON-119),
/// never on the live capture inputs, so any magnitude in range is stall-safe (ffmpeg
/// 8 would stall on a live `-itsoffset` this large). `None` when there aren't enough
/// samples to trust.
/// Linux-only: the PipeWire worker (`record/pipewire.rs`) is the sole caller.
#[cfg(target_os = "linux")]
fn median_offset_ms(mut samples: Vec<i64>) -> Option<i32> {
    if samples.len() < 5 {
        return None;
    }
    samples.sort_unstable();
    let m = samples[samples.len() / 2];
    Some(m.clamp(0, 1000) as i32)
}

/// Temp file the recording is captured to (video + both audio channels, ungated)
/// before the finalize pass produces `out_path`. Deterministic so the UI can read
/// its growing size for the live readout.
pub fn recording_temp_path(out_path: &std::path::Path) -> PathBuf {
    let stem = recording_stem(out_path);
    out_path.with_file_name(format!(".{stem}.recording.mkv"))
}

fn recording_stem(out_path: &std::path::Path) -> String {
    out_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "recording".into())
}

/// The stop/done/events/measured-offset Arc+Mutex quartet shared by both recording
/// entry points below (region/screencopy and portal/PipeWire): each builds one via
/// [`Self::new`], clones it once for the worker thread via [`Self::clone_for_worker`],
/// and turns the original into the [`RecordHandle`] it returns via [`Self::into_handle`]
/// — exactly the two Arc-clone "generations" the old hand-written setup had.
struct RecordShared {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    done: Arc<Mutex<Option<Result<PathBuf, String>>>>,
    events: Arc<Mutex<Vec<ToggleEvent>>>,
    measured_offset_ms: Arc<Mutex<Option<i32>>>,
    dims: Arc<Mutex<Option<(u32, u32)>>>,
}

impl RecordShared {
    fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            done: Arc::new(Mutex::new(None)),
            events: Arc::new(Mutex::new(Vec::new())),
            measured_offset_ms: Arc::new(Mutex::new(None)),
            dims: Arc::new(Mutex::new(None)),
        }
    }

    /// A second Arc clone of the quartet, moved into the worker thread's closure.
    fn clone_for_worker(&self) -> Self {
        Self {
            stop: self.stop.clone(),
            paused: self.paused.clone(),
            done: self.done.clone(),
            events: self.events.clone(),
            measured_offset_ms: self.measured_offset_ms.clone(),
            dims: self.dims.clone(),
        }
    }

    /// The public handle the UI polls/controls.
    fn into_handle(self) -> RecordHandle {
        RecordHandle {
            stop: self.stop,
            paused: self.paused,
            done: self.done,
            events: self.events,
            measured_offset_ms: self.measured_offset_ms,
            dims: self.dims,
        }
    }
}

/// Encode + destination settings shared by both recording entry points — everything
/// downstream of "what pixels are we capturing" (that part differs per entry point;
/// see [`RegionRecordParams`] / [`PipewireRecordParams`]).
pub struct RecordSettings {
    pub fps: u32,
    pub preferred_encoder: String,
    pub presets: crate::encode::Presets,
    pub zero_copy: bool,
    pub mic: bool,
    pub system_audio: bool,
    pub bitrate_kbps: u32,
    pub audio_offset_ms: i32,
    /// Auto A/V sync (DRAGON-119): when on, each worker probes the live audio-device
    /// latency and folds it into the SYSTEM channel's finalize delay. When off, the
    /// user's manual `audio_offset_ms` stands exactly as set (monitor extra forced to 0).
    pub auto_device_compensation: bool,
    pub max_res: (u32, u32),
    pub metadata: String,
    pub out_path: PathBuf,
}

/// The macOS SCK capture target for a recording (DRAGON-130). On macOS every record
/// mode reaches [`start_region_recording`] through [`RegionRecordParams`]; this says
/// which SCK content filter the video worker attaches to:
///
/// - [`Region`](Self::Region): a CROP of the most-overlapped display — the `x/y/w/h`
///   rect selects the sub-area (a `setSourceRect` on a display filter).
/// - [`Window`](Self::Window): a specific window by `CGWindowID`, via SCK's
///   desktop-independent filter — occluding windows are NOT recorded and capture
///   follows the window as it moves (unlike the old display-crop approximation).
/// - [`Display`](Self::Display): a whole display (monitor mode) by `Display-<id>`
///   output name — the display filter at full bounds, no crop.
///
/// macOS-only: the field carrying it is `#[cfg(target_os = "macos")]`, so the Linux
/// workers never see it and the Linux construction sites stay byte-identical.
#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MacRecordTarget {
    /// Record the `x/y/w/h` rect as a crop of the most-overlapped display.
    Region,
    /// Record a single window by its `CGWindowID` (occlusion-independent).
    Window(u32),
    /// Record the whole display named `Display-<id>` (monitor mode).
    Display(String),
}

/// Parameters for [`start_region_recording`]: the region (global logical coords) +
/// cursor flag, plus the settings shared with the PipeWire path.
pub struct RegionRecordParams {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub cursor: bool,
    /// The macOS SCK capture target (region crop / window / display). macOS-only —
    /// the Linux workers never see this field, so their construction sites stay
    /// byte-identical (DRAGON-130).
    #[cfg(target_os = "macos")]
    pub mac_target: MacRecordTarget,
    pub settings: RecordSettings,
}

/// Parameters for [`start_pipewire_recording`]: the portal stream + optional crop,
/// plus the settings shared with the region path.
// Constructed in the (dead-on-mac) `start_held_pipewire_recording` but READ only by the
// Linux-gated `start_pipewire_recording`; the fields are a cohesive unit, so allow the
// never-read lint off Linux rather than cfg-splitting the struct + its one constructor.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct PipewireRecordParams {
    pub fd: OwnedFd,
    pub node_id: u32,
    pub crop: Option<(u32, u32, u32, u32)>,
    pub settings: RecordSettings,
}

/// Build a [`RecordHandle`] whose `done` is already filled with an error — the
/// macOS/Windows stand-in until the SCK/WGC capture workers land (DRAGON-94/95
/// phase 3). Keeps the app's recording state machine (poll/stop/preview) working:
/// it polls `done`, sees the error, and surfaces it instead of hanging.
#[cfg(not(target_os = "linux"))]
fn unsupported_recording(msg: &str) -> RecordHandle {
    let shared = RecordShared::new();
    if let Ok(mut g) = shared.done.lock() {
        *g = Some(Err(msg.to_string()));
    }
    shared.into_handle()
}

/// Start recording a region (global logical coords) to `out_path` at `fps` on
/// macOS via ScreenCaptureKit (DRAGON-130 phase 3): the SCStream video worker
/// (`record::sck`) drives the SAME media-clock owned pipeline the Linux workers do
/// (audio pre-flight → `spawn_ffmpeg_media_clock` → `pump` → finalize). Mirrors the
/// Linux `start_region_recording`'s handle/threading so the app's recording state
/// machine (poll/stop/preview) works unchanged. Window/Monitor modes reach here as
/// a region rect too (the app funnels every mode through `RegionRecordParams` on
/// macOS — there is no per-window/per-display target plumbing yet).
#[cfg(target_os = "macos")]
pub fn start_region_recording(params: RegionRecordParams) -> RecordHandle {
    let RegionRecordParams { x, y, w, h, cursor, mac_target, settings } = params;
    let RecordSettings {
        fps, preferred_encoder, presets, zero_copy, mic, system_audio, bitrate_kbps,
        audio_offset_ms, auto_device_compensation, max_res, metadata, out_path,
    } = settings;
    // No GPU zero-copy path on macOS: h264_videotoolbox encodes inside ffmpeg from
    // the RGBA feed, so there is nothing for the (Linux DRM/DMA-BUF) zero-copy path
    // to claim.
    let _ = zero_copy;
    let shared = RecordShared::new();
    {
        // `measured_offset_ms` is intentionally not passed to `record_sck`: the SCK
        // worker feeds ffmpeg synchronously on its own thread with no delivery
        // channel to measure lag on (same reasoning as the Linux screencopy path).
        let RecordShared { stop, paused, done, events, dims, .. } = shared.clone_for_worker();
        std::thread::spawn(move || {
            let _done_guard = DoneGuard(done.clone());
            let result = sck::record_sck(
                x, y, w, h, fps.max(1), cursor, &preferred_encoder, &presets, mic,
                system_audio, bitrate_kbps, audio_offset_ms, auto_device_compensation, max_res,
                &out_path, stop, paused, &events, &dims, &metadata, mac_target,
            );
            if let Ok(mut g) = done.lock() {
                *g = Some(result);
            }
        });
    }
    shared.into_handle()
}

/// Start recording a region — unsupported stand-in for platforms with no capture
/// backend (neither Linux screencopy/PipeWire nor macOS ScreenCaptureKit).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn start_region_recording(params: RegionRecordParams) -> RecordHandle {
    let _ = params;
    unsupported_recording("screen recording is not supported on this platform")
}

/// Start recording a region (global logical coords) to `out_path` at `fps`.
/// `mic`/`system_audio` are the channels' initial on/off state at t=0.
#[cfg(target_os = "linux")]
pub fn start_region_recording(params: RegionRecordParams) -> RecordHandle {
    let RegionRecordParams { x, y, w, h, cursor, settings } = params;
    let RecordSettings {
        fps, preferred_encoder, presets, zero_copy, mic, system_audio, bitrate_kbps,
        audio_offset_ms, auto_device_compensation, max_res, metadata, out_path,
    } = settings;
    let shared = RecordShared::new();
    {
        // `measured_offset_ms` is intentionally not passed to `record_screencopy`: the
        // owned screencopy loop grabs and writes synchronously with no delivery channel,
        // so it has no lag figure to measure (see `record_screencopy_owned`'s doc).
        let RecordShared { stop, paused, done, events, dims, .. } = shared.clone_for_worker();
        std::thread::spawn(move || {
            let _done_guard = DoneGuard(done.clone());
            let result = screencopy::record_screencopy(
                x, y, w, h, fps.max(1), cursor, &preferred_encoder, &presets, mic,
                system_audio, bitrate_kbps, audio_offset_ms, auto_device_compensation, max_res,
                &out_path, stop, paused, &events, &dims, &metadata, zero_copy,
            );
            if let Ok(mut g) = done.lock() {
                *g = Some(result);
            }
        });
    }
    shared.into_handle()
}

/// Record a portal/PipeWire stream (`node_id` on the portal `fd`) to `out_path`,
/// optionally cropped (stream pixels) for region mode. Mirrors
/// [`start_region_recording`]'s handle/threading so the app's recording state
/// machine (poll/stop/finalize, live audio toggles) works unchanged.
#[cfg(not(target_os = "linux"))]
pub fn start_pipewire_recording(params: PipewireRecordParams) -> RecordHandle {
    let _ = params;
    // The xdg-portal/PipeWire capture path has no macOS analog (SCK replaces it).
    unsupported_recording("portal/PipeWire recording is Linux-only")
}

/// Record a portal/PipeWire stream (`node_id` on the portal `fd`) to `out_path`,
/// optionally cropped (stream pixels) for region mode. Mirrors
/// [`start_region_recording`]'s handle/threading so the app's recording state
/// machine (poll/stop/finalize, live audio toggles) works unchanged.
#[cfg(target_os = "linux")]
pub fn start_pipewire_recording(params: PipewireRecordParams) -> RecordHandle {
    let PipewireRecordParams { fd, node_id, crop, settings } = params;
    let RecordSettings {
        fps, preferred_encoder, presets, zero_copy, mic, system_audio, bitrate_kbps,
        audio_offset_ms, auto_device_compensation, max_res, metadata, out_path,
    } = settings;
    let shared = RecordShared::new();
    // The opt-in GPU zero-copy path applies only to a full (uncropped) stream with
    // hardware encoding on; otherwise (and on any failure) we use the CPU path. Dup
    // the portal fd up front so the fallback still has a live handle to connect with.
    #[cfg(feature = "zero-copy")]
    let try_zero_copy = zero_copy && preferred_encoder != "software" && crop.is_none();
    #[cfg(feature = "zero-copy")]
    let fallback_fd = if try_zero_copy { fd.try_clone().ok() } else { None };
    #[cfg(not(feature = "zero-copy"))]
    let _ = zero_copy;
    {
        let RecordShared { stop, paused, done, events, measured_offset_ms: measured, dims } =
            shared.clone_for_worker();
        std::thread::spawn(move || {
            let _done_guard = DoneGuard(done.clone());
            let paused_cpu = paused.clone();
            let cpu = |fd: OwnedFd, stop: Arc<AtomicBool>| {
                pipewire::record_pipewire(
                    fd, node_id, crop, fps.max(1), &preferred_encoder, &presets, mic,
                    system_audio, bitrate_kbps, audio_offset_ms, auto_device_compensation, max_res,
                    &out_path, stop, paused_cpu, &events, &measured, &dims, &metadata,
                )
            };
            #[cfg(feature = "zero-copy")]
            let result = if try_zero_copy {
                match zero_copy::record_pipewire_zero_copy(
                    fd, node_id, fps.max(1), &presets.codec, max_res, mic, system_audio,
                    bitrate_kbps, audio_offset_ms, auto_device_compensation, &out_path,
                    stop.clone(), paused.clone(), &events, dims.clone(), &metadata,
                ) {
                    Ok(p) => Ok(p),
                    // Zero-copy declined/failed: fall back to the CPU path on the dup'd
                    // fd (the original was consumed). `stop` may have been set by the
                    // watchdog, so clear it for the fresh attempt. (Errors after a
                    // pause only reach here when NOTHING was recorded — a paused
                    // session with segments on disk salvages internally instead.)
                    Err(e) => match fallback_fd {
                        Some(cpu_fd) => {
                            eprintln!(
                                "zero-copy unavailable ({e}); using the readback path. \
                                 Frames go via RAM but are still hardware-encoded by ffmpeg"
                            );
                            stop.store(false, Ordering::Relaxed);
                            cpu(cpu_fd, stop)
                        }
                        None => Err(e),
                    },
                }
            } else {
                cpu(fd, stop)
            };
            #[cfg(not(feature = "zero-copy"))]
            let result = cpu(fd, stop);
            if let Ok(mut g) = done.lock() {
                *g = Some(result);
            }
        });
    }
    shared.into_handle()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn wait_or_kill_reaps_a_finished_child_cleanly() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let status = wait_or_kill(&mut child, Duration::from_secs(5)).unwrap();
        assert!(status.success());
    }

    #[test]
    fn wait_or_kill_kills_a_child_that_outlives_the_deadline() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let start = Instant::now();
        let status = wait_or_kill(&mut child, Duration::from_millis(200)).unwrap();
        assert!(!status.success(), "a killed child must not report success");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the wait must be bounded, not ride out the child"
        );
    }

    #[test]
    fn muxer_alive_needs_the_file_to_outgrow_the_header() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("cck-muxer-alive-test-{}", std::process::id()));
        assert!(!muxer_alive(&path), "missing file is not alive");
        std::fs::write(&path, vec![0u8; 971]).unwrap();
        assert!(!muxer_alive(&path), "a bare container header is not alive");
        std::fs::write(&path, vec![0u8; 64 * 1024]).unwrap();
        assert!(muxer_alive(&path), "a grown file is");
        let _ = std::fs::remove_file(&path);
    }

    // ── MuxerWatchdog: bounds a wedged muxer's first-frame write (DRAGON-118/123) ──
    // A `sleep` child stands in for a spawned ffmpeg (as the wait_or_kill tests do);
    // the watchdog kills it iff its "output" file didn't grow past the header in the
    // budget — the same signal that catches an n8.1.2 muxer that never drains stdin.
    #[test]
    fn muxer_watchdog_kills_a_muxer_that_never_grows_its_file() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let missing = std::env::temp_dir().join(format!("cck-wd-missing-{}", child.id()));
        let _ = std::fs::remove_file(&missing); // never grows → looks wedged
        let start = Instant::now();
        let wd = MuxerWatchdog::arm_for(child.id(), missing, Duration::from_millis(200), None);
        // wait_or_kill reaps it once the watchdog SIGKILLs it (well inside 5s).
        let status = wait_or_kill(&mut child, Duration::from_secs(5)).unwrap();
        assert!(!status.success(), "the watchdog should have killed the wedged stand-in");
        assert!(wd.fired(), "the watchdog must report that it fired");
        assert!(start.elapsed() < Duration::from_secs(3), "it must fire promptly (bounded)");
    }

    #[test]
    fn muxer_watchdog_spares_a_muxer_whose_file_grew() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let grown = std::env::temp_dir().join(format!("cck-wd-grown-{}", child.id()));
        std::fs::write(&grown, vec![0u8; 64 * 1024]).unwrap(); // past the header → alive
        let wd = MuxerWatchdog::arm_for(child.id(), grown.clone(), Duration::from_millis(150), None);
        std::thread::sleep(Duration::from_millis(500)); // let the budget elapse
        assert!(!wd.fired(), "a muxer that showed life must NOT be killed");
        assert!(child.try_wait().unwrap().is_none(), "the stand-in must still be running");
        wd.disarm();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&grown);
    }

    #[test]
    fn muxer_watchdog_pause_gate_defers_the_budget() {
        // Paused time must not count against the watchdog (the owned path feeds a
        // paused muxer nothing BY DESIGN — an early pause used to read as a wedge
        // and get the recording killed, the DRAGON-122-integration pause crash).
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let missing = std::env::temp_dir().join(format!("cck-wd-paused-{}", child.id()));
        let _ = std::fs::remove_file(&missing);
        let paused = Arc::new(AtomicBool::new(true)); // paused from the start
        let wd = MuxerWatchdog::arm_for(
            child.id(),
            missing,
            Duration::from_millis(200),
            Some(paused.clone()),
        );
        std::thread::sleep(Duration::from_millis(600));
        assert!(!wd.fired(), "a paused session must never trip the watchdog");
        assert!(child.try_wait().unwrap().is_none(), "the muxer must survive the pause");
        // Resume: the budget starts being spent only now, and fires (file never grew).
        paused.store(false, Ordering::Relaxed);
        let status = wait_or_kill(&mut child, Duration::from_secs(5)).unwrap();
        assert!(!status.success(), "after resume the un-alive muxer is still caught");
        assert!(wd.fired());
    }

    #[test]
    fn muxer_watchdog_disarm_prevents_the_kill() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let missing = std::env::temp_dir().join(format!("cck-wd-disarm-{}", child.id()));
        let _ = std::fs::remove_file(&missing);
        let wd = MuxerWatchdog::arm_for(child.id(), missing, Duration::from_millis(300), None);
        wd.disarm(); // disarm BEFORE the budget elapses
        std::thread::sleep(Duration::from_millis(600));
        assert!(!wd.fired(), "a disarmed watchdog must not fire");
        assert!(child.try_wait().unwrap().is_none(), "the child must survive a disarmed watchdog");
        let _ = child.kill();
        let _ = child.wait();
    }

    // `median_offset_ms` is the Linux PipeWire worker's latency reducer (cfg'd to linux).
    #[cfg(target_os = "linux")]
    #[test]
    fn median_offset_ms_needs_five_samples_and_clamps() {
        assert_eq!(median_offset_ms(vec![1, 2, 3, 4]), None);
        assert_eq!(median_offset_ms(vec![10, 20, 30, 40, 50]), Some(30));
        assert_eq!(median_offset_ms(vec![-5, -4, -3, -2, -1]), Some(0), "clamped up");
        assert_eq!(
            median_offset_ms(vec![5000, 6000, 7000, 8000, 9000]),
            Some(1000),
            "clamped down"
        );
    }

}
