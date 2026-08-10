//! Portable media-clock OWNED-path plumbing shared by every capture worker
//! (DRAGON-130 phase 3): the frame-writer closure, the audio pre-flight check, and
//! the FIFO/smoke-check helpers. Relocated VERBATIM out of `record::pipewire`
//! (which is `#[cfg(target_os = "linux")]`) so the macOS SCK worker
//! (`record::sck`) can reuse the exact same audio pre-flight + frame-writer logic
//! without depending on the Linux-only PipeWire module. Nothing here is
//! platform-specific — it is pure ffmpeg/FIFO/audio-chain composition, identical on
//! Linux and macOS.

use crate::audio::capture::{CaptureChunk, MonitorCapture};
use crate::audio::clean_mic::MicTapHandle;
use crate::audio::filters::StreamTap;
use image::RgbaImage;
use std::io::Write;
use std::path::PathBuf;
use std::process::ChildStdin;
use std::time::Duration;

/// One channel's mute intervals (media seconds) — the same shape
/// `finalize::off_intervals` produces. Named so the owned workers'
/// `std::thread::scope` return types don't trip clippy's `type_complexity`.
pub(super) type MuteIntervals = Vec<(f64, f64)>;

/// Build the frame-writer closure the media-clock owned single-session loop
/// (DRAGON-125) uses: convert to NV12 on the hardware path (or pass RGBA through)
/// and write to `stdin` at the STREAM size. Shared so the conversion logic (and its
/// "never resize on the capture path" invariant) has exactly one definition.
/// Returns `false` once the pipe is closed. `pub(super)`: the screencopy owned path
/// (DRAGON-127, `record::screencopy`) and the macOS SCK path (`record::sck`) reuse
/// this exact closure instead of re-deriving the NV12/write logic.
pub(super) fn make_frame_writer(
    w0: u32,
    h0: u32,
    nv12: bool,
) -> impl FnMut(u32, u32, &[u8], &mut std::process::ChildStdin) -> bool {
    let (w0s, h0s) = (w0 as usize, h0 as usize);
    let mut nv12buf = vec![0u8; w0s * h0s * 3 / 2];
    move |w: u32, h: u32, rgba: &[u8], stdin: &mut std::process::ChildStdin| {
        // DRAGON-277: a capture frame whose byte length is SHORT of its reported `w*h*4` dims
        // would OOB-panic `rgba_to_nv12` (a band thread indexes past `rgba`) and — because
        // that panic crosses the scoped-thread join — take the WHOLE app down mid-recording.
        // That is the Windows "recording crashes instead of preview opening" on the hardware
        // (NV12) encode path (software passes RGBA through and never converts). Drop any such
        // malformed frame and keep the recording alive. `RgbaImage::from_raw` already guards
        // the resize branch; this also guards the same-size direct-use branch below. Portable
        // (a well-formed frame always has `>= w*h*4` bytes, so Linux/macOS are unaffected).
        if rgba.len() < (w as usize) * (h as usize) * 4 {
            log::warn!(
                "record: dropping malformed frame ({}x{}, {} bytes < {} expected)",
                w,
                h,
                rgba.len(),
                (w as usize) * (h as usize) * 4
            );
            return true;
        }
        let resized;
        let rgba = if (w, h) != (w0, h0) {
            match RgbaImage::from_raw(w, h, rgba.to_vec()) {
                Some(img) => {
                    resized =
                        image::imageops::resize(&img, w0, h0, image::imageops::FilterType::Triangle)
                            .into_raw();
                    &resized[..]
                }
                None => return true, // malformed frame; skip, keep going
            }
        } else {
            rgba
        };
        let out = if nv12 {
            crate::encode::rgba_to_nv12(rgba, w0s, h0s, &mut nv12buf);
            &nv12buf[..]
        } else {
            rgba
        };
        stdin.write_all(out).is_ok()
    }
}

