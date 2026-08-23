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
/// and write to `sink` at the STREAM size. Shared so the conversion logic (and its
/// "never resize on the capture path" invariant) has exactly one definition.
/// Returns `false` once the pipe is closed. `pub(super)`: the screencopy owned path
/// (DRAGON-127, `record::screencopy`) and the macOS SCK path (`record::sck`) reuse
/// this exact closure instead of re-deriving the NV12/write logic.
///
/// The sink is `&mut dyn Write` rather than `&mut ChildStdin` (DRAGON-711) so the stop
/// tail can hand the SAME closure a [`CoveringWriter`] wrapped around ffmpeg's stdin.
/// Every capture-loop call site passes `&mut stdin` exactly as before (the coercion is
/// implicit), so no worker's own write path changed.
pub(super) fn make_frame_writer(
    w0: u32,
    h0: u32,
    nv12: bool,
) -> impl FnMut(u32, u32, &[u8], &mut dyn Write) -> bool {
    let (w0s, h0s) = (w0 as usize, h0 as usize);
    let mut nv12buf = vec![0u8; w0s * h0s * 3 / 2];
    move |w: u32, h: u32, rgba: &[u8], sink: &mut dyn Write| {
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
        sink.write_all(out).is_ok()
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
/// FIFOs. The wedge was FOUND under `-shortest`, where an ffmpeg whose audio was the
/// shorter stream decided it was DONE and stopped draining its stdin video pipe WITHOUT
/// closing the read end. Neither recording muxer carries that flag now (DRAGON-457,
/// DRAGON-710), which removes the one KNOWN way to reach the wedge and none of the
/// unknown ones: any ffmpeg that stops reading and does not close leaves us in exactly
/// the same place, so this guard stays.
///
/// A plain blocking `write_all` of even one frame parks in the kernel forever (a
/// full-monitor RGBA frame is tens of MB, dwarfing the ~64K pipe). With the write hung,
/// the worker never reaches `wait_or_kill`, never fills `RecordHandle.done`, and the
/// preview spinner spins forever (`DoneGuard` can't fire — the thread is alive, not
/// unwound). SIGKILLing ffmpeg does NOT reliably unblock a write already parked in the
/// kernel (measured live), so the fix is to never block.
///
/// While this guard is active, a write to a FULL pipe fails immediately instead of
/// parking: `WouldBlock` on POSIX, a zero-byte `Ok` under Windows' `PIPE_NOWAIT`.
/// Restoring blocking mode on drop keeps the subsequent `drop(stdin)` (EOF) and any
/// later error semantics normal.
///
/// This guard is only half the answer, and for a long time it was mistaken for the whole
/// one (DRAGON-711): "the pipe is full" is not "the pipe is dead", and giving up on the
/// first one threw away the very frames the stop tail exists to write. What sits on top
/// of it now is [`CoveringWriter`], which retries a full pipe on a BOUNDED deadline. The
/// guard is what makes that deadline enforceable at all — a blocking write cannot be
/// abandoned once the kernel has it.
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

/// How long the stop tail's covering writes may spend WAITING on a full ffmpeg stdin
/// pipe, across the whole covering phase (DRAGON-711). Not per frame: one deadline is
/// taken before the first covering write and every later one shares it, so the phase
/// costs at most this no matter how many ticks [`super::pump::VideoTicker::ticks_to_cover`]
/// asks for.
///
/// Why three seconds:
/// * It has to cover a SATURATED encoder draining the tail. The measured worst case
///   (DRAGON-710, synthetic full-motion content at 5120x1440) was about a second of
///   video left uncovered, i.e. ~30 frames at 30fps; a hardware encoder that has fallen
///   to a third of realtime still drains that inside three seconds, and by the stop tail
///   nothing new is being captured, so its backlog only shrinks.
/// * It has to be invisible to a user stopping a recording. In the healthy case it costs
///   ZERO — a pipe with room never waits — and even the pathological case is dwarfed by
///   the 30s bounded reap that follows it on the very same path.
/// * It has to NEST. Three plus that 30s reap is 33s, comfortably inside
///   `progress::SESSION_STALL_SECS` (60), where the compile-time assert pinning that
///   relationship lives.
pub(super) const COVERING_WRITE_BUDGET: Duration = Duration::from_secs(3);

/// The stop tail's BOUNDED reap (DRAGON-118): how long [`run_video_stop_tail`] waits for
/// ffmpeg to flush its backlog and exit after the stdin EOF before killing it. Named
/// (DRAGON-711) because it is now one of two waits inside the stop tail, and the bound that
/// has to clear their SUM, `progress::SESSION_STALL_SECS`, should derive from it rather than
/// restate a literal. It was three waits and two bounds until DRAGON-712 deleted Windows'
/// own `STOP_KILL_GRACE`; see `platform/windows/record_worker.rs` for why that guard is
/// gone and what replaced it.
pub(super) const STOP_TAIL_REAP: Duration = Duration::from_secs(30);

/// How many full-pipe retries spin on [`std::thread::yield_now`] before the writer starts
/// sleeping. The fast regime — ffmpeg draining eagerly, the pipe momentarily full — is
/// measured in microseconds, and a sleep there would throttle the tail to one pipe buffer
/// per timer tick (~64K per 15ms on Windows, whose default timer resolution is 15.6ms, not
/// the 1ms the argument would suggest). Yielding covers that regime; the sleep below is
/// for the genuinely stalled one, where burning a core would buy nothing.
const COVERING_WRITE_SPINS: u32 = 64;

/// The poll interval once the spins are used up. Kept short because it is the only thing
/// standing between "the encoder freed a buffer" and "we filled it".
const COVERING_WRITE_POLL: Duration = Duration::from_millis(1);

/// How long ONE capture-loop frame write may keep waiting on a FULL ffmpeg stdin pipe
/// once the session has been asked to STOP, before it abandons the frame (DRAGON-712).
///
/// Small on purpose. Past the stop nothing new is being captured and the only thing left
/// to do is close the pipe, so every millisecond spent here is a millisecond the user
/// watches a "finalizing" spinner for. Half a second is long enough that a healthy
/// encoder finishing the frame in flight is never cut short, and short enough that the
/// stop feels immediate.
pub(super) const STOP_ABANDON_GRACE: Duration = Duration::from_millis(500);

/// How long ONE capture-loop frame write may keep waiting on a FULL ffmpeg stdin pipe
/// MID-SESSION before the session declares the muxer stalled and winds down (DRAGON-712).
///
/// This is the same question `MuxerWatchdog` asks from the other side, so it takes the
/// same number: the watchdog notices "ffmpeg is being fed and writes nothing" in
/// `MUXER_LIVENESS_SECS`, and this notices "ffmpeg is not even taking bytes" in the same
/// budget. Sharing it means a stalled muxer ends a session in one budget however it
/// stalled, instead of two different ones depending on which symptom appeared first.
/// A write that spends it returns `false`, which every owned worker already treats as
/// "ffmpeg is gone, wind down through the stop tail".
pub(super) const HOT_WRITE_STALL_BUDGET: Duration =
    Duration::from_secs(super::MUXER_LIVENESS_SECS);

/// Pure and unit-tested (DRAGON-712): whether a capture-loop frame write that has found
/// ffmpeg's stdin FULL should keep waiting for room, given whether the session has been
/// asked to stop and how long this frame has already waited.
///
/// The two budgets answer two different questions. Mid-session a full pipe is ordinary
/// backpressure and waiting is exactly right, so the only bound is the one that separates
/// "slow" from "stalled" ([`HOT_WRITE_STALL_BUDGET`]). Once the user has asked to stop,
/// waiting buys nothing: the frame in flight is one of the last, and the thing that
/// actually has to happen is the stdin EOF that [`run_video_stop_tail`] delivers, which
/// this write is standing in front of. So the budget collapses to
/// [`STOP_ABANDON_GRACE`].
fn keep_waiting_for_pipe(stopping: bool, waited: Duration) -> bool {
    if stopping { waited < STOP_ABANDON_GRACE } else { waited < HOT_WRITE_STALL_BUDGET }
}

// The stop path's own worst case, end to end, must stay inside the session bound that
// would otherwise declare the whole session wedged (DRAGON-423). Written here rather
// than restated as a literal, in the style of `pump`'s render-horizon assert, because
// the failure it prevents is the one this ticket exists for: a stop that spends longer
// than `progress::SESSION_STALL_SECS` produces the recovery alert and a pre-finalize
// `-recovered.mkv` instead of a finished recording.
const _: () = assert!(
    STOP_ABANDON_GRACE.as_millis()
        + COVERING_WRITE_BUDGET.as_millis()
        + STOP_TAIL_REAP.as_millis()
        < (super::progress::SESSION_STALL_SECS as u128) * 1000,
    "the stop path's abandon grace + covering budget + bounded reap must fit inside the \
     session stall bound, or a slow stop is reported to the user as a wedged recording"
);

// The mid-session stall budget must not outlast the watchdog that asks the same question
// from the other side, or a stalled muxer would be noticed twice, at two different times,
// depending only on which symptom appeared first.
const _: () = assert!(
    HOT_WRITE_STALL_BUDGET.as_secs() <= super::MUXER_LIVENESS_SECS,
    "the capture loop's full-pipe stall budget must not outlast MUXER_LIVENESS_SECS, or a \
     stalled ffmpeg ends the session on whichever symptom happened to be seen first"
);

/// The capture loop's video sink: ffmpeg's stdin, held NON-BLOCKING for the whole session,
/// written through a patient byte-level retry that can always be ABANDONED (DRAGON-712).
///
/// ## The wedge this closes
///
/// DRAGON-160 established the rule and [`NonblockingStdin`]'s doc still states it: a
/// blocking `write_all` of a full-monitor frame into an ffmpeg that stopped draining
/// without closing the pipe parks in the kernel forever. That rule was applied to the
/// stop tail's covering writes and to nothing else, so the CAPTURE LOOP kept writing to a
/// blocking pipe, and that is where a recording was lost in the field:
///
/// 1. ffmpeg's muxer stops taking packets. It happens when the audio the pump renders
///    runs past the video ffmpeg has actually received, which is exactly what the stop
///    does: the pump's final render advances the audio by the whole render horizon
///    (`pump::RENDER_LAG_SECS`, 1.5s) while the video side has stopped feeding. Measured
///    headlessly against the shipped argv: ffmpeg accepts audio ~0.2s past its last video
///    packet and then accepts nothing at all, on every encoder, container and
///    `-max_interleave_delta` value tried.
/// 2. With the muxer stopped, ffmpeg stops draining stdin too, and the capture loop's
///    in-flight `write_all` parks.
/// 3. The loop can then never reach the top of its `while !stop` test, so it never runs
///    the stop tail, so it never closes stdin. And the stdin EOF is the ONE thing that
///    releases ffmpeg: it finishes the video stream, the muxer stops waiting for video,
///    and the audio drains in about a second (measured: 0.8s to 1.7s).
///
/// So the deadlock ring closes on our own unabandonable write standing in front of the
/// EOF that would break it. Making the write abandonable is what opens the ring, and it
/// is why this is a completion of DRAGON-160 rather than a new mechanism.
///
/// ## Why this is not a throughput regression
///
/// Measured on the incident's own shape (1528x982 BGRA, 30fps, ~6MB a frame, the shipped
/// VideoToolbox argv): 26.9fps and 161.5 MB/s with the old blocking pipe, 27.0fps and
/// 161.9 MB/s through this. The spin-then-sleep backoff [`CoveringWriter`] already uses
/// covers the fast regime, which is where nearly all of the waiting is: 108470 spins
/// against 2098 backoff sleeps over a twelve second session.
///
/// ## What it costs when it fires
///
/// A frame abandoned MID-write leaves partial bytes in a rawvideo stream that has no
/// frame boundaries to resynchronize on, so [`truncated`](Self::truncated) latches and
/// the caller skips the stop tail's covering ticks entirely rather than shearing every
/// frame after it. That is a few frames of video possibly ending short of the audio,
/// paid only on a pipeline that had already stopped moving, in exchange for the
/// recording existing at all.
pub(super) struct SessionVideoSink<'a> {
    /// Declared FIRST so it drops FIRST: the guard restores the pipe's original flags,
    /// and it must do that while `stdin` below is still open.
    _nb: NonblockingStdin,
    stdin: ChildStdin,
    stop: &'a std::sync::atomic::AtomicBool,
    /// When the CURRENT write first found the pipe full. Reset on every byte that lands,
    /// so the budget is per stall and not per frame.
    waiting_since: Option<std::time::Instant>,
    spins: u32,
    truncated: bool,
}

impl<'a> SessionVideoSink<'a> {
    pub(super) fn new(stdin: ChildStdin, stop: &'a std::sync::atomic::AtomicBool) -> Self {
        let _nb = NonblockingStdin::new(&stdin);
        Self { _nb, stdin, stop, waiting_since: None, spins: 0, truncated: false }
    }

    /// True once a frame was abandoned mid-write, so the byte stream is no longer on a
    /// frame boundary and no further video may be appended (see the type doc).
    pub(super) fn truncated(&self) -> bool {
        self.truncated
    }

    /// Hand ffmpeg's stdin back to [`run_video_stop_tail`], restoring the pipe's original
    /// flags on the way (the stop tail re-applies its own guard).
    pub(super) fn into_stdin(self) -> ChildStdin {
        let Self { _nb, stdin, .. } = self;
        drop(_nb);
        stdin
    }

    /// Back off between full-pipe retries, exactly as [`CoveringWriter`] does: yield while
    /// the pipe is draining eagerly, sleep once the spins are spent. See
    /// [`COVERING_WRITE_SPINS`] for why the spin phase is load-bearing.
    fn back_off(&mut self) {
        if self.spins < COVERING_WRITE_SPINS {
            self.spins += 1;
            std::thread::yield_now();
        } else {
            std::thread::sleep(COVERING_WRITE_POLL);
        }
    }
}

impl Write for SessionVideoSink<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use std::io::ErrorKind;
        use std::sync::atomic::Ordering;
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            // The two spellings of "the pipe is full", the same pair `CoveringWriter`
            // reads: `WouldBlock` under POSIX `O_NONBLOCK`, a zero-byte success under
            // Windows' `PIPE_NOWAIT`. Everything else is the pipe being gone.
            let full = match self.stdin.write(buf) {
                Ok(0) => true,
                Ok(n) => {
                    self.spins = 0;
                    self.waiting_since = None;
                    return Ok(n);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => false,
                Err(e) if e.kind() == ErrorKind::WouldBlock => true,
                Err(e) => return Err(e),
            };
            let waited = match self.waiting_since {
                Some(since) => since.elapsed(),
                None => {
                    self.waiting_since = Some(std::time::Instant::now());
                    Duration::ZERO
                }
            };
            let stopping = self.stop.load(Ordering::Relaxed);
            if !keep_waiting_for_pipe(stopping, waited) {
                self.truncated = true;
                log::warn!(
                    "recording: ffmpeg stopped taking video after {waited:?} ({}); \
                     abandoning the frame so the session can close its stdin and let \
                     ffmpeg finish (DRAGON-712)",
                    if stopping { "the session was stopping" } else { "mid-session stall" }
                );
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    "ffmpeg stopped draining its video pipe",
                ));
            }
            if full {
                self.back_off();
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stdin.flush()
    }
}

