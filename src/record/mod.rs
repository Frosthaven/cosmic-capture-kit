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
//! - [`RecordShared`] mints the stop/paused/done/events/measured/dims/warm_at/settled_at
//!   Arc set once and hands the worker its clone — there is no other way to build a
//!   [`RecordHandle`], so every entry point wires the same lifecycle.
//! - [`wait_or_kill`] / [`MuxerWatchdog`] make every wait BOUNDED (DRAGON-118): nothing
//!   in a stop tail can hang the recorder (and the preview spinner) forever.
//! - `owned::run_video_stop_tail` is the SINGLE stop-tail
//!   implementation the RGBA workers share (DRAGON-161): the covering ticks are
//!   non-blocking and bounded (DRAGON-711 — a FULL pipe is retried until
//!   `owned::COVERING_WRITE_BUDGET`, since those writes are what makes the video reach
//!   the audio's media end; only a dead pipe or an expired deadline stops them), the
//!   reap is bounded, and the salvage-vs-fail decision is the
//!   pure, unit-tested `owned::salvage_decision`. A worker becomes a thin capture
//!   adapter feeding this shared core, rather than re-deriving the media-clock teardown.
//!
//! A new platform implements: a `cfg`-gated `start_region_recording`, a capture
//! connection that feeds frames to the media-clock loop, and — reusing `record::owned`
//! and `record::pump` verbatim — the shared stop tail. It does NOT reimplement the
//! media clock, the FIFO/audio pre-flight, the watchdog, or the salvage logic.

// Only `bench_record` prints, and that is Linux-only (DRAGON-709).
#[cfg(target_os = "linux")]
use crate::errln;

#[cfg(target_os = "linux")]
use cosmic_client_toolkit::screencopy::{CaptureOptions, CaptureSource, ScreencopySessionData};
#[cfg(target_os = "linux")]
use cosmic_client_toolkit::sctk::shm::slot::SlotPool;
#[cfg(target_os = "linux")]
use crate::screencopy::{connect, grab_cropped, outputs, pick_format};
#[cfg(target_os = "linux")]
use std::io::Write;
// The portal `fd` only ever exists on the Linux capture path; off-Linux it is a
// never-constructed TYPE in `PipewireRecordParams` (dead_code-allowed there). Unix
// keeps `std::os::fd::OwnedFd`; Windows has no `std::os::fd`, so alias its owned
// handle so the struct field type resolves (DRAGON-229).
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(windows)]
use std::os::windows::io::OwnedHandle as OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

mod finalize;
// DRAGON-728: what the finalize pass is doing, so a long pass stops looking like a hang. The
// pass publishes; the app's existing recording poll reads. Portable and pure apart from one
// mutex, so every platform's suite covers the rule.
pub(crate) mod finalize_progress;
// DRAGON-691 amendment 2: compositing live annotation ink into a WINDOW recording, which is
// the one capture mode whose file cannot contain the on-screen overlay. Portable and pure
// apart from the rasterizer, so every platform's suite covers the mapping and the replay even
// though only macOS and Windows ever run the bake.
pub(crate) mod ink_bake;
// DRAGON-724: the same ink, composited onto the CAPTURED FRAME instead, which is what every
// other capture mode already gets from the compositor. The size gate, the raster and the blend
// are portable and unit-tested everywhere, so the Linux gate proves the rule macOS and Windows
// record through; `ink_bake` above stays as the fallback for a frame too big to paint per tick.
pub(crate) mod ink_live;
/// DRAGON-432: the bounded ring that keeps an ffmpeg child's last stderr lines, plus the
/// parser that picks the one line worth showing a user. Portable and pure — no spawning
/// here, so every platform's suite covers it.
pub(crate) mod ffmpeg_log;
/// DRAGON-713: the ONE bounded capture-to-encoder frame queue, with the ONE eviction rule
/// (drop the OLDEST, coalesce same-tick arrivals) every raw-frame worker shares. Portable and
/// pure apart from one mutex and a doorbell channel, so every platform's suite proves the rule
/// macOS and Windows record through.
pub(crate) mod frame_queue;
// The Wayland screencopy + portal PipeWire capture workers are the Linux capture
// paths; macOS records via ScreenCaptureKit (DRAGON-94 phase 3). pump/finalize/
// sync_probe are pure ffmpeg/audio and stay portable. `owned` holds the portable
// media-clock owned-path plumbing (frame writer + audio pre-flight) both the Linux
// workers and the macOS SCK worker reuse.
// `owned` holds the portable media-clock owned-path plumbing; enabled on Windows too
// (DRAGON-229 M3), where its POSIX-FIFO seams get named-pipe arms.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod owned;
#[cfg(target_os = "linux")]
mod pipewire;
// The SESSION-level bound (DRAGON-423): the outer ring around every in-process bound below
// — a recording that stops making PROGRESS ends itself instead of hanging forever. Portable
// (a pure state machine plus two `fs::metadata` calls); the app drives it from the poll it
// already runs, which is the one place in the process a wedged worker cannot block.
pub(crate) mod progress;
mod pump;
// Crash RECOVERY (DRAGON-421): the session ledger + the start-of-recording sweep that
// makes sure a session which died badly cannot poison the next one — and, for a session
// that HANGS rather than dies, `recover::abandon_session`, the complete teardown that
// DRAGON-423's bound calls for. Portable — plain `std::fs` over the same per-pid marker
// namespace `crate::instance` owns.
pub(crate) mod recover;
// The SCK worker lives PHYSICALLY under `platform/mac/` (the closed-platform
// split, DRAGON-226) but stays mounted at `record::sck` — module paths are
// stable via `#[path]`, exactly like the `platform/mod.rs` mount registry.
#[cfg(target_os = "macos")]
#[path = "../platform/mac/screencapturekit/record_worker.rs"]
mod sck;
// The WGC worker lives PHYSICALLY under `platform/windows/` (the closed split,
// DRAGON-229) but stays mounted at `record::wgc` — module paths stable via `#[path]`,
// exactly like `record::sck`.
#[cfg(windows)]
#[path = "../platform/windows/record_worker.rs"]
mod wgc;
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
// LIVE recording-opening proof (DRAGON-421) — `#[ignore]`-gated. Drives the REAL public
// entry point and verifies the finished file BY WHAT IT SAYS; see its module doc for why
// the shape-level `media_clock_e2e_tests` twin cannot catch what this catches.
#[cfg(all(test, target_os = "linux"))]
mod opening_live_tests;
mod sync_probe;
// LIVE proof of the session-level bound (DRAGON-423): the wedge is CAUSED with real ffmpeg
// processes and real FIFOs, and the whole response is run against it. Skips LOUDLY without
// ffmpeg (the `av_sync_tests` carve-out); unix-only — it is built on FIFOs and signals.
#[cfg(all(test, unix))]
mod wedge_live_tests;
#[cfg(feature = "zero-copy")]
mod zero_copy;
// "One user action starts ONE recording session" (DRAGON-422): drives the REAL
// `start_pipewire_recording` and counts audio pre-flights, which is what a zero-copy
// decline answered by a whole second session shows up as. Runs in the normal suite
// (no compositor needed — see its module doc on the dead portal fd); the one test that
// needs a working pre-flight loudly skips without a Pulse server.
#[cfg(all(test, target_os = "linux", feature = "zero-copy"))]
mod zc_fallback_live_tests;