/// A scoped `O_NONBLOCK` toggle on ffmpeg's stdin for the stop tail's covering-tick
/// writes (DRAGON-160): sets the pipe non-blocking on construction and restores the
/// original flags on drop. Portable (Linux + macOS) — the fd/`fcntl` calls are POSIX,
/// and this guards EVERY owned worker's stop tail through [`run_video_stop_tail`], not
/// just the macOS SCK path it was born on.
///
/// The wedge this closes (reproduced headlessly — see the `nonblocking_*` tests in
/// [`super::sck`]): the stop tail feeds the FINAL covering video ticks
/// (`VideoTicker::ticks_to_cover`) to ffmpeg's stdin AFTER the pump has EOF'd the audio
/// FIFOs. Under `-shortest`, an ffmpeg whose audio was the shorter stream decides it is
/// DONE and stops draining its stdin video pipe WITHOUT closing the read end — so a
/// plain blocking `write_all` of even one frame parks in the kernel forever (a
/// full-monitor RGBA frame is tens of MB, dwarfing the ~64K pipe). With the write hung,
/// the worker never reaches `wait_or_kill`, never fills `RecordHandle.done`, and the
/// preview spinner spins forever (`DoneGuard` can't fire — the thread is alive, not
/// unwound). SIGKILLing ffmpeg does NOT reliably unblock a write already parked in the
/// kernel (measured live), so the fix is to never block.
///
/// While this guard is active, a `write_all` to a FULL pipe returns `WouldBlock`
/// immediately, so the covering-tick loop stops instead of hanging. Restoring blocking
/// mode on drop keeps the subsequent `drop(stdin)` (EOF) and any later error semantics
/// normal.
#[cfg(unix)]
pub(super) struct NonblockingStdin {
    /// The raw fd (owned by the `ChildStdin` the caller still holds — this only borrows
    /// it as an integer, so it does NOT conflict with the caller's `&mut stdin` writes).
    raw: std::os::fd::RawFd,
    restore: Option<rustix::fs::OFlags>,
}

#[cfg(unix)]
impl NonblockingStdin {
    pub(super) fn new(stdin: &ChildStdin) -> Self {
        use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
        use std::os::fd::AsRawFd;
        let raw = stdin.as_raw_fd();
        // SAFETY: `stdin` outlives this guard (the caller holds it for the whole
        // covering-tick loop and drops it only afterwards), so `raw` stays valid.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) };
        let restore = fcntl_getfl(borrowed).ok();
        if let Some(flags) = restore {
            let _ = fcntl_setfl(borrowed, flags | OFlags::NONBLOCK);
        }
        Self { raw, restore }
    }
}

#[cfg(unix)]
impl Drop for NonblockingStdin {
    fn drop(&mut self) {
        if let Some(flags) = self.restore {
            // SAFETY: same validity reasoning as `new` — the owning `ChildStdin` is
            // still alive when this guard drops (it is dropped only after).
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.raw) };
            let _ = rustix::fs::fcntl_setfl(borrowed, flags);
        }
    }
}

/// Windows (DRAGON-229 M3): the SAME scoped stdin-non-blocking guard, implemented with
/// `PIPE_NOWAIT` on ffmpeg's stdin (body under `platform/windows/named_pipe.rs`). Aliased
/// to `NonblockingStdin` so [`run_video_stop_tail`] names one type on both platforms.
#[cfg(windows)]
pub(super) use crate::platform::windows::named_pipe::StdinNoWait as NonblockingStdin;

/// The pure salvage-vs-fail decision every owned worker's stop tail ends with: given
/// how ffmpeg reaped (`wait_result`), whether the muxer was declared wedged
/// (`muxer_wedged` — the watchdog/liveness kill, meaning it never wrote real output),
/// and whether the temp grew past the bare container header (`temp_alive`,
/// i.e. [`super::muxer_alive`]), decide whether the recording SALVAGES (`Ok`) or FAILS
/// (`Err` with a named reason). Split out as a pure function so its exact truth table
/// is unit-tested once instead of re-derived by inspection in four workers.
///
/// The rule (DRAGON-125 field wedge): a clean exit with no wedge succeeds; a
/// stop-tail-only death (ffmpeg killed at the very end) that nonetheless left a
/// structurally sound mkv on disk is SALVAGED through finalize (mkv needs no trailer;
/// `-flush_packets 1` keeps the on-disk state packet-granular); a truly wedged muxer
/// (never wrote output) stays fatal — nothing worth keeping.
pub(super) fn salvage_decision(
    wait_result: &std::io::Result<std::process::ExitStatus>,
    muxer_wedged: bool,
    temp_alive: bool,
) -> Result<(), String> {
    match wait_result {
        Ok(s) if s.success() && !muxer_wedged => Ok(()),
        bad => {
            if !muxer_wedged && temp_alive {
                Ok(())
            } else if muxer_wedged {
                Err("recording failed: ffmpeg accepted frames but never wrote output \
                     (wedged muxer was killed)"
                    .to_string())
            } else {
                match bad {
                    Ok(s) => Err(format!("ffmpeg exited with {s}")),
                    Err(e) => Err(e.to_string()),
                }
            }
        }
    }
}

