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
    /// This is when the AUDIO CAPTURE began, and since DRAGON-672 that is all it is.
    /// It is NO LONGER media time 0 — see [`media_zero`], which every owned worker now
    /// takes once its ffmpeg exists and passes to [`super::pump::spawn`] instead.
    ///
    /// It was media 0 from DRAGON-417 until DRAGON-672, because audio is captured from
    /// this instant and the indicator claimed to be recording from it too, so throwing
    /// that audio away lost real speech. DRAGON-659/661 made the indicator honest (a
    /// warming spinner until the pipeline settles), which removed the reason, and the
    /// span between the two was left in every file: nothing had promised it, the video
    /// side had no frames for it, and the audio side rendered it as silence. Read
    /// [`media_zero`]'s doc for the whole argument before moving this back.
    ///
    /// The audio captured between this instant and media 0 is dropped by the mixer,
    /// silently and by design (`mixer::Track::push` counts it `pre_start_chunks`).
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

/// Report that the worker has confirmed a genuinely real first captured frame, at `at`
/// (the frame's OWN delivery instant, not the moment the worker got round to it). Fills
/// [`super::RecordHandle::warm_at`], the DRAGON-657 warmup signal.
///
/// Call it as the LAST thing before [`try_start_owned_audio`] and nowhere else. Earlier
/// would report a candidate the worker has not confirmed, and the whole point of the
/// signal is that it cannot lie about when capture became real; later would leave the
/// slot empty across the pre-flight, which is the longest part of the bring-up and the
/// span the UI most needs to know about.
///
/// Poison-tolerant like every other slot write here: a recording never dies of a lock.
///
/// Every worker on every platform calls this now (DRAGON-658 moved the three Linux ones
/// onto the same shape), so there is no `allow(dead_code)` left to carry.
pub(super) fn mark_first_frame(
    slot: &std::sync::Mutex<Option<std::time::Instant>>,
    at: std::time::Instant,
) {
    if let Ok(mut g) = slot.lock() {
        *g = Some(at);
    }
}

/// Report that the session's OPENING PHASE is over, at `at`: the pipeline is up and level
/// with the media clock, so from here the recording is steady-state. Fills
/// [`super::RecordHandle::settled_at`], the DRAGON-661 settled signal.
///
/// Call it where each worker's opening phase ENDS, next to the "opening covered N ticks"
/// mark, and nowhere else. That point is the honest one because everything before it is
/// still bring-up: [`mark_first_frame`] only says the capture stream has proven itself, and
/// the audio pre-flight, the ffmpeg spawn, the pump spawn and the catch-up ticks all follow
/// it. On the two zero-copy arms, which have no catch-up phase (see
/// `record_screencopy_zero_copy_owned`'s doc for why that was left alone), the equivalent
/// point is where the confirmed first frame reaches the muxer and the steady loop takes
/// over; their own call sites say so.
///
/// EVERY successful path must reach one of those calls, including a session whose clock
/// owed it nothing. A recording that never fills this slot is a recording whose chip never
/// offers STOP, so "the phase covered zero ticks" and "the phase never happened" must not
/// look the same to the UI.
///
/// Unlike `warm_at`, the instant is not tied to a particular frame's own timestamp: what
/// this names is when the PIPELINE became steady, which is simply now. Poison-tolerant
/// like every other slot write here, and kept a separate function from
/// [`mark_first_frame`] rather than a shared `fill` because the two slots' contracts differ
/// and each doc has to be able to say where its own call belongs.
pub(super) fn mark_settled(
    slot: &std::sync::Mutex<Option<std::time::Instant>>,
    at: std::time::Instant,
) {
    if let Ok(mut g) = slot.lock() {
        *g = Some(at);
    }
}