/// ONE process-wide lock for every test that touches the recording pipeline's
/// process-globals: the `CCK_TEST_FORCE_OWNED_FAILURE` / `CCK_TEST_MONITOR_SOURCE`
/// env seams, the mic-source override, and the pre-flight counter read as a delta
/// (`owned::preflights_started`). `media_clock_e2e_tests` and
/// `zc_fallback_live_tests` used to carry their OWN locks for these same globals,
/// which is no lock at all ACROSS the two modules; the race was latent until
/// DRAGON-554's fast new pre-flight tests shifted the schedule and it fired: one
/// module's forced-failure env failed the other module's real pre-flight mid-test,
/// and that pre-flight inflated the first module's counter delta. Both modules'
/// `test_lock()` now delegate here.
// DRAGON-691 amendment 2 widened this from Linux-only to every test build: the ink-bake E2E
// tests run real ffmpeg on macOS and Windows too, and they must not overlap each other or the
// A/V sync tests any more than the Linux modules may.
#[cfg(test)]
pub(crate) fn recording_globals_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}
#[cfg(test)]
mod av_sync_tests;
// DRAGON-712: the owned STOP TAIL, end to end, through the real `encode::command` argv and
// real audio FIFOs. UNIX rather than Linux-only on purpose: the wedge it pins shipped
// because `media_clock_e2e_tests` is Linux-gated and macOS therefore had no behavioural
// cover on the recording muxer at all. Skips loudly without ffmpeg; needs no pulse server,
// no compositor and no display, so a container can run it.
#[cfg(all(test, unix))]
mod stop_tail_e2e_tests;
// DRAGON-691 amendment 2: the window-mode ink bake, proven against DECODED PIXELS rather than
// against the file's shape (CLAUDE.md's DRAGON-417 rule). Every host, because the bake's model
// and its ffmpeg wiring are portable even though only macOS and Windows ever run it; skips
// loudly without ffmpeg.
#[cfg(test)]
mod ink_bake_e2e_tests;
// The owned media-clock E2E tests exercise the Linux pipewire owned-audio path
// (and real ffmpeg); macOS records via SCK (DRAGON-94 phase 3), so gate to Linux.
#[cfg(all(test, target_os = "linux"))]
mod media_clock_e2e_tests;

pub(crate) use sync_probe::{measure_av_offset, write_sync_clip, SYNC_CLIP_NAME};
// DRAGON-229 M3: the isolated WGC single-frame grab behind `--test windows-wgc-frame`.
// DRAGON-426 adds the explicitly-budgeted form, for the window-capture ladder's WGC rescue
// (which runs inside the scene enumeration and cannot afford the generous default wait).
#[cfg(windows)]
pub(crate) use wgc::{
    capture_one_frame as wgc_capture_one_frame,
    capture_one_frame_within as wgc_capture_one_frame_within,
};
#[cfg(feature = "zero-copy")]
pub use zero_copy::screencopy_dmabuf_test;
#[cfg(all(target_os = "linux", feature = "zero-copy"))]
pub use zero_copy::cuda_import_test;