/// The shared stop tail every RGBA owned worker (screencopy / PipeWire / SCK) runs once
/// the capture loop and pump have finished: feed the FINAL covering video ticks
/// NON-BLOCKING, close the stdin (EOF), kill a wedged muxer, reap ffmpeg BOUNDED
/// (DRAGON-118), and return the pure [`salvage_decision`] — logging the same salvage
/// warning the workers used to inline. Extracting it here (DRAGON-161) makes the
/// bounded covering-write behavior — born macOS-only in [`NonblockingStdin`]
/// (DRAGON-160) — the SINGLE implementation, so Linux gets it too.
///
/// `covering` is called `count` times, each writing ONE covering frame to `stdin`; it
/// returns `false` once a write fails. A covering-write failure is BENIGN under
/// `-shortest` (ffmpeg already took all the video it wants; the pipe non-blocking means
/// a full pipe returns `WouldBlock`), so — unlike the old blocking Linux loop, which
/// set `muxer_wedged` on failure and then DELETED the temp — it merely stops the loop
/// and never marks the muxer wedged. `count`/`covering` are skipped entirely when
/// `muxer_wedged` is already set (a wedged muxer is being killed regardless).
///
/// Returns `Ok(())` to salvage/finalize the temp, `Err(reason)` to have already
/// removed it and fail the recording (the caller does the `remove_file`).
pub(super) fn run_video_stop_tail(
    stdin: ChildStdin,
    child: &mut std::process::Child,
    temp: &std::path::Path,
    muxer_wedged: bool,
    count: u32,
    mut covering: impl FnMut(&mut ChildStdin) -> bool,
) -> Result<(), String> {
    let mut stdin = stdin;
    if !muxer_wedged {
        // Non-blocking for the covering ticks: a stalled ffmpeg (audio was the shorter
        // `-shortest` stream, so it stopped draining video without closing the pipe)
        // yields `WouldBlock` and the loop stops, instead of an unbounded kernel park.
        let _nb = NonblockingStdin::new(&stdin);
        for _ in 0..count {
            if !covering(&mut stdin) {
                break;
            }
        }
    }
    drop(stdin); // EOF -> ffmpeg flushes and exits
    if muxer_wedged {
        let _ = child.kill();
    }
    // Bounded reap (DRAGON-118): a healthy ffmpeg flushes its backlog and exits well
    // inside this; one that survives it is wedged and gets killed.
    let pid = child.id();
    let wait_result = super::wait_or_kill(child, Duration::from_secs(30));
    let temp_alive = super::muxer_alive(temp);
    // DRAGON-432: `wait_or_kill` has already written any stderr tail to the debug log; this
    // picks up the ONE line worth showing a user. `salvage_decision`'s own strings say only
    // what WE observed from outside ("ffmpeg exited with …"), and ffmpeg's own sentence is
    // usually the whole answer — it reaches the dialog because `RecordingFailed` is the one
    // failure whose detail is shown verbatim.
    let outcome = salvage_decision(&wait_result, muxer_wedged, temp_alive)
        .map_err(|e| super::ffmpeg_log::with_hint(e, super::ffmpeg_log::take_hint(pid)));
    match &outcome {
        Ok(()) if !matches!(&wait_result, Ok(s) if s.success() && !muxer_wedged) => {
            // Salvaged a stop-tail-only death (the temp is sound; see `salvage_decision`).
            log::warn!(
                "recording muxer had to be killed at stop ({wait_result:?}); salvaging the \
                 written temp into a finalized recording"
            );
        }
        Err(_) => {
            let _ = std::fs::remove_file(temp);
        }
        _ => {}
    }
    outcome
}

/// Test-only override for the owned path's system-monitor source (env
/// `CCK_TEST_MONITOR_SOURCE`) — lets a headless E2E harness point [`MonitorCapture`]
/// at a null-sink's monitor (success path) or a nonexistent source (forced-failure
/// path) without touching the user's default sink. Unset in normal operation,
/// where `None` resolves to the default sink's monitor.
fn test_monitor_source_override() -> Option<String> {
    std::env::var("CCK_TEST_MONITOR_SOURCE").ok().filter(|s| !s.trim().is_empty())
}

/// Test-only forced-failure seam for the E2E proof that a failed pre-flight check
/// produces a named, actionable error (env `CCK_TEST_FORCE_OWNED_FAILURE`):
/// pointing [`MonitorCapture`] at an unrecognized source name turned out NOT to be
/// a reliable "make it fail" mechanism on this platform — measured live,
/// PipeWire's pulse-compat layer silently falls back to the DEFAULT source for a
/// name it doesn't recognize instead of erroring (`pa_stream_connect_record`
/// still succeeds). This flag short-circuits [`try_start_owned_audio`] to `Err`
/// directly instead, which is what a genuinely failed pre-flight check looks
/// like from every caller's perspective.
fn test_force_owned_failure() -> bool {
    std::env::var("CCK_TEST_FORCE_OWNED_FAILURE").is_ok_and(|v| !v.trim().is_empty())
}