/// MEDIA TIME 0: the instant the whole pipeline is ready, which every owned worker takes
/// immediately before spawning [`super::pump`] and after its ffmpeg exists (DRAGON-672).
///
/// The file begins HERE, and the point is that this is the same instant the user is told
/// recording began. The warming spinner is on screen for everything before it
/// (`App::adopt_warm_start` declares live on `settled_at`, which lands a few milliseconds
/// after this), so nothing captured before this instant was ever promised to the user, and
/// nothing captured before it is kept.
///
/// WHY THIS MOVED, because it used to be [`OwnedAudioStart::capture_start`] and the reason
/// it was matters. DRAGON-417 anchored media 0 at the audio pre-flight's ENTRY, and its
/// justification was explicit: audio is captured from that instant while "the user speaks
/// into a live-looking indicator", so discarding it would lose real speech. That was true
/// when the indicator went live as the worker was spawned. DRAGON-659/661 then made the
/// indicator honest — it shows a warming spinner and does not claim to be recording until
/// the pipeline settles — and the anchor never followed. The result was a file that began
/// ~370ms (macOS) to ~830ms (Windows) before the spinner cleared, so every recording opened
/// with a span that nothing had promised and neither stream could fill: the video side had
/// no frames for it (DRAGON-628/652/667 are all attempts to paper over exactly this) and
/// the audio side rendered it as silence (`leading_gap_samples`, measured at 804ms).
///
/// The same principle is already written down for the countdown, in
/// the countdown's own runway rule: content landing in the file ahead of
/// what the user was shown "defeats what a countdown is FOR on video". This is that rule,
/// applied to the span the spinner covers instead of the span the digits cover.
///
/// NOTHING ELSE NEEDS TO CHANGE FOR THE AUDIO, and that is worth saying so nobody adds a
/// guard for it. `mixer::Track::push` already places chunks through
/// `MediaClock::media_at_signed`, so a chunk captured before media 0 reads as negative
/// rather than colliding at frame 0, and one lying wholly before 0 is counted
/// `pre_start_chunks` and dropped SILENTLY — its doc says "predates the session, so it
/// loses nothing". The pre-flight's audio is exactly that case. DRAGON-411's "DISCARDING
/// captured audio" `error` is measured on the post-0 part only, so it stays quiet.
///
/// Takes `audio_capture_start` ([`OwnedAudioStart::capture_start`]) only to REPORT the span
/// being dropped. That number is the warmup the user was shown a spinner for, it is the
/// single figure that says whether this platform's pre-flight has grown, and it is what a
/// "my recording is missing the first moment" report should be checked against first.
///
/// `start_gate` is the COUNTDOWN GATE (DRAGON-673): a flag the app RAISES at the instant it
/// promotes the recording, which is the instant the UI starts claiming to record. `None` for
/// a capture with no countdown. Media 0 is then where the promotion is, by construction, so
/// a countdown warms the whole pipeline behind its own digits and the file still begins
/// exactly where the digits run out. This is what lets the worker be spawned at countdown
/// START: the runway constant it used to need is gone, because a gate on the real event needs
/// no estimate of how long warmup takes.
///
/// IT IS A SIGNAL, NOT A DEADLINE, and that distinction is the whole of the fix that
/// followed. The first version of this gate carried the app's PREDICTION of countdown zero
/// (`Instant::now() + secs`, stamped as the countdown began) and held until that instant.
/// The prediction drifts from the tick schedule that actually fires the capture, because the
/// app is BUSY at countdown start: it spawns the recording worker and rebuilds every overlay
/// there, and `iced::time::every` hands its ticks to a queue that has all of that in front of
/// them. Measured on a 10s countdown, the file began 992ms before the UI said "Recording"
/// (media 0 at +37.516s, `ui: recording declared live (settled)` at +38.508s). Waiting for
/// the app to SAY GO removes the whole class of error: there is no estimate left to be wrong,
/// on any platform, at any countdown length, however slow the launch.
///
/// The wait is BOUNDED (DRAGON-118) by [`START_GATE_MAX_WAIT`]. The flag is raised by the
/// app's UI thread, so a bug there (or a session that ends without ever promoting) must not
/// be able to park a worker forever; hitting the cap is logged at `warn` and the session
/// starts anyway. The app also OPENS the gate when it abandons a warming worker, so an
/// ordinary countdown cancel never reaches that cap.
pub(super) fn media_zero(
    audio_capture_start: std::time::Instant,
    start_gate: Option<&std::sync::atomic::AtomicBool>,
    mic_rx: &std::sync::mpsc::Receiver<StreamTap>,
    sys_rx: &std::sync::mpsc::Receiver<CaptureChunk>,
) -> std::time::Instant {
    if let Some(gate) = start_gate
        && !gate.load(std::sync::atomic::Ordering::Relaxed)
    {
        log::debug!(
            "media-clock session: warm and holding for the countdown to reach zero; the file \
             begins where the app promotes the recording (DRAGON-673)"
        );
        // DRAIN BOTH AUDIO TAPS WHILE HOLDING, and throw the chunks away. They are all
        // pre-media-0 by construction, so none of them belongs in the file — but the
        // captures do not stop for the hold, and nothing else is reading them until the
        // pump spawns AFTER media 0. Left alone they queue for the whole countdown, and
        // the pump then starts life several thousand chunks behind: it renders forward
        // in real time, so it never catches up, and the audio arriving while it chews
        // through the backlog lands behind the render horizon and IS destroyed. Measured
        // on the first draft of this hold, a 10s countdown: `mic(late=759 lost=4.951s)` —
        // five seconds of real speech gone, and correctly reported as an `error` by
        // DRAGON-411's counter.
        //
        // Discarding here rather than letting the mixer drop them as `pre_start` is also
        // simply cheaper, and it is sound: the taps stamp CONTIGUOUSLY at the source
        // (`StreamAnchor` anchors once at first delivery and counts samples from there),
        // so dropping chunks off the channel cannot disturb the timestamps of the ones
        // that follow.
        //
        // The flag is polled on that same drain cadence: the gate opens on another thread,
        // so the file starts within one poll of the promotion, which is far inside the
        // frame the promoted UI is drawn in.
        let deadline = std::time::Instant::now() + START_GATE_MAX_WAIT;
        loop {
            if gate.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                log::warn!(
                    "media-clock session: the countdown never opened the start gate within \
                     {:.0}s — starting the file now instead of holding any longer \
                     (DRAGON-673)",
                    START_GATE_MAX_WAIT.as_secs_f64(),
                );
                break;
            }
            while mic_rx.try_recv().is_ok() {}
            while sys_rx.try_recv().is_ok() {}
            std::thread::sleep(GATE_POLL_INTERVAL);
        }
        // One last sweep, so the pump inherits genuinely empty channels.
        while mic_rx.try_recv().is_ok() {}
        while sys_rx.try_recv().is_ok() {}
    }
    let now = std::time::Instant::now();
    log::debug!(
        "media-clock session: media 0 is the settled pipeline; dropping {:.0}ms of audio \
         captured before it, which the user was shown a spinner or a countdown for \
         (DRAGON-672/673)",
        now.saturating_duration_since(audio_capture_start).as_secs_f64() * 1000.0,
    );
    now
}