/// A `Write` adapter over ffmpeg's NON-BLOCKING stdin that retries a FULL pipe until
/// `deadline`, then gives up (DRAGON-711). This is the stop tail's covering writes, and
/// only theirs: the capture loop writes to the raw blocking pipe as it always has.
///
/// What it fixes: [`NonblockingStdin`] makes a full pipe fail the write, and the covering
/// loop treated that failure as final, so under a saturated encoder the covering frames
/// were never written and the video stream ended SHORT of the audio — the DRAGON-78 rule
/// broken by the very code that implements it. `-shortest` hid it (it trimmed the audio
/// down to the short video instead); DRAGON-710 removed the flag and the shortfall showed
/// as itself: 28.866s of video against 29.013s of audio, measured on Windows/MF at
/// 5120x1440.
///
/// Why the retry lives HERE, at the byte level, rather than in the covering loop above:
/// a full pipe interrupts a write MID-FRAME. Re-writing the whole frame after a partial
/// write would push duplicate bytes into a rawvideo stream that has no frame boundaries
/// to resynchronize on, and every later frame would be sheared. Resuming from the exact
/// byte offset is the only correct retry, and only the writer knows that offset — which
/// is also why the `covering` closure's `-> bool` contract did not need to grow a third
/// state. `false` still means "stop the loop"; it now means it after the deadline, not
/// after the first `EAGAIN`.
///
/// Bounded, never blocking (DRAGON-118, DRAGON-160): the pipe stays non-blocking, so a
/// wedged ffmpeg that stopped draining without closing the pipe costs the budget and not
/// a second more, and a wedge is still never MARKED here — a stop tail that ran out of
/// patience is not evidence the muxer never wrote output.
pub(super) struct CoveringWriter<'a, W: Write> {
    inner: &'a mut W,
    deadline: std::time::Instant,
    spins: u32,
}