/// How long the pre-flight check waits for each owned audio source's FIRST item
/// before giving up on it (see [`try_start_owned_audio`]). Measured on a cold/
/// suspended PulseAudio/PipeWire-Pulse sink, `MonitorCapture`'s own connect +
/// stream-negotiate + first-chunk delivery took ~2s (its 300ms settle sleep is a
/// small fraction of that; most of it is the sink waking from SUSPENDED and the
/// stream's initial buffering) — generous margin over that keeps a healthy but
/// momentarily slow-to-wake device from being misread as "the owned path can't
/// start" and falling back needlessly.
const OWNED_AUDIO_SMOKE_BUDGET: std::time::Duration = std::time::Duration::from_millis(4000);

/// Whether `rx` delivers at least one item within `budget` — the pre-flight smoke
/// check both owned audio sources must pass. A source pointed at a bad device
/// connects/spawns successfully (its constructor already returned `Some`) but then
/// either produces nothing before disconnecting (pulse/ffmpeg reject a bad device
/// fast) or never produces anything at all — either way this returns `false`.
///
/// Consumes (loses) at most one real item on success. Since DRAGON-417 that item is
/// the source's FIRST, so the loss sits at media 0 rather than being absorbed
/// somewhere harmless — but it is one block (10-25ms), silence-filled by the mixer
/// and counted in `MixerStats::gap_samples` like any other hole. Handing it on
/// instead would mean threading two more values through every worker's
/// `pump::spawn` call, on platforms this tree cannot build; the trade is deliberate,
/// and recorded here so it is a known 25ms and not a mystery.
fn smoke_check<T>(rx: &std::sync::mpsc::Receiver<T>, budget: std::time::Duration) -> bool {
    rx.recv_timeout(budget).is_ok()
}

/// Create a FIFO at `path`, clearing any stale one a prior crash left behind.
/// POSIX-only: Windows has no `mkfifo` — its audio transport is named pipes (created in
/// [`try_start_owned_audio`]'s `#[cfg(windows)]` arm via
/// `crate::platform::windows::named_pipe::create_pipe_server`).
#[cfg(not(windows))]
pub(super) fn mkfifo(path: &std::path::Path) -> bool {
    let _ = std::fs::remove_file(path);
    #[cfg(not(target_os = "macos"))]
    {
        rustix::fs::mkfifoat(rustix::fs::CWD, path, rustix::fs::Mode::from_bits_truncate(0o600))
            .is_ok()
    }
    // rustix's `mkfifoat` is unavailable on Apple targets; use the plain POSIX
    // `libc::mkfifo` (same result: a 0600 FIFO at `path`). Additive per-platform
    // branch — the Linux path above is untouched.
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(cpath) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) == 0 }
    }
}