/// (Debug) End-to-end capture→encode of the FULL largest output at `fps` for
/// `secs` seconds (heaviest case), forcing a specific encoder `backend`
/// (`gpu`/`nvenc`, `cpu`/`vaapi`, `sw`). `--bench-record [secs] [fps] [backend]`.
#[cfg(target_os = "linux")]
pub fn bench_record(secs: u64, fps: u32, backend: &str) {
    use std::time::{Duration, Instant};
    let Some((conn, mut queue, mut data)) = connect(false) else {
        errln!("bench: connect failed");
        return;
    };
    let Some((output, name, _, (ow, oh))) = outputs(&data)
        .into_iter()
        .max_by_key(|(_, _, _, (w, h))| (*w as i64) * (*h as i64))
    else {
        errln!("bench: no output");
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
        errln!("bench: no capture formats reported");
        return;
    };
    let (cw, ch) = formats.buffer_size;
    let Some((format, swizzle, force_opaque)) = pick_format(&formats.shm_formats) else {
        errln!("bench: no usable shm format offered by the compositor");
        return;
    };
    let stride = cw * 4;
    let Ok(mut pool) = SlotPool::new((stride * ch) as usize, &data.shm) else {
        errln!("bench: shm pool allocation failed");
        return;
    };
    let Ok((buffer, _)) = pool.create_buffer(cw as i32, ch as i32, stride as i32, format) else {
        errln!("bench: shm buffer allocation failed");
        return;
    };
    let even = |v: u32| v - (v % 2);
    let (ew, eh) = (even(cw), even(ch));
    let temp = std::path::Path::new("/tmp/cck-bench-record.mkv");
    let Some(plan) = crate::encode::EncodePlan::for_backend(backend, ew, eh, &crate::encode::Presets::default()) else {
        errln!("bench: encoder backend '{backend}' not available on this system");
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
        errln!("bench: could not create the inert audio FIFOs");
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
        errln!("bench: ffmpeg spawn failed");
        audio_stop.store(true, Ordering::Relaxed);
        let _ = mic_writer.join();
        let _ = sys_writer.join();
        let _ = std::fs::remove_file(&mic_fifo_path);
        let _ = std::fs::remove_file(&sys_fifo_path);
        return;
    };
    let Some(stdin) = child.stdin.take() else {
        errln!("bench: ffmpeg stdin unavailable");
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
    errln!(
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
    // DRAGON-432: this reap was a bare `let _ = child.wait()`, so `--bench-record` was the one
    // record-path ffmpeg whose failure said nothing at all — the bench printed its fps line and
    // exited 0 over a muxer that had died. It does not go through `wait_or_kill` (a bench is
    // not a stop tail and has no budget to enforce), so it reports for itself.
    let pid = child.id();
    match child.wait() {
        Ok(s) if s.success() => ffmpeg_log::discard(pid),
        Ok(s) => {
            errln!("bench: ffmpeg exited with {s}");
            ffmpeg_log::report(pid, &format!("bench muxer exited with {s}"));
        }
        Err(e) => {
            errln!("bench: could not reap ffmpeg: {e}");
            ffmpeg_log::report(pid, "bench muxer could not be reaped");
        }
    }
    let _ = std::fs::remove_file(&mic_fifo_path);
    let _ = std::fs::remove_file(&sys_fifo_path);
    errln!(
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
    /// The wall instant the worker confirmed a genuinely real first captured frame, which is
    /// also the instant it starts its audio pre-flight. `None` if stopped before that point.
    ///
    /// DRAGON-657: the WARMUP signal. A recording is not truly under way until the
    /// capture stream has actually delivered a frame, so the workers do their video bring-up
    /// FIRST and start audio only once this is filled. The strict order costs the bring-up's
    /// latency (roughly 250-600ms) at every start, deliberately: it is what makes "recording"
    /// mean recording. Filled by EVERY worker on every platform since DRAGON-658 brought the
    /// three Linux ones (screencopy, PipeWire and both zero-copy arms) onto the same shape.
    ///
    /// It is NOT media 0 and it is NOT what the UI reads. It was both, once: media 0 until
    /// DRAGON-672 (which moved it to [`Self::settled_at`]) and the elapsed anchor until
    /// DRAGON-673 (which moved that to the same place, because the worker now starts at
    /// countdown START and this fires a whole countdown before the file does). What remains
    /// here is the worker's own account of when capture became real, which is the first half
    /// of every bring-up investigation this pair has been used for. Its ONE reader is the
    /// app's bring-up log line (`App::adopt_warm_start`), which reports the span from here to
    /// media 0: the pre-flight, the ffmpeg spawn, the opening ticks and any countdown hold.
    pub warm_at: Arc<Mutex<Option<std::time::Instant>>>,
    /// MEDIA 0: the wall instant the worker anchored the session's media clock, which is
    /// where the delivered file begins. `None` until the worker gets there.
    ///
    /// DRAGON-725: the slot exists because the app was reading media 0 off
    /// [`Self::settled_at`], and the two are not the same instant. Filled at every
    /// `owned::media_zero(...)` call site, on every worker and every platform, in the same
    /// breath as the clock itself, so nothing can be off by a step: whatever this says is
    /// what `MediaClock::new` was handed.
    ///
    /// THIS is the elapsed readout's anchor and the clock every live ink stroke is stamped
    /// against, because both are counting the FILE. `settled_at` is a later, separate
    /// question ("may the chip offer STOP yet"), and reading media 0 off it made every
    /// baked ink mark lead the cursor that drew it by the whole opening phase, 500ms on the
    /// owner's take, and made the readout at stop undercount the file by the same amount.
    pub media_zero_at: Arc<Mutex<Option<std::time::Instant>>>,
    /// The wall instant the worker's OPENING PHASE ended: the first moment the whole
    /// pipeline is genuinely up and level with the media clock, with capture confirmed, the
    /// audio pre-flight done, ffmpeg spawned, the pump running and the opening's media debt
    /// paid. `None` if the session ended before that point.
    ///
    /// DRAGON-661: the SETTLED signal, the later of this pair. [`Self::warm_at`] fires at
    /// the START of the bring-up and real work still follows it. On one measured mac
    /// session the first frame landed at +1651ms and this at +2251ms, with the mic device
    /// setup (~190ms), the ffmpeg spawn and the opening catch-up filling the 600ms between
    /// them; up to a third of that opening's ticks were repeats of one frame, because
    /// capture cannot keep pace with a loop that is racing the clock. A UI that says "live"
    /// at `warm_at` is therefore pointing straight at the span still being papered over.
    ///
    /// THIS IS NOT MEDIA 0, and it never reliably was (DRAGON-725). It is the LIVE
    /// DECLARATION and only that: the record chip's STOP face and the tray's "Recording"
    /// state. Media 0 is [`Self::media_zero_at`], which is strictly earlier, and anything
    /// counting the FILE (the elapsed readout, the ink clock) must read that one.
    ///
    /// This doc used to say "this one IS media 0", on the reasoning that the worker takes
    /// it immediately after `owned::media_zero` and so lands a few milliseconds later. The
    /// opening phase sits between the two, and its length is not ours to choose: it ends
    /// when a video write returns inside one tick period, and ffmpeg paces the video pipe
    /// against the audio timestamps it has already received, so the span is set by another
    /// process's muxer scheduler. Measured span, media 0 to here, `debug test mac-rec-bench
    /// largest 5 videotoolbox 30 1245`, n=8 per build: 328ms median before the v0.34.0
    /// batch and 480ms after it. See `owned::mark_settled` for the whole argument, for the
    /// levers that were tried and made it WORSE, and for each call site.
    pub settled_at: Arc<Mutex<Option<std::time::Instant>>>,
    /// DRAGON-691 amendment 2: the live annotation ink to composite into a WINDOW recording,
    /// filled by the app just before it asks the recording to stop.
    ///
    /// The same contract as `events` beside it, and for the same reason: the UI is where the
    /// user's actions happen and the worker is where the file is finished, so the UI records
    /// what happened and the worker bakes it in at the end. `None` means "nothing to
    /// composite", which is every recording that is not an inked window one, and finalize
    /// then builds exactly the command it always did.
    ///
    /// Read ONCE, at finalize, after the stop tail. Nothing in the record path waits on it.
    ///
    /// DRAGON-724: on a session the live composite took (`ink_live::live_ink_affordable`), the
    /// worker DROPS whatever is here, because the ink is already in the frames. The app fills it
    /// either way and does not have to know which path ran: that is deliberate, and it is what
    /// makes the fallback need no coordination at all.
    pub ink: Arc<Mutex<Option<crate::record::ink_bake::InkBake>>>,
    /// DRAGON-724: the live annotation scene, republished by the app whenever it changes, so the
    /// capture worker can paint it onto each frame on its way to the encoder.
    ///
    /// The OPPOSITE contract to `ink` above, and they are siblings on purpose. That one is
    /// written once at the end and read once at finalize; this one is written all through the
    /// session and read per tick. A worker that paints from this leaves the finalize pass
    /// stream-copying its video, which is the whole point of the ticket.
    ///
    /// Empty on every recording that is not an inked WINDOW recording whose capture reads the
    /// window's own buffer, and nothing in the record path waits on it. Which recordings those
    /// are is a question about the BACKEND and not about the platform (DRAGON-736): the Linux
    /// PORTAL workers are fed this too, while the Linux NATIVE screencopy worker is not, and
    /// both are reachable in the same build.
    pub live_ink: crate::record::ink_live::LiveInk,
    /// DRAGON-693, the BRONZE tier: a one-shot request for the freshest captured frame, so the
    /// armed annotation window can show what the recording is actually seeing.
    ///
    /// Deliberately request-driven rather than a slot the worker keeps fresh. A 4K frame is
    /// 33MB, and copying one into a mutex on every tick would cost a live recording real
    /// bandwidth to serve a window that exists for a few seconds of a session, if at all. The
    /// app raises the flag when it arms a tool and the worker answers ONCE.
    pub peek: FramePeek,
}

/// A one-shot request for the freshest captured frame (DRAGON-693).
///
/// The Bronze tier's whole need from the record path: a no-layer-shell WINDOW cast streams the
/// recorded window's own pixels to us and nothing else knows where that window is, so the
/// armed overlay draws on the last frame the recording saw. Nothing in the record path waits
/// on this and no bound is involved: a worker that never answers costs the annotation its
/// backdrop and costs the recording nothing at all.
#[derive(Clone)]
pub struct FramePeek {
    /// Raised by the app, lowered by the worker when it serves one.
    want: Arc<AtomicBool>,
    /// The served frame. Drained by the app.
    frame: Arc<Mutex<Option<PeekedFrame>>>,
}

/// One served frame: `(width, height, tightly-packed RGBA)`.
pub type PeekedFrame = (u32, u32, Vec<u8>);

impl FramePeek {
    fn new() -> Self {
        Self { want: Arc::new(AtomicBool::new(false)), frame: Arc::new(Mutex::new(None)) }
    }

    /// Ask the worker for one frame. Idempotent: a second ask before the first is served
    /// changes nothing.
    pub fn request(&self) {
        self.want.store(true, Ordering::Relaxed);
    }

    /// Take the served frame, if one has arrived. Non-blocking and poison-tolerant, the house
    /// drain idiom.
    pub fn take(&self) -> Option<PeekedFrame> {
        self.frame.lock().ok().and_then(|mut g| g.take())
    }

    /// Worker side: is a frame wanted? Clears the request, so exactly one frame is copied per
    /// ask however often this is called.
    ///
    /// Its ONE caller is the Linux PipeWire worker, which is the only capture path a Bronze
    /// session can run on, so it is honestly dead everywhere else.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn wanted(&self) -> bool {
        self.want.swap(false, Ordering::Relaxed)
    }

    /// Worker side: publish one frame. Overwrites anything undrained, which is right: the app
    /// wants the freshest, and a stale one it never picked up is of no use to it.
    ///
    /// Dead off Linux for the same reason [`FramePeek::wanted`] is.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn serve(&self, w: u32, h: u32, rgba: &[u8]) {
        if let Ok(mut g) = self.frame.lock() {
            *g = Some((w, h, rgba.to_vec()));
        }
    }
}

/// Guarantees a recording's `done` slot is filled even if the worker thread unwinds
/// (panics) or returns early without posting a result. Held for the worker's lifetime;
/// on drop it fills an error only if nothing was posted. Without this, a worker that dies
/// silently leaves `done` empty forever and the UI's `RecordingPoll` waits on it — so the
/// preview stays stuck on its spinner (DRAGON-106).
struct DoneGuard(Arc<Mutex<Option<Result<PathBuf, String>>>>);
impl Drop for DoneGuard {
    fn drop(&mut self) {
        // DRAGON-421: this is THE "the recording worker is over" seam — it already exists
        // precisely because it fires on the paths where nothing else does (a panic, an
        // early return). Nothing the crash-recovery ledger names is live wreckage once the
        // worker has stopped, so drop it here rather than at a happy-path call site that a
        // dying worker would skip.
        recover::clear_session();
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
    let pid = child.id();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                // DRAGON-432: every record-path ffmpeg is reaped here, so this is the one
                // place that can report what it said without touching each worker — including
                // the feature-gated zero-copy ones, which cannot be compiled on every dev
                // machine. A healthy exit drops the ring silently; anything else writes the
                // tail to the debug log.
                if s.success() {
                    ffmpeg_log::discard(pid);
                } else {
                    ffmpeg_log::report(pid, &format!("exited with {s}"));
                }
                return Ok(s);
            }
            Ok(None) => {}
            Err(e) => {
                ffmpeg_log::report(pid, "could not be reaped");
                return Err(e);
            }
        }
        if std::time::Instant::now() >= deadline {
            log::warn!("ffmpeg still running {timeout:?} after its inputs closed; killing it");
            let _ = child.kill();
            // Killed after outliving its budget: whatever it last printed is the best
            // evidence for WHY it would not exit. Report AFTER the reap, not before — `kill`
            // is asynchronous, and while the child is still dying its stderr pipe is still
            // open and the drain is still reading. Reporting first raced the drain and printed
            // a ring missing exactly the lines that explain the kill.
            let status = child.wait();
            ffmpeg_log::report(pid, "killed after outliving its stop budget");
            return status;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// What one poll of [`wait_or_kill_progressing`] concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReapStep {
    /// The output grew. Adopt this as the new high-water mark and restart the stall clock.
    Progressed(u64),
    /// No growth yet, and the stall budget still has room.
    Waiting,
    /// The output has not grown for the whole stall budget. This one is wedged.
    Stalled,
}

/// **Pure**, unit-tested (DRAGON-716): one poll of the progress-aware reap
/// [`wait_or_kill_progressing`] runs.
///
/// `mark` is the largest output size seen so far, `reading` what the probe just returned,
/// and `since_growth` how long it has been since the mark last moved.
///
/// Three edges are deliberate and are pinned by the tests below:
///
/// * **Growth beats a spent budget.** An output that grows on the very poll where the
///   budget runs out is progressing, not stalled, so the growth arm is checked FIRST.
/// * **Equal is not growth** (`>`, not `>=`). A file that stops changing size is exactly
///   the symptom being watched for, so a repeated reading must not refresh the clock.
/// * **A SHRINKING reading is not growth either.** `-movflags +faststart` rewrites the
///   file at the end to move the moov atom, and nothing promises the size climbs
///   monotonically while it does. Treating a dip as progress would hand a wedged process
///   a free extension; treating it as a stall would kill an honest one, which is why the
///   budget is generous rather than the comparison being clever.
pub(crate) fn reap_step(
    mark: u64,
    reading: u64,
    since_growth: std::time::Duration,
    stall: std::time::Duration,
) -> ReapStep {
    if reading > mark {
        return ReapStep::Progressed(reading);
    }
    if since_growth < stall { ReapStep::Waiting } else { ReapStep::Stalled }
}

/// [`wait_or_kill`]'s sibling for a pass whose HONEST duration scales with the work
/// (DRAGON-716): reap `child`, killing it only once `probe` has reported no growth for
/// `stall`.
///
/// # Why a second reaper exists
///
/// [`wait_or_kill`] bounds a pass whose remaining work is roughly CONSTANT: a muxer
/// flushing the backlog it already holds. Its budget is a wall-clock cap, and for that
/// shape the cap IS the wedge detector.
///
/// The ink finalize is not that shape. It re-encodes the whole recording in software
/// (`finalize::finalize_ink_args`: a filtered stream cannot be stream-copied), so its
/// duration scales with recording length times frame area. A constant cap over that is not
/// a wedge detector at all, it is a LENGTH LIMIT, and it was one: measured at
/// 22.1s for a 2 minute 1080p recording on a quiet 16-core desktop, so a ~2.7 minute take
/// crossed the old 30s cap there, and a laptop, a Retina-sized window or any load crossed
/// it far sooner. Past the cap `wait_or_kill` SIGKILLed the encode and the non-success
/// status became an `Err`, so the recording was LOST after the user had already watched a
/// spinner for the full budget.
///
/// # Why this is still bounded
///
/// DRAGON-118's rule is that nothing in the record path waits unboundedly, and this keeps
/// it, by asking a different question with the SAME number: "has the output stopped
/// growing for `stall`?" rather than "has `stall` elapsed?". A wedged ffmpeg writes
/// nothing and still dies in one budget. An honest encode that keeps writing now finishes.
/// This is the question `MuxerWatchdog` and [`muxer_alive`] already ask of the live muxer,
/// so it is the house pattern rather than a new mechanism.
///
/// The total duration is therefore bounded by the WORK rather than by a clock, exactly as
/// `progress::SESSION_STALL_SECS` is. A process that dribbles one byte just inside every
/// budget would run on, and that is accepted for the same reason it is accepted there:
/// the alternative kills honest work, and nothing has ever been seen to do it.
///
/// `probe` is injected rather than taking a path, so the decision above is testable
/// without a filesystem and this loop carries the only syscall.
///
/// **`probe` must be MONOTONIC for the life of one child**, and that is the caller's job, not
/// this loop's. The high-water mark exists so a momentary dip cannot hand a wedged process a
/// free extension, which means a LEGITIMATE reset reads as a stall: a second attempt writing
/// to a path the first attempt left partial would sit under the dead attempt's mark for most
/// of its run and be killed for it. `finalize::run_ink_pass` therefore deletes its output
/// before every attempt. A caller that cannot make that guarantee wants a different bound.
pub(crate) fn wait_or_kill_progressing(
    child: &mut std::process::Child,
    stall: std::time::Duration,
    mut probe: impl FnMut() -> u64,
) -> std::io::Result<std::process::ExitStatus> {
    let pid = child.id();
    let mut mark = probe();
    let mut last_growth = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                // Same reporting contract as `wait_or_kill`: a healthy exit drops the
                // stderr ring silently, anything else writes its tail to the debug log.
                if s.success() {
                    ffmpeg_log::discard(pid);
                } else {
                    ffmpeg_log::report(pid, &format!("exited with {s}"));
                }
                return Ok(s);
            }
            Ok(None) => {}
            Err(e) => {
                ffmpeg_log::report(pid, "could not be reaped");
                return Err(e);
            }
        }
        match reap_step(mark, probe(), last_growth.elapsed(), stall) {
            ReapStep::Progressed(n) => {
                mark = n;
                last_growth = std::time::Instant::now();
            }
            ReapStep::Waiting => {}
            ReapStep::Stalled => {
                log::warn!(
                    "ffmpeg wrote nothing to its output for {stall:?} while being reaped; \
                     killing it as wedged (DRAGON-716)"
                );
                let _ = child.kill();
                // Report AFTER the reap, for `wait_or_kill`'s reason: `kill` is
                // asynchronous, and reporting first races the stderr drain and prints a
                // ring missing exactly the lines that explain the kill.
                let status = child.wait();
                ffmpeg_log::report(pid, "killed after its output stopped growing");
                return status;
            }
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
                #[cfg(unix)]
                if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
                #[cfg(windows)]
                {
                    // DRAGON-229 M3: OpenProcess(PROCESS_TERMINATE) + TerminateProcess — the
                    // SIGKILL analog for a wedged WGC-recording ffmpeg whose first-frame
                    // write is blocking the recorder (body under platform/windows/).
                    crate::platform::windows::terminate_pid(pid);
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
///
/// The finalize pass DELETES this once it has delivered, and only then (DRAGON-748,
/// `finalize::finalize_with_ink`); a finalize that failed leaves it for the DRAGON-118 salvage
/// and for `recover`. Nothing else needs to clean up after a healthy recording, which is why
/// the three `debug test` rec benches used to leak one of these per run.
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
    warm_at: Arc<Mutex<Option<std::time::Instant>>>,
    media_zero_at: Arc<Mutex<Option<std::time::Instant>>>,
    settled_at: Arc<Mutex<Option<std::time::Instant>>>,
    ink: Arc<Mutex<Option<crate::record::ink_bake::InkBake>>>,
    live_ink: crate::record::ink_live::LiveInk,
    peek: FramePeek,
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
            warm_at: Arc::new(Mutex::new(None)),
            media_zero_at: Arc::new(Mutex::new(None)),
            settled_at: Arc::new(Mutex::new(None)),
            ink: Arc::new(Mutex::new(None)),
            live_ink: crate::record::ink_live::LiveInk::new(),
            peek: FramePeek::new(),
        }
    }

    /// A second Arc clone of the set, moved into the worker thread's closure.
    fn clone_for_worker(&self) -> Self {
        Self {
            stop: self.stop.clone(),
            paused: self.paused.clone(),
            done: self.done.clone(),
            events: self.events.clone(),
            measured_offset_ms: self.measured_offset_ms.clone(),
            dims: self.dims.clone(),
            warm_at: self.warm_at.clone(),
            media_zero_at: self.media_zero_at.clone(),
            settled_at: self.settled_at.clone(),
            ink: self.ink.clone(),
            live_ink: self.live_ink.clone(),
            peek: self.peek.clone(),
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
            warm_at: self.warm_at,
            media_zero_at: self.media_zero_at,
            settled_at: self.settled_at,
            ink: self.ink,
            live_ink: self.live_ink,
            peek: self.peek,
        }
    }
}