impl<'a, W: Write> CoveringWriter<'a, W> {
    pub(super) fn new(inner: &'a mut W, deadline: std::time::Instant) -> Self {
        Self { inner, deadline, spins: 0 }
    }

    /// Back off between full-pipe retries: yield first, sleep once the spins are spent.
    fn back_off(&mut self) {
        if self.spins < COVERING_WRITE_SPINS {
            self.spins += 1;
            std::thread::yield_now();
        } else {
            std::thread::sleep(COVERING_WRITE_POLL);
        }
    }
}

impl<W: Write> Write for CoveringWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use std::io::ErrorKind;
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            // The two spellings of "the pipe is full": `WouldBlock` under POSIX
            // `O_NONBLOCK`, and a zero-byte success under Windows' `PIPE_NOWAIT`. Both
            // are retryable; every other error is the pipe being gone, and waiting on a
            // dead pipe would only spend the budget for nothing.
            let full = match self.inner.write(buf) {
                Ok(0) => true,
                Ok(n) => {
                    self.spins = 0;
                    return Ok(n);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => false,
                Err(e) if e.kind() == ErrorKind::WouldBlock => true,
                Err(e) => return Err(e),
            };
            if std::time::Instant::now() >= self.deadline {
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    "covering write gave up on a full ffmpeg stdin pipe",
                ));
            }
            if full {
                self.back_off();
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

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
/// the capture loop and pump have finished: feed the FINAL covering video ticks on a
/// BOUNDED non-blocking write, close the stdin (EOF), kill a wedged muxer, reap ffmpeg
/// BOUNDED (DRAGON-118), and return the pure [`salvage_decision`] — logging the same
/// salvage warning the workers used to inline. Extracting it here (DRAGON-161) makes the
/// bounded covering-write behavior — born macOS-only in [`NonblockingStdin`]
/// (DRAGON-160) — the SINGLE implementation, so Linux gets it too.
///
/// `covering` is called `count` times, each writing ONE covering frame; it returns
/// `false` once a write fails. These writes are what MAKES the video at least as long as
/// the audio (the DRAGON-78 rule), so failing one is a real loss, not a formality: this
/// doc used to justify ignoring it with "the video is already long enough by the time
/// these run", which was circular, and under a saturated encoder simply false
/// (DRAGON-711). Every frame therefore goes through a [`CoveringWriter`], which retries
/// a full pipe until [`COVERING_WRITE_BUDGET`] — ONE deadline shared by the whole phase —
/// and only then reports the failure that stops the loop.
///
/// A stopped loop still never marks the muxer wedged and never deletes the temp (unlike
/// the old blocking Linux loop, which did both): running out of patience with a full pipe
/// says nothing about whether ffmpeg wrote real output, and the recording that IS on disk
/// is worth more than the tick we could not deliver. `count`/`covering` are skipped
/// entirely when `muxer_wedged` is already set (a wedged muxer is being killed regardless).
///
/// Returns `Ok(())` to salvage/finalize the temp, `Err(reason)` to have already
/// removed it and fail the recording (the caller does the `remove_file`).
pub(super) fn run_video_stop_tail(
    stdin: ChildStdin,
    child: &mut std::process::Child,
    temp: &std::path::Path,
    muxer_wedged: bool,
    truncated: bool,
    count: u32,
    mut covering: impl FnMut(&mut dyn Write) -> bool,
) -> Result<(), String> {
    let mut stdin = stdin;
    if truncated {
        // DRAGON-712: the capture loop abandoned a frame mid-write, so the byte stream is
        // no longer on a frame boundary. Appending covering frames here would shear every
        // one of them (rawvideo has no boundaries to resynchronize on — see
        // `CoveringWriter`'s doc), so the EOF below is the only thing left to do. The
        // video may end short of the audio; that is the price of the pipeline having
        // stopped moving, and it is far cheaper than the sheared tail or the wedge.
        log::warn!(
            "recording stop tail: a video frame was abandoned mid-write, so the covering \
             ticks are skipped (the stream is not on a frame boundary); the video may end \
             slightly short of the audio"
        );
    }
    if !muxer_wedged && !truncated {
        // Non-blocking for the covering ticks: a stalled ffmpeg (one that stopped
        // draining video without closing the pipe) can never park this write in the
        // kernel. `CoveringWriter` then spends at most `COVERING_WRITE_BUDGET` waiting
        // for such a pipe to drain — the guard is what keeps that wait abandonable.
        let _nb = NonblockingStdin::new(&stdin);
        let deadline = std::time::Instant::now() + COVERING_WRITE_BUDGET;
        let mut sink = CoveringWriter::new(&mut stdin, deadline);
        for _ in 0..count {
            if !covering(&mut sink) {
                // Loud on purpose: this is the video ending short of the audio, the one
                // thing the covering ticks exist to prevent, and it spent a whole
                // `COVERING_WRITE_BUDGET` failing to. Either the pipe stayed full that
                // long or it is gone; both are worth a line in the debug log.
                log::warn!(
                    "recording stop tail: a covering video write failed (full ffmpeg stdin \
                     past the {COVERING_WRITE_BUDGET:?} deadline, or a closed pipe); the \
                     video stream may end slightly short of the audio"
                );
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
    let wait_result = super::wait_or_kill(child, STOP_TAIL_REAP);
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
///
/// # This is NOT media 0, and it never reliably was (DRAGON-725)
///
/// It was documented as landing "a few milliseconds after" [`media_zero`], and the app
/// therefore read media 0 off this slot. Both halves of that were wrong, and the second
/// one made every baked ink mark lead the cursor that drew it by the whole span.
///
/// The span is the OPENING PHASE, and its length is not ours to choose. The phase ends
/// when a video write returns inside one tick period, and ffmpeg paces the video pipe
/// against the audio timestamps it has already received (the same lockstep gate DRAGON-712
/// measured at the muxer and DRAGON-717 measured on the video pipe), so how long it takes
/// is decided by ANOTHER PROCESS'S SCHEDULER. Measured with `debug test mac-rec-bench
/// largest 5 videotoolbox 30 1245`, n=8 per build: 328ms median before the v0.34.0 batch,
/// 480ms after it, and the range ran 54ms to 562ms. The "9 to 30ms" readings that made the
/// old claim look true came from light-content window recordings whose first write happened
/// to return inside one tick, not from the pipeline being fast.
///
/// **Levers tried, all MEASURED, all worse, so nobody re-runs them.** Restoring `-shortest`
/// does shorten it (480ms to 206ms median) and is the ONLY thing that does, but DRAGON-710
/// and DRAGON-712 both measured that flag losing recordings, so it stays gone. Delivering
/// the audio EARLIER (deciding `pump`'s device-latency latch at pump entry, so the DRAGON-554
/// opening prime renders at 203ms instead of 421ms) made it 5x WORSE, 2604ms median.
/// Delivering it LATER (the pre-DRAGON-717 fixed render holdback) made it 6.7x worse,
/// 3206ms. The shipped configuration already sits near that curve's optimum.
///
/// A related red herring, recorded here because it will look like the culprit to the next
/// reader exactly as it did to the last one: `pump::SYS_LATENCY_LATCH_BUDGET` (350ms) holds
/// the opening prime on every macOS and Windows session, because
/// `audio::capture::MonitorCapture::latest_signed_latency_ms` structurally never reports on
/// SCK or WASAPI and the latch only fails open once the budget expires. It is real, it is on
/// the critical path, and cutting it is the 2604ms row above. Leave it alone.
///
/// So the fix was never to close the span. It is to stop inferring media 0 from it: that is
/// [`super::RecordHandle::media_zero_at`], filled by [`media_zero`] itself.
///
/// `session_start` is media 0, taken so this can REPORT the span (DRAGON-725 acceptance 4).
/// The old claim drifted silently for weeks because nothing measured it, so every session
/// now says how long its opening took, and one past [`SETTLE_SPAN_WARN`] says so loudly.
pub(super) fn mark_settled(
    slot: &std::sync::Mutex<Option<std::time::Instant>>,
    at: std::time::Instant,
    session_start: std::time::Instant,
) {
    if let Ok(mut g) = slot.lock() {
        *g = Some(at);
    }
    let span = at.saturating_duration_since(session_start);
    if settle_span_is_notable(span) {
        log::warn!(
            "recording bring-up: the opening phase ran {:.0}ms past media 0, which is longer \
             than the {:.0}ms this platform's openings are expected to need; the file and the \
             UI are still correct (both count from media 0, DRAGON-725), but the opening's \
             own ticks are close to outliving the capture queue's opening window and will \
             start repeating frames",
            span.as_secs_f64() * 1000.0,
            SETTLE_SPAN_WARN.as_secs_f64() * 1000.0,
        );
    } else {
        log::debug!(
            "recording bring-up: the opening phase ran {:.0}ms past media 0 (DRAGON-725)",
            span.as_secs_f64() * 1000.0,
        );
    }
}

/// How long an opening phase may run past media 0 before [`mark_settled`] says so at `warn`
/// rather than `debug`.
///
/// Not a correctness bound: nothing downstream needs this span to be small any more, since
/// the file and every UI readout count from media 0. It is a DRIFT DETECTOR, and it exists
/// because the old "a few milliseconds" claim went wrong silently and stayed wrong for weeks.
///
/// The number is tied to a real edge rather than picked. Past
/// `frame_queue::OPENING_STASH_MAX_SECS` the capture queue starts dropping the very frames
/// the opening's ticks are waiting for, which is DRAGON-554's frozen opening returning, so
/// the warning has to fire BEFORE that. Measured healthy openings top out at 562ms, so this
/// sits comfortably above the normal band and comfortably below the edge.
pub(super) const SETTLE_SPAN_WARN: std::time::Duration = std::time::Duration::from_millis(1500);

const _: () = assert!(
    SETTLE_SPAN_WARN.as_millis()
        < (super::frame_queue::OPENING_STASH_MAX_SECS as u128) * 1000,
    "the opening-span warning must fire BEFORE the capture queue's opening window expires \
     (DRAGON-725): past that window the opening's own ticks lose the frames they are waiting \
     for and the file opens on a repeated frame, which is the DRAGON-554 freeze"
);

/// Whether an opening phase's span past media 0 is long enough to report at `warn`.
///
/// Pure, unit-tested. The whole rule is [`SETTLE_SPAN_WARN`]; this exists so the comparison
/// has one home and one test rather than being spelled inline at a call site nothing can
/// exercise without a live recording.
fn settle_span_is_notable(span: std::time::Duration) -> bool {
    span >= SETTLE_SPAN_WARN
}

/// MEDIA TIME 0: the instant the whole pipeline is ready, which every owned worker takes
/// immediately before spawning [`super::pump`] and after its ffmpeg exists (DRAGON-672).
///
/// The file begins HERE, and everything that counts the FILE counts from here: the record
/// chip's elapsed readout and the clock every live ink stroke is stamped against, both
/// through [`super::RecordHandle::media_zero_at`], which this function fills.
///
/// The warming spinner is on screen for everything before it, so nothing captured before
/// this instant was ever promised to the user, and nothing captured before it is kept.
///
/// THE SPINNER DOES NOT CLEAR HERE, and this doc used to say it did (DRAGON-725). It said
/// `settled_at` "lands a few milliseconds after this", and the app read media 0 off that
/// slot on the strength of it. The opening phase sits between the two and is 328 to 480ms
/// on a measured mac, not a few milliseconds, so every baked ink mark led the cursor that
/// drew it by that span and the elapsed readout undercounted the finished file by it. The
/// live declaration is a SEPARATE question with a separate answer; see
/// [`mark_settled`] for the measurements, and for the levers that were tried on the span
/// and made it worse.
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
///
/// `slot` is [`super::RecordHandle::media_zero_at`], and filling it HERE rather than at the
/// six call sites is the point: the function that mints media 0 is the function that
/// reports it, so a worker cannot anchor its clock at one instant and tell the UI another.
pub(super) fn media_zero(
    slot: &std::sync::Mutex<Option<std::time::Instant>>,
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
    // Poison-tolerant like every other slot write here: a recording never dies of a lock.
    if let Ok(mut g) = slot.lock() {
        *g = Some(now);
    }
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

/// RETRACT the two bring-up signals a worker published for a session it is now ABANDONING
/// to a successor (DRAGON-730). The mirror of [`media_zero`] and [`mark_settled`], and the
/// only thing in this module that ever empties a slot.
///
/// Exactly one caller can reach this: the pipewire zero-copy worker's `ZcOutcome::Fallback`
/// return, which hands the whole recording to the CPU pipewire worker on the SAME slots (see
/// `record::start_pipewire_recording`, where both workers are given clones of one `Arc`).
/// Every other worker on every platform owns its session to the end, and the screencopy
/// zero-copy arm's post-media-0 failures are all `ZcOutcome::Done`, so none of them has
/// anything to retract.
///
/// WHY it exists, and what it does NOT fix on its own. A filled slot is supposed to describe
/// the worker that owns the session; a fallback left the abandoned attempt's media 0 sitting
/// in a slot the UI reads every 100ms, so the elapsed readout counted from an instant no
/// delivered file begins at and every ink stroke was stamped against it. Emptying the slots
/// is the invariant half of that fix and it is NOT the whole of it: a poll that already read
/// the abandoned value cannot be reached from here, which is why `app::recording`'s
/// `warm_adoption` also RE-ADOPTS a media 0 that changed. Read that function's doc before
/// deciding either half is redundant, because neither covers the other's case.
///
/// BOTH slots go, not just media 0, and that is deliberate. `warm_adoption`'s guard rests on
/// the ordering "a poll that can see `settled_at` has already been able to see
/// `media_zero_at`", and retracting only the earlier one would invert that pair for the
/// length of the successor's bring-up. Clearing `settled_at` also puts the record chip back
/// on its warming face while the CPU worker comes up, which is simply true: the pipeline is
/// not steady, and DRAGON-422 will refuse a STOP pressed in that window anyway.
///
/// [`super::RecordHandle::warm_at`] is deliberately left alone. Nothing counts from it, and
/// [`mark_first_frame`] overwrites, so the successor's own first frame lands there before it
/// reaches [`media_zero`] and the bring-up log line reads the right pair either way.
///
/// Poison-tolerant like every other slot access here: a recording never dies of a lock.
#[cfg(feature = "zero-copy")]
pub(super) fn retract_bring_up(
    media_zero_at: &std::sync::Mutex<Option<std::time::Instant>>,
    settled_at: &std::sync::Mutex<Option<std::time::Instant>>,
) {
    if let Ok(mut g) = media_zero_at.lock() {
        *g = None;
    }
    if let Ok(mut g) = settled_at.lock() {
        *g = None;
    }
    log::debug!(
        "recording bring-up: this attempt is handing the session to its fallback worker, so \
         its media 0 and settled marks are retracted; the successor publishes its own \
         (DRAGON-730)"
    );
}

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

/// DRAGON-725: the opening-span drift detector. One module of its own, per the house norm
/// that a file with several pure decisions names a module per decision.
#[cfg(test)]
mod settle_span_tests {
    use super::{SETTLE_SPAN_WARN, settle_span_is_notable};
    use std::time::Duration;

    /// The measured healthy band stays QUIET. These are the real readings from
    /// `debug test mac-rec-bench largest 5 videotoolbox 30 1245`: 54ms was the shortest
    /// opening seen on the pre-batch build and 562ms the longest on the batch. A detector
    /// that cried at either would be noise nobody reads.
    #[test]
    fn the_measured_healthy_band_says_nothing() {
        for ms in [0, 15, 54, 206, 328, 480, 562] {
            assert!(
                !settle_span_is_notable(Duration::from_millis(ms)),
                "{ms}ms is inside the measured healthy band and must not warn"
            );
        }
    }

    /// A span that has grown past the band DOES warn. 2985ms is the worst reading in the
    /// owner's own log, from a saturated 6400x3600 encode, and it is exactly the shape of
    /// growth this exists to catch.
    #[test]
    fn a_span_past_the_band_warns() {
        assert!(settle_span_is_notable(SETTLE_SPAN_WARN));
        assert!(settle_span_is_notable(Duration::from_millis(2985)));
        assert!(settle_span_is_notable(Duration::from_secs(30)));
    }

    /// The threshold is an inclusive edge, so the boundary case is decided rather than
    /// left to whichever way the comparison happened to be written.
    #[test]
    fn the_threshold_is_inclusive() {
        assert!(!settle_span_is_notable(SETTLE_SPAN_WARN - Duration::from_millis(1)));
        assert!(settle_span_is_notable(SETTLE_SPAN_WARN));
    }

    /// The relation the compile-time assert pins, stated once more at runtime so a reader
    /// grepping the tests finds the WHY: the warning has to beat the capture queue's
    /// opening window, because past that the opening's ticks lose their frames.
    #[test]
    fn the_warning_fires_before_the_opening_queue_gives_up() {
        let window = Duration::from_secs(crate::record::frame_queue::OPENING_STASH_MAX_SECS);
        assert!(
            SETTLE_SPAN_WARN < window,
            "SETTLE_SPAN_WARN ({SETTLE_SPAN_WARN:?}) must fire before the opening window \
             ({window:?}) so the warning arrives before the file's opening starts repeating"
        );
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

/// The DRAGON-711 bounded-retry writer, proved against fake pipes so it runs on every
/// platform (the real-ffmpeg half of this contract is `sck`'s `nonblocking_*` tests,
/// which are macOS-only, and the recording E2Es).
#[cfg(test)]
mod covering_writer_tests {
    use super::{COVERING_WRITE_BUDGET, CoveringWriter};
    use std::io::{Error, ErrorKind, Write};
    use std::time::{Duration, Instant};

    /// A pipe that reports itself FULL (in either platform's spelling) for the first
    /// `full_for` attempts of each write, then accepts at most `chunk` bytes — the shape
    /// of a real pipe behind a saturated encoder.
    struct FakePipe {
        wrote: Vec<u8>,
        full_for: u32,
        seen: u32,
        chunk: usize,
        /// `true` = Windows `PIPE_NOWAIT` (a zero-byte success), `false` = POSIX
        /// `O_NONBLOCK` (`WouldBlock`). Both spellings must retry identically.
        zero_byte_full: bool,
    }

    impl FakePipe {
        fn new(full_for: u32, chunk: usize, zero_byte_full: bool) -> Self {
            Self { wrote: Vec::new(), full_for, seen: 0, chunk, zero_byte_full }
        }
    }

    impl Write for FakePipe {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.seen < self.full_for {
                self.seen += 1;
                return if self.zero_byte_full {
                    Ok(0)
                } else {
                    Err(Error::new(ErrorKind::WouldBlock, "full"))
                };
            }
            self.seen = 0;
            let n = self.chunk.min(buf.len());
            self.wrote.extend_from_slice(&buf[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A pipe that is full FOREVER — the stalled ffmpeg that stopped draining without
    /// closing.
    struct AlwaysFull {
        zero_byte_full: bool,
    }

    impl Write for AlwaysFull {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            if self.zero_byte_full {
                Ok(0)
            } else {
                Err(Error::new(ErrorKind::WouldBlock, "full"))
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A pipe whose reader is GONE. Waiting on it would only burn the budget.
    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(Error::new(ErrorKind::BrokenPipe, "gone"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The whole point (DRAGON-711): a momentarily full pipe used to end the covering
    /// phase on the FIRST `EAGAIN`, so the frames that make video ≥ audio were never
    /// written. Both platforms' spellings of "full" must now be ridden out.
    #[test]
    fn a_full_pipe_is_retried_and_the_frame_lands_whole() {
        for zero_byte_full in [false, true] {
            let mut pipe = FakePipe::new(3, 4096, zero_byte_full);
            let frame: Vec<u8> = (0..40_000u32).map(|i| i as u8).collect();
            let deadline = Instant::now() + Duration::from_secs(5);
            let ok = {
                let mut w = CoveringWriter::new(&mut pipe, deadline);
                w.write_all(&frame).is_ok()
            };
            assert!(ok, "a full-then-draining pipe must not fail the write");
            assert_eq!(pipe.wrote, frame, "every byte, in order, exactly once");
        }
    }

    /// Partial writes RESUME at the byte offset instead of restarting the frame. A
    /// rawvideo stream has no frame boundaries to resynchronize on, so a restart would
    /// shear every later frame — which is why the retry lives in the writer and not in
    /// the covering loop.
    #[test]
    fn a_partial_write_resumes_instead_of_repeating_bytes() {
        let mut pipe = FakePipe::new(1, 7, false);
        let frame: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        {
            let mut w = CoveringWriter::new(&mut pipe, Instant::now() + Duration::from_secs(5));
            w.write_all(&frame).expect("the write completes");
        }
        assert_eq!(pipe.wrote.len(), frame.len(), "no duplicated bytes");
        assert_eq!(pipe.wrote, frame);
    }

    /// The bound (DRAGON-118: nothing in the record path waits unboundedly). A pipe that
    /// never drains costs the deadline and stops — it does not park, and it does not wait
    /// a second longer than it was given.
    #[test]
    fn a_pipe_that_never_drains_gives_up_at_the_deadline() {
        for zero_byte_full in [false, true] {
            let mut pipe = AlwaysFull { zero_byte_full };
            let budget = Duration::from_millis(120);
            let started = Instant::now();
            let err = {
                let mut w = CoveringWriter::new(&mut pipe, started + budget);
                w.write_all(&[0u8; 4096]).unwrap_err()
            };
            let took = started.elapsed();
            assert_eq!(err.kind(), ErrorKind::TimedOut, "names the deadline, not the pipe");
            assert!(took >= budget, "it must actually spend the budget: {took:?}");
            assert!(took < budget + Duration::from_secs(2), "and must not overrun it: {took:?}");
        }
    }

    /// A DEAD pipe is not a full one: it fails instantly, so a closed ffmpeg never costs
    /// the covering budget.
    #[test]
    fn a_closed_pipe_fails_immediately() {
        let started = Instant::now();
        let mut pipe = Broken;
        let err = {
            let mut w = CoveringWriter::new(&mut pipe, started + Duration::from_secs(30));
            w.write_all(&[0u8; 64]).unwrap_err()
        };
        assert_eq!(err.kind(), ErrorKind::BrokenPipe, "the real error survives");
        assert!(started.elapsed() < Duration::from_secs(1), "and is not waited on");
    }

    /// The budget is the STOP TAIL's, not one frame's: a hundred covering frames against
    /// a dead-full pipe still cost one budget, because the deadline is taken once.
    #[test]
    fn the_budget_covers_the_whole_phase_not_each_frame() {
        let mut pipe = AlwaysFull { zero_byte_full: false };
        let budget = Duration::from_millis(80);
        let started = Instant::now();
        let deadline = started + budget;
        let mut written = 0u32;
        {
            let mut w = CoveringWriter::new(&mut pipe, deadline);
            for _ in 0..100 {
                if w.write_all(&[0u8; 4096]).is_err() {
                    break;
                }
                written += 1;
            }
        }
        assert_eq!(written, 0, "nothing lands on a pipe that never drains");
        assert!(
            started.elapsed() < budget + Duration::from_secs(2),
            "one shared deadline, not one per frame: {:?}",
            started.elapsed()
        );
    }

    /// The shipped budget, pinned. It has to be big enough to drain a saturated encoder's
    /// tail (~1s of video, the DRAGON-710 worst case) and small enough that the 30s
    /// bounded reap after it still fits inside `progress::SESSION_STALL_SECS`.
    #[test]
    fn the_shipped_budget_stays_in_its_band() {
        assert!(
            COVERING_WRITE_BUDGET >= Duration::from_secs(2),
            "too small to drain a saturated encoder's tail"
        );
        assert!(
            COVERING_WRITE_BUDGET <= Duration::from_secs(10),
            "long enough to be felt when a user stops a recording"
        );
    }
}

/// The capture loop's abandonable video write (DRAGON-712): the pure retry decision, and
/// the sink's behaviour against a real pipe whose reader never drains.
#[cfg(test)]
mod session_video_sink_tests {
    use super::{
        HOT_WRITE_STALL_BUDGET, STOP_ABANDON_GRACE, keep_waiting_for_pipe,
    };
    use std::time::Duration;

    /// Mid-session a full pipe is ordinary backpressure: keep waiting, all the way to the
    /// stall budget. This is the property that keeps the fix from costing throughput.
    #[test]
    fn mid_session_a_full_pipe_is_backpressure_not_a_failure() {
        assert!(keep_waiting_for_pipe(false, Duration::ZERO));
        assert!(keep_waiting_for_pipe(false, Duration::from_millis(1)));
        assert!(keep_waiting_for_pipe(false, Duration::from_secs(1)));
        assert!(keep_waiting_for_pipe(false, HOT_WRITE_STALL_BUDGET - Duration::from_millis(1)));
    }

    /// A mid-session stall still ENDS. Without this the loop would spin forever on a
    /// wedged ffmpeg and never reach its own stop tail, which is DRAGON-297's shape.
    #[test]
    fn a_mid_session_stall_ends_at_the_stall_budget() {
        assert!(!keep_waiting_for_pipe(false, HOT_WRITE_STALL_BUDGET));
        assert!(!keep_waiting_for_pipe(false, HOT_WRITE_STALL_BUDGET + Duration::from_secs(1)));
    }

    /// THE regression this ticket exists for: once the user has asked to stop, the write
    /// must give up quickly, because it is standing in front of the stdin EOF that is the
    /// only thing that releases a jammed ffmpeg.
    #[test]
    fn a_stopping_session_abandons_the_frame_at_the_grace() {
        assert!(keep_waiting_for_pipe(true, Duration::ZERO));
        assert!(keep_waiting_for_pipe(true, STOP_ABANDON_GRACE - Duration::from_millis(1)));
        assert!(!keep_waiting_for_pipe(true, STOP_ABANDON_GRACE));
        assert!(!keep_waiting_for_pipe(true, STOP_ABANDON_GRACE + Duration::from_secs(30)));
    }

    /// The stop grace must be far shorter than the mid-session one. They answer different
    /// questions (see `keep_waiting_for_pipe`), and a stop that inherited the mid-session
    /// budget would spend twelve seconds before closing the pipe, which is most of the
    /// way to the session bound on its own.
    #[test]
    fn the_stop_grace_is_far_shorter_than_the_mid_session_budget() {
        assert!(
            STOP_ABANDON_GRACE * 10 <= HOT_WRITE_STALL_BUDGET,
            "stopping must abandon an order of magnitude sooner than a mid-session stall: \
             {STOP_ABANDON_GRACE:?} vs {HOT_WRITE_STALL_BUDGET:?}"
        );
    }

    /// The whole point, end to end, against a REAL pipe nobody reads: a stopping session's
    /// frame write returns instead of parking in the kernel, so the caller can reach the
    /// stdin EOF. Before DRAGON-712 this write was blocking and this test could not
    /// terminate.
    #[cfg(unix)]
    #[test]
    fn a_stopping_write_returns_instead_of_parking_on_an_unread_pipe() {
        use super::SessionVideoSink;
        use std::io::Write;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Instant;

        // A real child holding a real pipe it never reads: `cat` with its stdin piped and
        // its own stdin never touched would still drain, so use `sleep`, which reads
        // nothing at all. The pipe fills and stays full.
        let Ok(mut child) = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            eprintln!("SKIP: no `sleep` binary to hold an unread pipe");
            return;
        };
        let stdin = child.stdin.take().expect("piped stdin");
        let stop = AtomicBool::new(true);
        let mut sink = SessionVideoSink::new(stdin, &stop);
        let frame = vec![0u8; 4 * 1024 * 1024];
        let started = Instant::now();
        let err = sink.write_all(&frame).expect_err("an unread pipe cannot take 4MB");
        let took = started.elapsed();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err:?}");
        assert!(sink.truncated(), "an abandoned frame must latch `truncated`");
        assert!(
            took < STOP_ABANDON_GRACE * 4,
            "a stopping write must abandon near its grace, not park: took {took:?}"
        );
        drop(sink.into_stdin());
        let _ = child.kill();
        let _ = child.wait();
        let _ = stop.load(Ordering::Relaxed);
    }
}