/// Everything the media-clock owned path's pre-flight check starts before deciding
/// to commit to it: both output FIFOs (paths only — opened later, by the pump,
/// AFTER ffmpeg is spawned; see `super::pump::spawn`'s doc), the clean-mic tap
/// chain, and the system-monitor capture. Call [`cleanup`](Self::cleanup)
/// explicitly if this is abandoned instead of handed to a worker's owned session —
/// a bare `Drop` can't call `MonitorCapture::stop` (which consumes `self`; see
/// `crate::audio::capture`'s doc for why that type has no `Drop` of its own).
/// `pub(super)`: the DRAGON-125 E2E harness (`record::media_clock_e2e_tests`, a
/// sibling module under `record`) drives this same pre-flight check directly (no
/// live portal is available headlessly, so it cannot go through `record_pipewire`
/// itself).
pub(super) struct OwnedAudioStart {
    /// When the session's audio capture began — the instant this pre-flight was
    /// entered, which is within a millisecond of the worker thread's own start and
    /// therefore of the app marking itself "recording"
    /// (`App::start_screencopy_recording` / `start_held_pipewire_recording` set
    /// `recording_started` in the same breath as spawning the worker).
    ///
    /// This is the session's MEDIA TIME 0 (DRAGON-417). Every owned worker passes it
    /// straight to [`super::pump::spawn`] rather than stamping a fresh `Instant::now()`
    /// once its own video side is ready: audio is already being captured from this
    /// instant, and anything captured before media 0 is silently thrown away by the
    /// mixer (it lands at a negative media position). Anchoring at the later
    /// video-ready instant is exactly what discarded the opening seconds of a
    /// recording — the user speaks into a live-looking indicator while the pipeline
    /// quietly drops what it hears. The video side COVERS the resulting opening span
    /// instead: `VideoTicker::due_video_ticks` already returns however many ticks the
    /// span is worth, and the worker writes that many, so the recording is honest in
    /// both streams.
    ///
    /// WHAT those covering ticks show is the worker's own call, and it is not free
    /// (DRAGON-628). The Linux and Windows workers repeat the first captured frame,
    /// which reads as a frozen opening; `record::sck` on macOS instead spends them on
    /// the frames its capture had already delivered and was discarding, which halved
    /// the measured frozen head. If you touch a worker's covering burst, the rule is
    /// that the tick COUNT is the contract and the pixels are not: changing the count
    /// changes media time, changing the pixels does not.
    pub(super) capture_start: std::time::Instant,
    pub(super) mic_fifo_path: PathBuf,
    pub(super) sys_fifo_path: PathBuf,
    pub(super) mic_tap: MicTapHandle,
    pub(super) mic_rx: std::sync::mpsc::Receiver<StreamTap>,
    pub(super) monitor: MonitorCapture,
    pub(super) sys_rx: std::sync::mpsc::Receiver<CaptureChunk>,
    /// Windows (DRAGON-229 M3): the named-pipe SERVER ends created at pre-flight (the
    /// `\\.\pipe\…` paths above are their client-facing names, passed to ffmpeg). Handed
    /// to `pump::spawn`, which `ConnectNamedPipe`s them on the pump thread — the analog of
    /// the POSIX `open_fifo_write_end`. On POSIX the FIFOs are ordinary filesystem paths,
    /// so no handle is carried.
    #[cfg(windows)]
    pub(super) mic_pipe: crate::platform::windows::named_pipe::PipeServer,
    #[cfg(windows)]
    pub(super) sys_pipe: crate::platform::windows::named_pipe::PipeServer,
}

impl OwnedAudioStart {
    pub(super) fn cleanup(self) {
        let _ = self.monitor.stop(); // bounded ≤2s (DRAGON-118)
        drop(self.mic_tap); // bounded drain, its own Drop impl
        // POSIX FIFOs are filesystem paths to unlink; Windows named pipes are destroyed by
        // dropping their server handles (the `#[cfg(windows)]` `mic_pipe`/`sys_pipe` fields
        // drop with `self`), so there is nothing to remove.
        #[cfg(not(windows))]
        {
            let _ = std::fs::remove_file(&self.mic_fifo_path);
            let _ = std::fs::remove_file(&self.sys_fifo_path);
        }
    }
}

/// Attempt to start every OWNED-path audio component (the recorder's pre-flight
/// check, DRAGON-127): both FIFOs, the clean-mic tap chain, and the system-monitor
/// capture — each ALREADY producing at least one real item within
/// [`OWNED_AUDIO_SMOKE_BUDGET`]. `Err` with a short, specific reason (naming which
/// of pulse connection / FIFO / mic chain failed) if any of these don't come up —
/// there is no fallback path any more; the caller turns this straight into the
/// recording's failure. Nothing about the video capture is touched here, so a
/// failure here never risks a portal `fd` — provably so, by construction: this
/// function doesn't even TAKE an `fd`/`node_id` parameter.
/// How many audio pre-flights this process has STARTED (see [`try_start_owned_audio`]).
///
/// Doubles as the per-call token in the FIFO/pipe names and as the honest answer to "how
/// many recording sessions did that one user action start?" — the question DRAGON-422
/// turned on, and one a process list can only answer by inference.
static PREFLIGHTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The value of [`PREFLIGHTS`] — how many audio pre-flights have been started in this
/// process. For the tests that pin "one user action starts ONE recording session".
#[cfg(test)]
// Its only caller is `record::zc_fallback_live_tests`, which is gated to the Linux
// zero-copy build (there is no zero-copy fallback to test anywhere else).
#[cfg_attr(not(all(target_os = "linux", feature = "zero-copy")), allow(dead_code))]
pub(super) fn preflights_started() -> u32 {
    PREFLIGHTS.load(std::sync::atomic::Ordering::Relaxed)
}