/// The ceiling on [`media_zero`]'s countdown hold (DRAGON-118: nothing in the record path
/// waits unboundedly). Comfortably past the longest countdown the UI offers, so it never
/// fires on a real capture; it exists because the gate is raised by the app and a bug there
/// would otherwise park the worker with ffmpeg already spawned and idle.
const START_GATE_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// How often [`media_zero`]'s hold drains the audio taps and re-reads the start gate. One
/// number for both because they are the same loop: the drain has to be frequent enough that
/// the taps never build a backlog, and the gate read is free, so the file starts within one
/// of these of the app's promotion.
const GATE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Called INLINE by every worker, as the first thing after it has confirmed a genuinely
/// real captured frame and reported it through [`mark_first_frame`].
///
/// It used to be startable on its OWN thread (`AudioPreflight`, DRAGON-554) so a worker's
/// video bring-up overlapped it and the session's opening span was the MAX of the two
/// rather than their sum. That seam is GONE (DRAGON-657 on macOS/Windows, DRAGON-658 on
/// Linux) and should not come back: overlapping meant audio capture, and so media 0, and
/// so the app's own "recording" state, all began before anything proved the capture
/// stream would ever deliver a pixel. The strict order costs the video bring-up's latency
/// (roughly 250-600ms) at every recording start, knowingly, because a warmup the UI can
/// show is worth more than an opening span the file has to paper over.
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