/// Encode + destination settings shared by both recording entry points — everything
/// downstream of "what pixels are we capturing" (that part differs per entry point;
/// see [`RegionRecordParams`] / [`PipewireRecordParams`]).
pub struct RecordSettings {
    pub fps: u32,
    /// The encoder REQUEST: "auto" (travels as "auto" so the worker's hint-first
    /// ladder resolves it fresh each session, DRAGON-571) or a concrete user pick.
    pub preferred_encoder: String,
    /// The last-known-good auto hint accompanying an "auto" request (`None` for a
    /// concrete pick): the ladder probes it first, so the happy path pays one
    /// probe-encode. See [`resolve_session_plan`].
    pub encoder_hint: Option<String>,
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
    /// The COUNTDOWN GATE (DRAGON-673): a flag the app RAISES when it promotes the
    /// recording, i.e. at the instant the UI starts claiming to record, or `None` when there
    /// is no countdown. The worker holds media 0 until it is raised, so a countdown warms
    /// the whole pipeline behind its own digits and the file still starts exactly where the
    /// promotion does.
    ///
    /// A flag rather than the countdown-zero INSTANT, which is what this carried first: the
    /// app's prediction of zero (stamped as the countdown began) ran ~1s ahead of the tick
    /// that actually fires the capture, so the file started before the UI said "Recording".
    /// See `owned::media_zero` for the measurement and why a signal cannot drift.
    pub start_gate: Option<Arc<AtomicBool>>,
}