pub(super) fn try_start_owned_audio() -> Result<OwnedAudioStart, String> {
    // Media time 0 (DRAGON-417) — see `OwnedAudioStart::capture_start`. Taken before
    // anything is started so no part of the capture predates the anchor.
    let capture_start = std::time::Instant::now();
    // Counted (and logged) FIRST, before anything can return early — including the
    // forced-failure seam below. The count answers "how many recording sessions did that
    // one user action start?" (DRAGON-422), and a pre-flight that was attempted and
    // failed started a session just as much as one that succeeded: the field failure's
    // second session began with a pre-flight that was always going to fail the same way
    // the first one did. A counter that only recorded the successes would have said one.
    //
    // The value doubles as a per-call token in the FIFO/pipe names. The pid alone is NOT
    // unique enough: the screencopy worker's GPU zero-copy attempt runs its OWN pre-flight
    // while the CPU path's is already up, and `mkfifo` clears any stale file at the path
    // first — so with shared names the second pre-flight replaced the first's FIFOs and,
    // on falling back, `cleanup()` unlinked them. ffmpeg then found no such file and the
    // recording came out silent, with nothing but an "Error opening input" from a child
    // process to show for it (measured live while reproducing DRAGON-417).
    crate::util::timing_mark("rec/pre: entry (media 0)");
    let token = PREFLIGHTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    log::info!("media-clock pipeline: audio pre-flight #{token} starting");
    if test_force_owned_failure() {
        return Err("forced failure (test seam)".to_string());
    }
    let pid = std::process::id();
    // POSIX: two FIFOs in the runtime dir (paths the pump opens later). Windows
    // (DRAGON-229 M3): two `\\.\pipe\…` named pipes — `create_pipe_server` returns the
    // server handles immediately (carried into `OwnedAudioStart`), the paths are the
    // client names ffmpeg opens. Both branches yield the same two path strings the rest of
    // the pre-flight and the ffmpeg command consume.
    #[cfg(not(windows))]
    let (mic_fifo_path, sys_fifo_path) = {
        let dir = crate::util::runtime_dir();
        let m = PathBuf::from(format!("{dir}/cosmic-capture-kit.{pid}.{token}.micmix.pcm"));
        let s = PathBuf::from(format!("{dir}/cosmic-capture-kit.{pid}.{token}.sysmix.pcm"));
        if !mkfifo(&m) || !mkfifo(&s) {
            let _ = std::fs::remove_file(&m);
            let _ = std::fs::remove_file(&s);
            return Err("could not create the audio mixer FIFOs".to_string());
        }
        (m, s)
    };
    #[cfg(windows)]
    let (mic_fifo_path, sys_fifo_path, mic_pipe, sys_pipe) = {
        use crate::platform::windows::named_pipe::create_pipe_server;
        let m = PathBuf::from(format!(r"\\.\pipe\cosmic-capture-kit.{pid}.{token}.micmix"));
        let s = PathBuf::from(format!(r"\\.\pipe\cosmic-capture-kit.{pid}.{token}.sysmix"));
        let Some(mp) = create_pipe_server(&m) else {
            return Err("could not create the audio mixer named pipe (mic)".to_string());
        };
        let Some(sp) = create_pipe_server(&s) else {
            return Err("could not create the audio mixer named pipe (system)".to_string());
        };
        (m, s, mp, sp)
    };
    crate::util::timing_mark("rec/pre: fifos made");
    let (cfg, speaker) = crate::audio::config::recording_mic_config();
    // The AEC far-end reference (DRAGON-128): with the default speaker ("System
    // (automatic)"), the recording's OWN system capture below monitors the same
    // default sink the dedicated far-end capture would, so ONE capture serves both —
    // the ring is fed by the capture thread's tee (capture-time framing, per
    // `StreamTap`'s dual-timebase doc) and `spawn_aec_monitor`'s second capture is
    // retired. An EXPLICITLY chosen speaker may differ from the default sink the
    // recording monitors, so that case keeps the dedicated capture (far-end
    // behavior identical to before, per the AEC CAUTION).
    //
    // Windows (DRAGON-282): the recording's system capture loops back the CHOSEN output
    // endpoint (`speaker` is threaded into `MonitorCapture`'s source below), so its tee IS
    // the correct far-end reference for whatever endpoint is picked — there is no separate
    // default-sink monitor to diverge from (the Linux case this empty-speaker guard exists
    // for), and WASAPI has no per-sink `.monitor` dshow source for the dedicated
    // `spawn_aec_monitor` path anyway. So Windows shares the tee whenever echo is on, which
    // keeps the far-end fed from the same endpoint the system track records.
    #[cfg(windows)]
    let shared_farend =
        cfg.echo_cancellation.then(crate::audio::filters::aec::new_far_end_ring);
    #[cfg(not(windows))]
    let shared_farend = (cfg.echo_cancellation && speaker.trim().is_empty())
        .then(crate::audio::filters::aec::new_far_end_ring);
    let Some((mic_tap, mic_rx)) =
        crate::audio::clean_mic::setup_clean_mic_tap(cfg, &speaker, shared_farend.clone())
    else {
        let _ = std::fs::remove_file(&mic_fifo_path);
        let _ = std::fs::remove_file(&sys_fifo_path);
        return Err("microphone capture chain failed to start".to_string());
    };
    crate::util::timing_mark("rec/pre: mic tap up");
    let farend_tee: Option<crate::audio::capture::CaptureTee> = shared_farend.map(|ring| {
        let mut feeder = crate::audio::filters::aec::FarEndFeeder::new(ring);
        Box::new(move |samples: &[f32]| feeder.feed_interleaved_stereo(samples)) as _
    });
    // System-audio monitor source: the test override (`CCK_TEST_MONITOR_SOURCE`) always
    // wins (headless E2E). Otherwise, on Windows the persisted Output-device choice
    // (`speaker`, a WASAPI endpoint id) selects which render endpoint's loopback is recorded
    // — empty/`"default"` keep the default endpoint (DRAGON-282). On Linux/macOS the monitor
    // always follows the default sink / SCK display mix (`None`), exactly as before.
    #[cfg(windows)]
    let monitor_source = test_monitor_source_override()
        .or_else(|| crate::platform::windows::wasapi_loopback::specific_endpoint_id(&speaker));
    #[cfg(not(windows))]
    let monitor_source = test_monitor_source_override();
    let Some((monitor, sys_rx)) = MonitorCapture::start(monitor_source, farend_tee) else {
        drop(mic_tap);
        let _ = std::fs::remove_file(&mic_fifo_path);
        let _ = std::fs::remove_file(&sys_fifo_path);
        return Err("system audio (pulse monitor) connection failed to start".to_string());
    };
    crate::util::timing_mark("rec/pre: monitor up");
    log::info!(
        "media-clock pipeline: mic cleanup latency {:.1}ms",
        mic_tap.processing_latency_ms()
    );
    let started = OwnedAudioStart {
        capture_start,
        mic_fifo_path,
        sys_fifo_path,
        mic_tap,
        mic_rx,
        monitor,
        sys_rx,
        #[cfg(windows)]
        mic_pipe,
        #[cfg(windows)]
        sys_pipe,
    };
    if !smoke_check(&started.mic_rx, OWNED_AUDIO_SMOKE_BUDGET) {
        started.cleanup();
        return Err("microphone capture produced no audio (mic chain not responding)".to_string());
    }
    crate::util::timing_mark("rec/pre: mic smoke ok");
    if !smoke_check(&started.sys_rx, OWNED_AUDIO_SMOKE_BUDGET) {
        started.cleanup();
        return Err(
            "system audio (pulse monitor) produced no audio (pulse connection not responding)"
                .to_string(),
        );
    }
    crate::util::timing_mark("rec/pre: sys smoke ok (preflight done)");
    Ok(started)
}