/// Resolve a recording session's encode plan through the DRAGON-571 hint-first auto
/// ladder ([`crate::encode::EncodePlan::resolve_hinted`]), then apply the resolution's
/// write-backs in ONE place for every worker (screencopy, PipeWire, SCK, WGC): warn
/// when a previously-good hardware encoder just lost to software, and update the
/// persisted last-known-good hint. The hint is a cache; `preferred_encoder` itself is
/// never written here: only a real settings-picker click writes that.
pub(crate) fn resolve_session_plan(
    requested: &str,
    hint: Option<&str>,
    ew: u32,
    eh: u32,
    presets: &crate::encode::Presets,
) -> crate::encode::EncodePlan {
    let plan = crate::encode::EncodePlan::resolve_hinted(requested, hint, ew, eh, presets);
    let outcome = crate::encode::auto_resolution_outcome(requested, plan.encoder_id(), hint);
    if outcome.hardware_hint_failed {
        // The DRAGON-571 regression signature: hardware that worked before failed its
        // probe THIS session (driver asleep, NVENC sessions held by a game or overlay,
        // the ffmpeg sidecar missing the encoder). Nothing gets pinned (the next
        // session re-probes from scratch), but this recording runs on CPU, so say so
        // loudly instead of letting the stutter be the first symptom.
        log::warn!(
            "auto encoder resolution fell back to software: the last-known-good hardware \
             encoder ({}) failed its probe this session; this recording is CPU-encoded, \
             and the next recording re-probes from scratch",
            hint.unwrap_or("?"),
        );
    }
    if let Some(id) = outcome.new_hint {
        crate::state::note_encoder_auto_hint(&id);
    }
    plan
}