/// The audio pre-flight, STARTED on its own thread (DRAGON-554) so a worker's
/// video-side bring-up — the PipeWire connect + format negotiation + first frame on
/// Linux, the SCK target resolution + stream build/start + first frame on macOS —
/// overlaps it instead of following it. Serialized, those two independent startups
/// summed into the session's opening span, every millisecond of which the file covers
/// with copies of the first captured frame; overlapped, the span is their MAX.
///
/// The invariant is untouched: media time 0 is still the pre-flight's own entry
/// instant (`OwnedAudioStart::capture_start`), and `start()` is the FIRST thing a
/// worker does, so that instant stays within a millisecond of the worker thread's
/// start and of the app marking itself "recording" (DRAGON-417). Only the point where
/// the worker WAITS on the result moved.
///
/// [`join`](Self::join) adds no unbounded wait (DRAGON-118): the pre-flight is bounded
/// by construction (FIFO creation, the capture-chain starts, and two
/// [`OWNED_AUDIO_SMOKE_BUDGET`] smoke checks), so the thread join it performs is
/// bounded by those same budgets. A worker whose video side failed first calls
/// [`abandon`](Self::abandon) instead, which joins and tears down whatever the
/// pre-flight managed to start.
// Dead on Windows only: the WGC worker still runs its pre-flight inline (its capture
// bring-up has not shown the multi-second serialized startup this seam exists for);
// the seam stays portable so wiring it there is a call-site change, not a port.
#[cfg_attr(windows, allow(dead_code))]
pub(super) struct AudioPreflight(PreflightState);

// Dead on Windows only — see `AudioPreflight`.
#[cfg_attr(windows, allow(dead_code))]
enum PreflightState {
    Running(std::thread::JoinHandle<Result<OwnedAudioStart, String>>),
    /// The spawn-failure fallback: the pre-flight already ran inline on the caller's
    /// thread (an OS that cannot spawn a thread this early is already degraded, but
    /// the session still gets its ordinary serialized start rather than dying).
    /// Boxed so this rare arm does not size the whole enum (clippy's
    /// `large_enum_variant`; `OwnedAudioStart` is ~240 bytes of handles).
    Done(Box<Result<OwnedAudioStart, String>>),
}

// Dead on Windows only — see `AudioPreflight`.
#[cfg_attr(windows, allow(dead_code))]
impl AudioPreflight {
    /// Start the pre-flight on its own thread. Call FIRST in a worker, before any
    /// video-side bring-up, so `capture_start` (media 0) stays within a millisecond
    /// of the worker's start.
    pub(super) fn start() -> Self {
        match std::thread::Builder::new()
            .name("cck-audio-preflight".to_string())
            .spawn(try_start_owned_audio)
        {
            Ok(thread) => AudioPreflight(PreflightState::Running(thread)),
            Err(e) => {
                log::warn!(
                    "audio pre-flight: could not spawn its thread ({e}); running it inline"
                );
                AudioPreflight(PreflightState::Done(Box::new(try_start_owned_audio())))
            }
        }
    }

    /// Wait for the pre-flight's result (bounded by its own internal budgets; see the
    /// type doc). A panicked pre-flight thread reports as an ordinary named failure.
    pub(super) fn join(self) -> Result<OwnedAudioStart, String> {
        match self.0 {
            PreflightState::Running(thread) => thread.join().unwrap_or_else(|_| {
                Err("the audio pre-flight thread panicked".to_string())
            }),
            PreflightState::Done(result) => *result,
        }
    }

    /// Join and tear down: for a worker whose VIDEO side already failed, so whatever
    /// the pre-flight started (captures, FIFOs) never leaks past the failed session.
    pub(super) fn abandon(self) {
        if let Ok(owned) = self.join() {
            owned.cleanup();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::salvage_decision;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    use std::process::ExitStatus;

    #[cfg(unix)]
    fn ok(code: i32) -> std::io::Result<ExitStatus> {
        // A raw wait-status whose low byte is 0 encodes a normal exit with `code`.
        Ok(ExitStatus::from_raw(code << 8))
    }
    #[cfg(windows)]
    fn ok(code: i32) -> std::io::Result<ExitStatus> {
        // Windows exit statuses ARE the process exit code directly.
        Ok(ExitStatus::from_raw(code as u32))
    }
    #[cfg(unix)]
    fn killed() -> std::io::Result<ExitStatus> {
        // Low byte non-zero = terminated by signal (SIGKILL = 9): never `success()`.
        Ok(ExitStatus::from_raw(9))
    }
    #[cfg(windows)]
    fn killed() -> std::io::Result<ExitStatus> {
        // No signals on Windows — a killed ffmpeg surfaces as a non-zero exit code.
        Ok(ExitStatus::from_raw(1))
    }

    #[test]
    fn clean_exit_with_no_wedge_succeeds() {
        assert!(salvage_decision(&ok(0), false, true).is_ok());
        assert!(salvage_decision(&ok(0), false, false).is_ok());
    }

    #[test]
    fn stop_tail_death_with_a_live_temp_salvages() {
        // ffmpeg killed at the very end but the mkv on disk grew past the header —
        // the DRAGON-125 field wedge: salvage it through finalize, don't delete.
        assert!(salvage_decision(&killed(), false, true).is_ok());
        assert!(salvage_decision(&ok(1), false, true).is_ok());
    }

    #[test]
    fn stop_tail_death_with_no_temp_is_fatal() {
        // Killed AND nothing on disk grew: nothing worth keeping.
        let e = salvage_decision(&killed(), false, false).unwrap_err();
        assert!(e.contains("ffmpeg"), "names the ffmpeg failure: {e}");
        let e = salvage_decision(&ok(1), false, false).unwrap_err();
        assert!(e.contains("exited with"), "reports the non-zero exit: {e}");
    }

    #[test]
    fn a_wedged_muxer_is_always_fatal_even_with_a_grown_temp() {
        // The watchdog/liveness kill (`muxer_wedged`) means it never wrote REAL output;
        // a grown temp is just the bare header churn — stays fatal, named as such.
        let e = salvage_decision(&killed(), true, true).unwrap_err();
        assert!(e.contains("never wrote output"), "names the wedge: {e}");
        let e = salvage_decision(&ok(0), true, true).unwrap_err();
        assert!(e.contains("never wrote output"), "a wedge overrides even a clean exit: {e}");
    }

    #[test]
    fn a_reap_io_error_surfaces_its_message() {
        let e = salvage_decision(&Err(std::io::Error::other("reap boom")), false, false)
            .unwrap_err();
        assert!(e.contains("reap boom"), "surfaces the io error: {e}");
    }
}