/// Rewrite a recording's metadata line so its `encoder=` and `preset=` fields name the tier
/// the session ACTUALLY resolved to, plus that tier's speed preset. Pure, unit-tested.
///
/// The sibling of `finalize::annotate_codec`, which does the same thing for `codec=auto`,
/// and for the same reason: the app builds the line at spawn time, and some of what the line
/// claims is not decided until the worker has resolved its plan.
///
/// **Windows only, and that is the point** (DRAGON-754). Everywhere else the app resolves the
/// encoder itself, cheaply: Linux reads `/dev/nvidia0` and `/dev/dri`, macOS reads the ffmpeg
/// encoder list. Windows has no device node to sniff, so its honest gate is a real
/// probe-ENCODE per tier, and `encode::device::available_encoders` runs one for every tier
/// there is. That walk was happening on the UI thread, inside the record control's own click
/// handler, purely to name the encoder in this line: measured on the owner's box at 1.1 to
/// 1.3s warm and about 2.35s cold, which is the freeze the user reported. So the Windows app
/// writes the REQUEST here (`auto`, or the tier the user picked, both free) and the WGC worker
/// overwrites it with the tier it resolved, on its own thread, from the probe the recording
/// was going to pay for anyway.
///
/// Writing the request and stopping there was the other option and it is the worse one: this
/// field is read back by `debug inspect`, and DRAGON-718 already paid for a metadata field
/// that reported an intention rather than an event. A missed annotation degrades to the
/// request rather than to nothing, which is why the app writes that rather than a placeholder.
///
/// The rewrite is TOTAL and order-independent: it replaces whatever those two fields hold,
/// leaves every other field byte-identical, and is a no-op on a line that carries neither.
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn annotate_session_encoder(
    metadata: &str,
    encoder: &str,
    presets: &crate::encode::Presets,
) -> String {
    let preset = crate::encode::session_preset_label(encoder, presets);
    metadata
        .split(" | ")
        .map(|field| match field.split_once('=') {
            Some(("encoder", _)) => format!("encoder={encoder}"),
            Some(("preset", _)) => format!("preset={preset}"),
            _ => field.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// DRAGON-754: the metadata rewrite that lets a Windows recording start without walking the
/// encoder probe ladder on the UI thread.
#[cfg(test)]
mod annotate_session_encoder_tests {
    use super::annotate_session_encoder;
    use crate::encode::Presets;

    fn presets() -> Presets {
        Presets {
            nvenc: "p5".to_string(),
            x264: "veryfast".to_string(),
            vaapi_cl: 3,
            codec: "auto".to_string(),
        }
    }

    const LINE: &str = "Cosmic Capture Kit | type=video | source=cosmic | mode=monitor | \
                        encoder=auto | preset=- | codec=auto | zero_copy=off | \
                        max_res=original | fps=30 | bitrate=8000k | audio=mic";

    #[test]
    fn the_resolved_tier_replaces_the_request_in_both_fields() {
        let out = annotate_session_encoder(LINE, "nvenc", &presets());
        assert!(out.contains("| encoder=nvenc |"), "{out}");
        assert!(out.contains("| preset=p5 |"), "{out}");
        assert!(!out.contains("encoder=auto"), "{out}");
    }

    /// The Windows tiers with no preset namespace of their own keep the dash, so a
    /// `requested=mf` session reads exactly as it did when the app resolved the field.
    #[test]
    fn a_preset_less_tier_keeps_the_dash() {
        let out = annotate_session_encoder(LINE, "mf", &presets());
        assert!(out.contains("| encoder=mf |"), "{out}");
        assert!(out.contains("| preset=- |"), "{out}");
    }

    /// A concrete request that the ladder had to abandon: the file must say what RAN, which
    /// is the whole reason the worker gets to write this field.
    #[test]
    fn a_fallback_to_software_is_recorded_as_software() {
        let asked = LINE.replace("encoder=auto", "encoder=qsv");
        let out = annotate_session_encoder(&asked, "software", &presets());
        assert!(out.contains("| encoder=software |"), "{out}");
        assert!(out.contains("| preset=veryfast |"), "{out}");
    }

    #[test]
    fn every_other_field_survives_byte_identical() {
        let out = annotate_session_encoder(LINE, "nvenc", &presets());
        for field in LINE.split(" | ") {
            if field.starts_with("encoder=") || field.starts_with("preset=") {
                continue;
            }
            assert!(out.contains(field), "lost {field} from {out}");
        }
        // Same field count, so nothing was merged or dropped.
        assert_eq!(out.split(" | ").count(), LINE.split(" | ").count());
    }

    #[test]
    fn a_line_with_neither_field_is_returned_unchanged() {
        let plain = "Cosmic Capture Kit | type=video | fps=30";
        assert_eq!(annotate_session_encoder(plain, "nvenc", &presets()), plain);
        assert_eq!(annotate_session_encoder("", "nvenc", &presets()), "");
    }
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

/// The Windows WGC capture target (DRAGON-229 M3) — the Windows analog of
/// [`MacRecordTarget`]. On Windows every record mode reaches [`start_region_recording`]
/// through [`RegionRecordParams`]; this says which WGC content item the video worker
/// attaches to:
///
/// - [`Region`](Self::Region): a CROP of the most-overlapped monitor (a
///   `CopySubresourceRegion` on a `CreateForMonitor` item).
/// - [`Window`](Self::Window): a specific window by `HWND` decimal id, via WGC's
///   `CreateForWindow` — occluding windows are NOT recorded and capture follows the
///   window as it moves.
/// - [`Display`](Self::Display): a whole monitor (monitor mode) by `\\.\DISPLAYn` name —
///   the `CreateForMonitor` item at full bounds, no crop.
///
/// Windows-only: the field carrying it is `#[cfg(windows)]`, so the Linux/mac workers never
/// see it and their construction sites stay byte-identical.
#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WinRecordTarget {
    /// Record the `x/y/w/h` rect as a crop of the most-overlapped monitor.
    Region,
    /// Record a single window by its `HWND` decimal id (occlusion-independent).
    Window(String),
    /// Record the whole monitor named `\\.\DISPLAYn` (monitor mode).
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
    /// The Windows WGC capture target (region crop / window / display). Windows-only —
    /// the Linux/mac workers never see this field, so their construction sites stay
    /// byte-identical (DRAGON-229).
    #[cfg(windows)]
    pub win_target: WinRecordTarget,
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
        fps, preferred_encoder, encoder_hint, presets, zero_copy, mic, system_audio,
        bitrate_kbps, audio_offset_ms, auto_device_compensation, max_res, metadata, out_path, start_gate,
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
        let RecordShared {
            stop, paused, done, events, dims, warm_at, media_zero_at, settled_at, ink, live_ink, ..
        } =
            shared.clone_for_worker();
        std::thread::spawn(move || {
            let _done_guard = DoneGuard(done.clone());
            let result = sck::record_sck(
                x, y, w, h, fps.max(1), cursor, &preferred_encoder, encoder_hint.as_deref(),
                &presets, mic, system_audio, bitrate_kbps, audio_offset_ms,
                auto_device_compensation, max_res, start_gate, &out_path, stop, paused, &events, &dims,
                &warm_at, &media_zero_at, &settled_at, &ink, &live_ink, &metadata, mac_target,
            );
            if let Ok(mut g) = done.lock() {
                *g = Some(result);
            }
        });
    }
    shared.into_handle()
}

/// Start recording a region — unsupported stand-in for platforms with no capture
/// backend (neither Linux screencopy/PipeWire, macOS ScreenCaptureKit, nor Windows WGC).
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn start_region_recording(params: RegionRecordParams) -> RecordHandle {
    let _ = params;
    unsupported_recording("screen recording is not supported on this platform")
}

/// Start recording a region (global physical coords) to `out_path` at `fps` on Windows
/// via Windows.Graphics.Capture (DRAGON-229 M3): the WGC video worker (`record::wgc`)
/// drives the SAME media-clock owned pipeline the Linux/mac workers do (audio pre-flight →
/// `spawn_ffmpeg_media_clock` → `pump` → finalize). Mirrors the mac
/// `start_region_recording`'s handle/threading so the app's recording state machine
/// (poll/stop/preview) works unchanged. Window/Monitor modes reach here as a region rect
/// too (the app funnels every mode through `RegionRecordParams`); `win_target` selects the
/// WGC item.
#[cfg(windows)]
pub fn start_region_recording(params: RegionRecordParams) -> RecordHandle {
    let RegionRecordParams { x, y, w, h, cursor, win_target, settings } = params;
    let RecordSettings {
        fps, preferred_encoder, encoder_hint, presets, zero_copy, mic, system_audio,
        bitrate_kbps, audio_offset_ms, auto_device_compensation, max_res, metadata, out_path, start_gate,
    } = settings;
    // No GPU zero-copy path on Windows: libx264 encodes inside ffmpeg from the RGBA feed
    // (the Linux DRM/DMA-BUF zero-copy path has no Windows analog).
    let _ = zero_copy;
    let shared = RecordShared::new();
    {
        // `measured_offset_ms` is intentionally not passed to `record_wgc`: the worker feeds
        // ffmpeg synchronously on its own thread with no delivery channel to measure lag on
        // (same reasoning as the Linux screencopy / mac SCK paths).
        let RecordShared {
            stop, paused, done, events, dims, warm_at, media_zero_at, settled_at, ink, live_ink, ..
        } =
            shared.clone_for_worker();
        std::thread::spawn(move || {
            let _done_guard = DoneGuard(done.clone());
            let result = wgc::record_wgc(
                x, y, w, h, fps.max(1), cursor, &preferred_encoder, encoder_hint.as_deref(),
                &presets, mic, system_audio, bitrate_kbps, audio_offset_ms,
                auto_device_compensation, max_res, start_gate, &out_path, stop, paused, &events, &dims,
                &warm_at, &media_zero_at, &settled_at, &ink, &live_ink, &metadata, win_target,
            );
            if let Ok(mut g) = done.lock() {
                *g = Some(result);
            }
        });
    }
    shared.into_handle()
}

/// Start recording a region (global logical coords) to `out_path` at `fps`.
/// `mic`/`system_audio` are the channels' initial on/off state at t=0.
#[cfg(target_os = "linux")]
pub fn start_region_recording(params: RegionRecordParams) -> RecordHandle {
    let RegionRecordParams { x, y, w, h, cursor, settings } = params;
    let RecordSettings {
        fps, preferred_encoder, encoder_hint, presets, zero_copy, mic, system_audio,
        bitrate_kbps, audio_offset_ms, auto_device_compensation, max_res, metadata, out_path, start_gate,
    } = settings;
    let shared = RecordShared::new();
    {
        // `measured_offset_ms` is intentionally not passed to `record_screencopy`: the
        // owned screencopy loop grabs and writes synchronously with no delivery channel,
        // so it has no lag figure to measure (see `record_screencopy_owned`'s doc).
        let RecordShared {
            stop, paused, done, events, dims, warm_at, media_zero_at, settled_at, ..
        } =
            shared.clone_for_worker();
        std::thread::spawn(move || {
            let _done_guard = DoneGuard(done.clone());
            let result = screencopy::record_screencopy(
                x, y, w, h, fps.max(1), cursor, &preferred_encoder, encoder_hint.as_deref(),
                &presets, mic, system_audio, bitrate_kbps, audio_offset_ms,
                auto_device_compensation, max_res, start_gate, &out_path, stop, paused, &events, &dims,
                &warm_at, &media_zero_at, &settled_at, &metadata, zero_copy,
            );
            if let Ok(mut g) = done.lock() {
                *g = Some(result);
            }
        });
    }
    shared.into_handle()
}

/// Whether a zero-copy DECLINE (`ZcOutcome::Fallback`, i.e. nothing was recorded) may be
/// answered by starting the session over on the CPU path: `Ok(())` to run it, `Err(msg)`
/// to fail the recording with `msg` — the user-facing reason, since it is repeated
/// verbatim into the alert (`app::failure::alert_message`, `Failure::RecordingFailed`).
///
/// PURE so this truth table is pinned by tests rather than re-derived by inspection.
///
/// ## The field failure this exists for (DRAGON-422)
///
/// One user action produced two recording sessions. The zero-copy attempt started, its
/// portal stream never delivered a dmabuf frame, and four seconds later the user pressed
/// stop — which set this same `stop` flag and opened the preview's loading spinner. The
/// attempt then declined, and the fallback CLEARED the flag ("it may have been set by our
/// watchdog") and started a whole second recording: a new audio pre-flight, a new pump, a
/// new muxer, all behind a preview the user was already looking at, with no stop button
/// left anywhere in the UI and nothing that would ever set the flag again. It was still
/// recording a minute later. Because that second session never finished, `RecordHandle.done`
/// was never filled, so the app's own failure path (`update_recording`'s `Err` arm →
/// `diag::note_failure` + `App::fail_session`) never ran either, and the spinner stayed up
/// for good.
///
/// Hence the rule: a stop that has already been asked for is never overridden, and it is
/// never cleared. The zero-copy worker's own aborts trip a private flag now (see
/// `zero_copy::record_pipewire_zero_copy_owned`), so a set flag here means the SESSION was
/// stopped, and the only honest answer to "record this" is to say it did not happen.
#[cfg(all(target_os = "linux", feature = "zero-copy"))]
fn zero_copy_fallback(reason: &str, stop_requested: bool, have_fd: bool) -> Result<(), String> {
    if stop_requested {
        return Err(format!(
            "the GPU capture path could not record ({reason}), and the recording was \
             stopped before the fallback could take over"
        ));
    }
    if !have_fd {
        // The portal fd could not be duplicated at start, so there is nothing left to
        // connect the CPU path with (a portal fd cannot be re-opened).
        return Err(reason.to_string());
    }
    Ok(())
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
        fps, preferred_encoder, encoder_hint, presets, zero_copy, mic, system_audio,
        bitrate_kbps, audio_offset_ms, auto_device_compensation, max_res, metadata, out_path, start_gate,
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
        // DRAGON-736: `ink` and `live_ink` reach the PORTAL workers now, and only them. A
        // portal WINDOW cast delivers the toplevel's own buffer, so our ink surface is not in
        // it and has to be composited in exactly as it is on macOS and Windows. The NATIVE
        // screencopy destructuring above deliberately still takes neither: that path crops the
        // SCREEN, so the compositor has already put the ink in the frame.
        let RecordShared {
            stop, paused, done, events, measured_offset_ms: measured, dims, warm_at,
            media_zero_at, settled_at, ink, live_ink, peek, ..
        } = shared.clone_for_worker();
        std::thread::spawn(move || {
            let _done_guard = DoneGuard(done.clone());
            let paused_cpu = paused.clone();
            let ink_cpu = ink.clone();
            let live_ink_cpu = live_ink.clone();
            let warm_cpu = warm_at.clone();
            let media_zero_cpu = media_zero_at.clone();
            let settled_cpu = settled_at.clone();
            // The countdown gate is shared, not consumed: a zero-copy attempt that declines
            // hands the whole recording to the CPU path, which must hold for the same
            // promotion (by then the gate is normally already open, and reading it again
            // costs nothing).
            let start_gate_cpu = start_gate.clone();
            let cpu = |fd: OwnedFd, stop: Arc<AtomicBool>| {
                pipewire::record_pipewire(
                    fd, node_id, crop, fps.max(1), &preferred_encoder, encoder_hint.as_deref(),
                    &presets, mic, system_audio, bitrate_kbps, audio_offset_ms,
                    auto_device_compensation, max_res, start_gate_cpu, &out_path, stop, paused_cpu, &events,
                    &measured, &dims, &warm_cpu, &media_zero_cpu, &settled_cpu, &ink_cpu,
                    &live_ink_cpu, &metadata, &peek,
                )
            };
            #[cfg(feature = "zero-copy")]
            let result = if try_zero_copy {
                match zero_copy::record_pipewire_zero_copy(
                    fd, node_id, fps.max(1), &presets.codec, max_res, start_gate, mic,
                    system_audio, bitrate_kbps, audio_offset_ms, auto_device_compensation,
                    &out_path, stop.clone(), paused.clone(), &events, dims.clone(), &warm_at,
                    &media_zero_at, &settled_at, &ink, &metadata,
                ) {
                    // The session ran (or failed on its own terms): its result IS the
                    // recording's result. Never retried — see `ZcOutcome`.
                    zero_copy::ZcOutcome::Done(r) => r,
                    // Zero-copy declined and nothing was recorded. Fall back to the CPU
                    // path on the dup'd fd (the original was consumed) — but only while
                    // the session is still wanted; see `zero_copy_fallback` for what
                    // "still wanted" means and why this no longer touches `stop`.
                    // (Errors after a pause only reach here when NOTHING was recorded —
                    // a paused session with segments on disk salvages internally.)
                    zero_copy::ZcOutcome::Fallback(e) => {
                        let stopped = stop.load(Ordering::Relaxed);
                        match (zero_copy_fallback(&e, stopped, fallback_fd.is_some()), fallback_fd)
                        {
                            (Ok(()), Some(cpu_fd)) => {
                                log::warn!(
                                    "zero-copy unavailable ({e}); using the readback path. \
                                     Frames go via RAM but are still hardware-encoded by ffmpeg"
                                );
                                cpu(cpu_fd, stop)
                            }
                            (Err(reason), _) => {
                                log::warn!(
                                    "zero-copy unavailable ({e}); not falling back: {reason}"
                                );
                                Err(reason)
                            }
                            // Unreachable by construction — `zero_copy_fallback` only
                            // says yes when a fallback fd exists, and it is told so by
                            // the very `Option` matched here. Spelled out rather than
                            // unwrapped: nothing in a recording worker should panic.
                            (Ok(()), None) => Err(e),
                        }
                    }
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
    // DRAGON-229: only the not(windows) process-management tests below use these;
    // the portable `muxer_alive` test does not, so gate the import to match.
    #[cfg(not(target_os = "windows"))]
    use std::time::{Duration, Instant};

    // ── The zero-copy fallback's truth table (DRAGON-422) ───────────────────
    //
    // The rule these pin: a fallback may only ever start a session that has not been
    // stopped. See `zero_copy_fallback`'s doc for the field failure.

    #[cfg(all(target_os = "linux", feature = "zero-copy"))]
    #[test]
    fn a_plain_decline_falls_back_to_the_cpu_path() {
        assert!(zero_copy_fallback("no dmabuf frames", false, true).is_ok());
    }

    #[cfg(all(target_os = "linux", feature = "zero-copy"))]
    #[test]
    fn a_decline_after_a_stop_fails_instead_of_recording_again() {
        let e = zero_copy_fallback("no dmabuf frames", true, true)
            .expect_err("a stopped session must never be started over");
        // The reason is repeated verbatim into the user's alert, so it has to say both
        // halves: what could not record, and why nothing took over.
        assert!(e.contains("no dmabuf frames"), "keeps the cause: {e}");
        assert!(e.contains("stopped"), "says the recording was stopped: {e}");
    }

    #[cfg(all(target_os = "linux", feature = "zero-copy"))]
    #[test]
    fn a_stop_wins_even_when_a_fallback_fd_is_available() {
        // The dup'd portal fd is what MAKES a second session possible, so this is the
        // exact combination the field failure hit. Having the means is not a reason.
        for have_fd in [true, false] {
            assert!(
                zero_copy_fallback("declined", true, have_fd).is_err(),
                "have_fd={have_fd}: a requested stop must be final"
            );
        }
    }

    #[cfg(all(target_os = "linux", feature = "zero-copy"))]
    #[test]
    fn a_decline_with_nothing_to_fall_back_on_reports_the_cause() {
        // The portal fd could not be duplicated at start and cannot be re-opened, so
        // there is no CPU path to run; the zero-copy reason is the whole diagnosis.
        let e = zero_copy_fallback("no VAAPI device for zero-copy", false, false).unwrap_err();
        assert_eq!(e, "no VAAPI device for zero-copy");
    }

    // DRAGON-229: these process-management tests spawn a Unix `true`/`sleep` child
    // (absent on Windows → "program not found") and exercise the watchdog SIGKILL,
    // which is a Windows M0 no-op (recording is off there until M3). Gated off Windows;
    // Linux + macOS run them unchanged. The `muxer_alive` FS test below stays portable.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn wait_or_kill_reaps_a_finished_child_cleanly() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let status = wait_or_kill(&mut child, Duration::from_secs(5)).unwrap();
        assert!(status.success());
    }

    #[cfg(not(target_os = "windows"))] // DRAGON-229: Unix `sleep` child; see the group note above
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

    // DRAGON-716: the same two questions, asked of the PROGRESS-aware reaper. The probe is
    // injected here rather than pointed at a file, which is the whole reason
    // `wait_or_kill_progressing` takes a closure: the loop becomes deterministic and fast
    // instead of waiting on a real encoder.

    /// THE regression. This child outlives the budget many times over, and must survive,
    /// because it is still writing. Under the constant cap this replaces it was SIGKILLed
    /// and the recording was lost.
    #[cfg(not(target_os = "windows"))] // DRAGON-229: Unix `sleep` child; see the group note above
    #[test]
    fn wait_or_kill_progressing_rides_out_a_child_that_keeps_writing() {
        let mut child = std::process::Command::new("sleep").arg("1").spawn().unwrap();
        let mut grown = 0u64;
        let status = wait_or_kill_progressing(&mut child, Duration::from_millis(100), || {
            grown += 4096;
            grown
        })
        .unwrap();
        assert!(
            status.success(),
            "a child that is still writing must be reaped cleanly, not killed at a clock"
        );
    }

    /// And the wedge still dies in one budget, so DRAGON-118 holds: no growth, no mercy.
    #[cfg(not(target_os = "windows"))] // DRAGON-229: Unix `sleep` child; see the group note above
    #[test]
    fn wait_or_kill_progressing_kills_a_child_that_stops_writing() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let start = Instant::now();
        let status =
            wait_or_kill_progressing(&mut child, Duration::from_millis(200), || 4096).unwrap();
        assert!(!status.success(), "a killed child must not report success");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "a stalled pass must still be bounded, not ride out the child"
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
    #[cfg(not(target_os = "windows"))] // DRAGON-229: Unix `sleep` child + watchdog SIGKILL; see the group note
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

    #[cfg(not(target_os = "windows"))] // DRAGON-229: Unix `sleep` child; see the group note
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

    #[cfg(not(target_os = "windows"))] // DRAGON-229: Unix `sleep` child + watchdog SIGKILL; see the group note
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

    #[cfg(not(target_os = "windows"))] // DRAGON-229: Unix `sleep` child; see the group note
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

/// The progress-aware reap's one decision (DRAGON-716).
///
/// The rule these pin: a pass whose honest duration scales with the recording is bounded
/// by having STOPPED WRITING, never by a wall clock, because a wall clock over that shape
/// is a length limit that destroys long recordings.
#[cfg(test)]
mod reap_step_tests {
    use super::{ReapStep, reap_step};
    use std::time::Duration;

    const STALL: Duration = Duration::from_secs(30);

    /// The ordinary case the old constant budget got wrong: a long encode that keeps
    /// writing is progressing however long it has been running in total.
    #[test]
    fn a_growing_output_keeps_the_pass_alive_forever() {
        // Well past any wall-clock budget, and still not stalled, because it GREW.
        assert_eq!(
            reap_step(1_000, 2_000, Duration::from_secs(600), STALL),
            ReapStep::Progressed(2_000)
        );
    }

    /// Growth is checked FIRST, so an output that moves on the very poll where the budget
    /// runs out is progressing rather than killed by a one-poll race.
    #[test]
    fn growth_beats_a_budget_that_has_just_run_out() {
        assert_eq!(reap_step(1_000, 1_001, STALL, STALL), ReapStep::Progressed(1_001));
        assert_eq!(
            reap_step(1_000, 1_001, STALL + Duration::from_secs(5), STALL),
            ReapStep::Progressed(1_001)
        );
    }

    /// A repeated reading is the symptom being watched for, so it must NOT refresh the
    /// clock. `>` and not `>=`.
    #[test]
    fn an_unchanged_size_is_not_progress() {
        assert_eq!(reap_step(1_000, 1_000, Duration::ZERO, STALL), ReapStep::Waiting);
        assert_eq!(reap_step(1_000, 1_000, STALL, STALL), ReapStep::Stalled);
    }

    /// `-movflags +faststart` rewrites the file to move the moov atom, and nothing
    /// promises the size climbs monotonically while it does. A dip must not be read as
    /// progress and hand a wedged process a free extension.
    ///
    /// This is also the reason `finalize::run_ink_pass` DELETES its output before every
    /// attempt. A retry whose ffmpeg truncates a partial file back to zero would sit under
    /// the dead attempt's high-water mark for most of its run and be killed for it, so the
    /// software fallback would be destroyed by the very bound meant to protect it. The
    /// contract is on the caller, and this test is why.
    #[test]
    fn a_shrinking_output_is_not_progress() {
        assert_eq!(reap_step(9_000, 4_000, Duration::ZERO, STALL), ReapStep::Waiting);
        assert_eq!(reap_step(9_000, 4_000, STALL, STALL), ReapStep::Stalled);
        // The retry shape specifically: a fresh child starting from zero under a stale mark.
        assert_eq!(reap_step(50_000_000, 0, STALL, STALL), ReapStep::Stalled);
        assert_eq!(reap_step(50_000_000, 5_000_000, STALL, STALL), ReapStep::Stalled);
    }

    /// The budget edge, in the same direction `owned::keep_waiting_for_pipe` uses: strictly
    /// inside the budget waits, and reaching it stalls.
    #[test]
    fn the_budget_edge_is_exclusive() {
        assert_eq!(reap_step(0, 0, STALL - Duration::from_millis(1), STALL), ReapStep::Waiting);
        assert_eq!(reap_step(0, 0, STALL, STALL), ReapStep::Stalled);
    }

    /// A wedge still dies in ONE budget, which is what keeps DRAGON-118 true: an output
    /// that never grows at all reaches `Stalled` on nothing but the clock.
    #[test]
    fn an_output_that_never_grows_still_gets_killed() {
        assert_eq!(reap_step(0, 0, Duration::from_secs(29), STALL), ReapStep::Waiting);
        assert_eq!(reap_step(0, 0, Duration::from_secs(31), STALL), ReapStep::Stalled);
    }

    /// The first probe of a file that does not exist yet reads zero, and a muxer that then
    /// writes its header is progressing. Pinned because the reaper seeds its high-water
    /// mark from exactly that reading.
    #[test]
    fn a_first_byte_after_an_absent_file_is_progress() {
        assert_eq!(reap_step(0, 1_041, Duration::from_secs(1), STALL), ReapStep::Progressed(1_041));
    }
}
