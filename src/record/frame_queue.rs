//! The capture-to-encoder frame hand-off (DRAGON-713): ONE bounded queue, with ONE
//! eviction rule, shared by every raw-frame recording worker.
//!
//! Every worker that captures RGBA/BGRA frames on one thread and feeds ffmpeg on another
//! (`record::pipewire` on Linux, `record::sck` on macOS, `record::wgc` on Windows) used to
//! carry its own `sync_channel(8)` plus `try_send`. That is the wrong container, and it was
//! wrong in the same way on all three.
//!
//! ## The eviction direction is the whole point
//!
//! `try_send` refuses the INCOMING frame when the queue is full and leaves the queue
//! untouched, so during any slow-write window the queue holds the eight OLDEST arrivals and
//! discards everything newer. The consumer wants the exact opposite: `latch_freshest` (the
//! DRAGON-164 contract) drains the queue, keeps only the NEWEST frame, and throws the rest
//! away. Producer and consumer therefore worked against each other: the producer discarded
//! precisely the frames the consumer would have kept, and held precisely the ones it would
//! throw away, so the next tick wrote a frame up to a full queue depth of delivery intervals
//! stale while the current frame had been available and refused.
//!
//! DRAGON-667 had already diagnosed this, on Windows, for the OPENING phase alone, and its
//! `stash_frame` is what this module lifts into the shared tree. The steady-state queue on
//! all three platforms never got the same treatment. It has it now, and there is only one
//! copy of the rule left.
//!
//! ## Coalescing is not decimation
//!
//! Capture sources deliver on CHANGE, at up to the display's refresh rate, so a 240Hz panel
//! can deliver eight frames inside one 30fps tick and only one of them can ever be shown.
//! Same-tick arrivals therefore COALESCE into the newest entry: the entry keeps its own
//! delivery stamp and takes only the newer pixels. Skipping frames that arrive too soon
//! would instead keep the OLDEST of each tick, and a change that lands 5ms after a kept
//! frame and is then followed by a static screen would be lost for as long as the screen
//! stays still. Same reason the consumer latches the freshest rather than the next.
//!
//! The coalesced entry KEEPING its original stamp is what makes the grouping terminate: it
//! is the fixed reference each later arrival is measured against, so a new entry begins once
//! per `1/fps`. Measuring against the PREVIOUS frame instead makes the reference slide
//! forward on every coalesce, no arrival is ever a full tick past it, and an entire
//! pre-flight collapses into one frame. That was the first draft of DRAGON-667's version of
//! this, and `the_stash_keeps_one_frame_per_tick_across_a_real_preflight` exists to catch it.
//!
//! ## Two bounds, one queue
//!
//! The queue is retuned once per session rather than being two containers:
//!
//! * [`FrameBound::opening`], while the worker is bringing the session up and nothing is
//!   draining: the audio pre-flight runs INLINE on the worker thread on every platform, and
//!   each frame delivered during it belongs to a real opening tick. Bounded by BOTH
//!   [`OPENING_STASH_BUDGET_BYTES`] and [`OPENING_STASH_MAX_SECS`], with a hard floor at
//!   [`FRAME_CHANNEL_DEPTH`]: a flat frame count is the wrong unit because frames are the
//!   captured size (1.4MB for a small region against 33MB for a 4K monitor, a 23x spread),
//!   and a pure byte rule would leave the largest captures worse off than the depth they
//!   already shipped with.
//! * [`FrameBound::steady`], once the opening phase has handed over: exactly
//!   [`FRAME_CHANNEL_DEPTH`] frames, which is the depth every worker already had.
//!
//! **The steady depth is deliberately NOT raised.** Retaining what a saturated session
//! refuses would need gigabytes, the media clock owes `fps * elapsed` ticks whatever is
//! queued (so depth buys latency, not smoothness), and DRAGON-710 measured a large
//! downstream buffer making captured-frame drops sixty times worse. Same eight slots, better
//! contents.
//!
//! ## The doorbell
//!
//! The queue is a `Mutex<VecDeque<F>>`, which cannot be waited on, so a `sync_channel::<()>(1)`
//! rides beside it purely as a wakeup: the producer pushes a frame and then drops a token in
//! (ignoring a full channel), and [`FrameSource::recv_timeout`] waits on the token. The
//! token count is not the frame count and is never treated as one, so a stale token is a
//! harmless spurious wakeup: the consumer finds nothing, re-feeds `last`, and CFR holds. The
//! sender lives in [`FrameSink`], so the mpsc `Disconnected` semantics every worker's
//! first-frame wait already relies on survive unchanged.

use std::collections::VecDeque;
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How many captured frames a worker holds between the capture source and the encoder in
/// the STEADY state. Small on purpose: one frame is `w*h*4` bytes (tens of megabytes at 4K),
/// and depth buys latency, not smoothness, because the media clock decides how many ticks
/// are owed regardless of how much is queued.
///
/// It is also the FLOOR the opening bound may never evict below: a single 4K frame exceeds
/// any sensible byte budget, so a pure byte rule would leave the largest captures worse off
/// than the flat depth that already shipped.
pub(crate) const FRAME_CHANNEL_DEPTH: usize = 8;

/// How many bytes of captured frames the OPENING queue may hold (DRAGON-667).
///
/// The session's opening span is the whole audio pre-flight, which runs inline on the worker
/// thread with nothing draining: measured at ~370ms on macOS and 843ms (1137ms on another
/// session) on Windows, which is 11 to 34 ticks at 30fps. The opening phase places one
/// captured frame per tick, so covering that span needs about that many frames, and what it
/// used to have was [`FRAME_CHANNEL_DEPTH`] of them at most. On Windows the first 0.8s of
/// every recording was consequently one still image, confirmed by `freezedetect` and by
/// per-frame hashes on the finished file.
pub(crate) const OPENING_STASH_BUDGET_BYTES: usize = 96 * 1024 * 1024;

/// How many SECONDS of ticks the opening queue will hold, whatever the byte budget allows.
/// It exists to cover the pre-flight, which is bounded by its own budgets; holding several
/// seconds of a small region past that is memory spent on frames no tick will ever ask for.
pub(crate) const OPENING_STASH_MAX_SECS: u64 = 2;

/// What the shared eviction rule needs to know about one captured frame. Deliberately tiny:
/// the workers' frame types differ (a struct with an `Instant` on macOS and Windows, a
/// timing tuple on Linux) and none of that belongs here.
pub(crate) trait CapturedFrame {
    /// The pixel payload's size in bytes, for the byte budget.
    fn frame_bytes(&self) -> usize;

    /// How long after `older` this frame was delivered, saturating at zero. Both stamps come
    /// from the same worker's own monotonic clock; nothing here compares across workers.
    fn since(&self, older: &Self) -> Duration;

    /// Take `newer`'s PIXELS, keeping this frame's own delivery stamp (see the module doc's
    /// coalescing section for why the stamp must not move).
    fn absorb(&mut self, newer: Self);
}

/// How much a queue may hold. Two named constructors rather than a free-form struct, so the
/// only two shapes in the tree are the two the workers actually switch between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameBound {
    /// Never evict below this many frames, whatever the byte budget says.
    floor: usize,
    /// Never hold more than this many frames.
    max_frames: usize,
    /// Never hold more than this many bytes of pixels, subject to `floor`.
    budget_bytes: usize,
    /// One media tick at the session's fps: arrivals closer together than this coalesce.
    tick_gap: Duration,
}

impl FrameBound {
    /// The steady-state bound: exactly [`FRAME_CHANNEL_DEPTH`] frames, the depth every worker
    /// already had. Do not raise it; see the module doc.
    pub(crate) fn steady(fps: u32) -> Self {
        Self {
            floor: FRAME_CHANNEL_DEPTH,
            max_frames: FRAME_CHANNEL_DEPTH,
            budget_bytes: usize::MAX,
            tick_gap: tick_gap(fps),
        }
    }

    /// The opening bound: the dual byte/seconds budget, floored at [`FRAME_CHANNEL_DEPTH`],
    /// that carries real pixels across the audio pre-flight (DRAGON-667).
    pub(crate) fn opening(fps: u32) -> Self {
        Self {
            floor: FRAME_CHANNEL_DEPTH,
            max_frames: (OPENING_STASH_MAX_SECS * u64::from(fps.max(1))) as usize,
            budget_bytes: OPENING_STASH_BUDGET_BYTES,
            tick_gap: tick_gap(fps),
        }
    }
}

/// One media tick at `fps`, the window inside which two arrivals are the same tick.
fn tick_gap(fps: u32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(fps.max(1)))
}

/// What one [`stash_frame`] did: whether the frame merged into the newest entry, and how many
/// frames fell off the OLD end to stay inside the bound. Both are observability only, but
/// both matter: a coalesce means those pixels were superseded before any tick could show
/// them, and an eviction means the span outran its budget and the earliest ticks will repeat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stashed {
    pub(crate) coalesced: bool,
    pub(crate) evicted: u32,
}

/// Take `frame` into `pending`: coalesce it into the newest entry when the two belong to the
/// same media tick, otherwise append; then evict from the OLDEST end until the queue is
/// inside `bound`.
///
/// THE EVICTION DIRECTION IS THE FIX, and the module doc says why. Public because the opening
/// phase runs this same rule over its own local queue, so the frames it holds and the frames
/// the capture thread holds obey one rule and not two.
///
/// Pure, and unit-tested below.
pub(crate) fn stash_frame<F: CapturedFrame>(
    pending: &mut VecDeque<F>,
    frame: F,
    bound: FrameBound,
) -> Stashed {
    let mut out = Stashed::default();
    let same_tick = pending
        .back()
        .is_some_and(|b| frame.since(b) < bound.tick_gap);
    match pending.back_mut() {
        Some(back) if same_tick => {
            back.absorb(frame);
            out.coalesced = true;
        }
        _ => pending.push_back(frame),
    }
    while pending.len() > bound.floor
        && (pending.len() > bound.max_frames || queue_bytes(pending) > bound.budget_bytes)
    {
        pending.pop_front();
        out.evicted += 1;
    }
    out
}

/// The pixel bytes a queue is holding. Recomputed per push rather than tracked incrementally:
/// the queue is at most a couple of seconds of ticks, so this is a few dozen additions on a
/// thread that just finished copying megabytes.
fn queue_bytes<F: CapturedFrame>(q: &VecDeque<F>) -> usize {
    q.iter().map(|f| f.frame_bytes()).sum()
}

/// Say once that the opening span outran its budget. Not fatal and not rare enough to be an
/// error: it means the earliest opening ticks will repeat the first frame. Shared so all
/// three workers say the same thing.
pub(crate) fn note_opening_evictions(evicted: u64) {
    if evicted > 0 {
        log::warn!(
            "recording: the opening frame queue hit its {}MB / {}s budget ({evicted} frames \
             dropped from the oldest end); the earliest opening ticks will repeat the first \
             frame",
            OPENING_STASH_BUDGET_BYTES / (1024 * 1024),
            OPENING_STASH_MAX_SECS,
        );
    }
}

/// The queue itself, behind one lock, plus the bound it is currently held to.
struct Bounded<F> {
    frames: VecDeque<F>,
    bound: FrameBound,
}

/// What the producer and the consumer share.
struct Shared<F> {
    q: Mutex<Bounded<F>>,
}

impl<F: CapturedFrame> Shared<F> {
    /// The queue, recovering from a poisoned lock rather than degrading.
    ///
    /// Nothing under this lock can panic (a `VecDeque` push, a pop and some arithmetic), so
    /// poisoning is not reachable in practice, and the usual "degrade instead" answer is the
    /// wrong one here: dropping every frame from a poisoned capture queue would silently
    /// freeze the video for the rest of the recording. The data cannot be logically corrupted
    /// by a panic in a critical section this small, so carry on. Same call as `mixer`'s.
    fn lock(&self) -> std::sync::MutexGuard<'_, Bounded<F>> {
        self.q.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The PRODUCER handle: whatever thread the capture source delivers on. Cloneable, so a
/// source that wants several sinks keeps mpsc's disconnect semantics.
pub(crate) struct FrameSink<F> {
    shared: Arc<Shared<F>>,
    bell: SyncSender<()>,
}

impl<F> Clone for FrameSink<F> {
    fn clone(&self) -> Self {
        Self { shared: self.shared.clone(), bell: self.bell.clone() }
    }
}

impl<F: CapturedFrame> FrameSink<F> {
    /// Take one captured frame. NEVER blocks and never refuses: a queue at its bound evicts
    /// from the oldest end instead, so the newest content is always the content that
    /// survives. Reports what that cost, for the worker's own counters.
    pub(crate) fn push(&self, frame: F) -> Stashed {
        let out = {
            let mut g = self.shared.lock();
            let bound = g.bound;
            stash_frame(&mut g.frames, frame, bound)
        };
        // The doorbell, not a queue: a full channel means the consumer has a wakeup pending
        // already and one wakeup drains everything.
        let _ = self.bell.try_send(());
        out
    }
}

/// The CONSUMER handle: the worker thread that feeds ffmpeg.
pub(crate) struct FrameSource<F> {
    shared: Arc<Shared<F>>,
    bell: Receiver<()>,
}

impl<F: CapturedFrame> FrameSource<F> {
    /// Retune the queue to a new bound, trimming it to fit immediately. The one caller is
    /// the hand-over from the opening phase to the steady-state loop.
    pub(crate) fn retune(&self, bound: FrameBound) {
        let mut g = self.shared.lock();
        g.bound = bound;
        while g.frames.len() > bound.floor
            && (g.frames.len() > bound.max_frames
                || queue_bytes(&g.frames) > bound.budget_bytes)
        {
            g.frames.pop_front();
        }
    }

    /// The oldest queued frame, if any.
    fn pop_front(&self) -> Option<F> {
        self.shared.lock().frames.pop_front()
    }

    /// Wait up to `timeout` for a frame and take the OLDEST one, exactly as the bounded
    /// channel this replaces did: `Timeout` means nothing arrived (a static screen, not a
    /// stall), `Disconnected` means every sink is gone and the capture source is finished.
    /// A frame still queued outranks a disconnect, so nothing captured is stranded.
    pub(crate) fn recv_timeout(&self, timeout: Duration) -> Result<F, RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(f) = self.pop_front() {
                return Ok(f);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.bell.recv_timeout(left) {
                // A token says a push happened; it may already have been drained, in which
                // case the deadline has shrunk and the next lap sleeps for what is left.
                Ok(()) => continue,
                Err(RecvTimeoutError::Timeout) => {
                    return self.pop_front().ok_or(RecvTimeoutError::Timeout);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return self.pop_front().ok_or(RecvTimeoutError::Disconnected);
                }
            }
        }
    }

    /// Hand every queued frame to `visit`, OLDEST FIRST, and report how many there were.
    /// The queue is emptied under the lock and visited outside it, so a slow visitor never
    /// blocks the capture thread.
    ///
    /// This is the one primitive both consumer shapes are built from: the steady-state loop
    /// keeps only the last frame it is handed (the DRAGON-164 latch), and the opening phase
    /// keeps them all, because each belongs at a media tick of its own.
    pub(crate) fn drain_each(&self, mut visit: impl FnMut(F)) -> u32 {
        let taken = std::mem::take(&mut self.shared.lock().frames);
        let n = taken.len() as u32;
        for f in taken {
            visit(f);
        }
        n
    }
}

/// A queue plus its doorbell, held to `bound` until someone retunes it.
pub(crate) fn channel<F: CapturedFrame>(bound: FrameBound) -> (FrameSink<F>, FrameSource<F>) {
    let shared = Arc::new(Shared { q: Mutex::new(Bounded { frames: VecDeque::new(), bound }) });
    let (bell_tx, bell_rx) = sync_channel::<()>(1);
    (
        FrameSink { shared: shared.clone(), bell: bell_tx },
        FrameSource { shared, bell: bell_rx },
    )
}

#[cfg(test)]
mod tests {
    use super::{
        channel, note_opening_evictions, stash_frame, CapturedFrame, FrameBound,
        FRAME_CHANNEL_DEPTH, OPENING_STASH_MAX_SECS,
    };
    use std::collections::VecDeque;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    /// A 4x4 test frame whose every byte is `mark` (so a test can say WHICH frame it is
    /// looking at), delivered at `at`. The same shape the macOS and Windows workers use.
    #[derive(Debug)]
    struct TestFrame {
        mark: u8,
        px: Vec<u8>,
        at: Instant,
    }

    impl CapturedFrame for TestFrame {
        fn frame_bytes(&self) -> usize {
            self.px.len()
        }
        fn since(&self, older: &Self) -> Duration {
            self.at.saturating_duration_since(older.at)
        }
        fn absorb(&mut self, newer: Self) {
            self.mark = newer.mark;
            self.px = newer.px;
        }
    }

    fn frame_at(mark: u8, at: Instant) -> TestFrame {
        TestFrame { mark, px: vec![mark; 4 * 4 * 4], at }
    }

    /// A bound whose byte budget is spelled out, for the tests that drive the budget edge.
    fn bound_of(fps: u32, max_frames: usize, budget_bytes: usize) -> FrameBound {
        let mut b = FrameBound::opening(fps);
        b.max_frames = max_frames;
        b.budget_bytes = budget_bytes;
        b
    }

    /// DRAGON-713's headline, and DRAGON-667's before it: the queue must throw away the
    /// STALE end, because the consumer only ever wants the fresh one. The container this
    /// replaces did the exact opposite.
    #[test]
    fn the_queue_evicts_the_oldest_not_the_newest() {
        let t0 = Instant::now();
        let mut q = VecDeque::new();
        // 4x4x4 = 64 bytes a frame; a 640-byte budget is 10 frames.
        let bound = bound_of(30, usize::MAX, 640);
        for i in 0u8..40 {
            stash_frame(&mut q, frame_at(i, t0 + Duration::from_millis(40 * i as u64)), bound);
        }
        assert!(q.len() <= 10, "held to the byte budget, got {}", q.len());
        assert_eq!(q.back().expect("non-empty").mark, 39, "the newest must survive");
        assert!(
            q.front().expect("non-empty").mark > 0,
            "the oldest must have been evicted, not the newest"
        );
    }

    /// The same property through the real sink: a producer that outruns the consumer must
    /// leave the consumer holding the END of the burst, not its beginning.
    #[test]
    fn a_producer_that_outruns_the_consumer_leaves_the_newest_frames() {
        let t0 = Instant::now();
        let (sink, src) = channel::<TestFrame>(FrameBound::steady(30));
        for i in 0u8..100 {
            // 40ms apart: every frame is its own 30fps tick, so nothing coalesces and the
            // bound is the only thing thinning the queue.
            sink.push(frame_at(i, t0 + Duration::from_millis(40 * i as u64)));
        }
        let mut seen = Vec::new();
        let n = src.drain_each(|f| seen.push(f.mark));
        assert_eq!(n as usize, FRAME_CHANNEL_DEPTH, "the steady depth is unchanged");
        assert_eq!(seen, vec![92, 93, 94, 95, 96, 97, 98, 99], "the tail of the burst");
    }

    /// A 240Hz panel delivers ~8 frames per 30fps tick and only one can ever be shown.
    /// Coalescing must keep the LAST of them: skipping the late arrivals instead would keep
    /// the oldest, and a change landing 5ms into a tick before the screen went static would
    /// be lost for as long as it stayed static.
    #[test]
    fn the_queue_coalesces_within_a_tick_and_keeps_the_freshest() {
        let t0 = Instant::now();
        let mut q = VecDeque::new();
        let bound = FrameBound::opening(30);
        let mut coalesced = 0u32;
        for i in 0u8..8 {
            // 4ms apart: all eight inside one 33.3ms tick.
            let out = stash_frame(&mut q, frame_at(i, t0 + Duration::from_millis(4 * i as u64)), bound);
            coalesced += u32::from(out.coalesced);
        }
        assert_eq!(q.len(), 1, "one tick must hold one frame");
        assert_eq!(q[0].mark, 7, "and it must be the freshest of that tick");
        assert_eq!(coalesced, 7, "the seven superseded arrivals are reported");
    }

    /// The coalesced entry keeps its OWN stamp, which is what makes the grouping terminate.
    /// Measuring each arrival against the previous frame instead slides the reference
    /// forward forever and collapses a whole pre-flight into one frame.
    #[test]
    fn the_queue_keeps_one_frame_per_tick_across_a_real_preflight() {
        // The measured case: an 843ms pre-flight at 30fps needs ~25 ticks covered. A 60Hz
        // panel delivers ~50 frames in that span, two per tick.
        let t0 = Instant::now();
        let mut q = VecDeque::new();
        let bound = FrameBound::opening(30);
        for i in 0u16..50 {
            let at = t0 + Duration::from_micros(16_667 * i as u64);
            stash_frame(&mut q, frame_at(i as u8, at), bound);
        }
        assert!(
            (24..=26).contains(&q.len()),
            "one frame per 30fps tick across 833ms, got {}",
            q.len()
        );
    }

    /// A 4K frame alone exceeds any sensible byte budget, so a pure byte rule would leave
    /// the LARGEST captures worse off than the flat depth they shipped with.
    #[test]
    fn the_byte_budget_never_evicts_below_the_channel_depth() {
        let t0 = Instant::now();
        let mut q = VecDeque::new();
        let bound = bound_of(30, usize::MAX, 0);
        for i in 0u8..20 {
            stash_frame(&mut q, frame_at(i, t0 + Duration::from_millis(40 * i as u64)), bound);
        }
        assert_eq!(q.len(), FRAME_CHANNEL_DEPTH, "the floor is the steady depth");
    }

    /// Eviction is reported only when it happens, so the log can say so ONCE.
    #[test]
    fn the_queue_reports_whether_it_evicted() {
        let t0 = Instant::now();
        let bound = FrameBound::opening(30);
        let mut q = VecDeque::new();
        for i in 0u8..4 {
            let at = t0 + Duration::from_millis(40 * i as u64);
            assert_eq!(stash_frame(&mut q, frame_at(i, at), bound).evicted, 0);
        }
        // Past the 2-second cap at 30fps (60 ticks), the oldest starts falling off.
        let mut long = VecDeque::new();
        let mut ever = 0u32;
        for i in 0u16..70 {
            let at = t0 + Duration::from_millis(40 * i as u64);
            ever += stash_frame(&mut long, frame_at(i as u8, at), bound).evicted;
        }
        assert!(ever > 0, "outrunning the cap must be reported so the log can say so once");
        assert_eq!(long.len(), (OPENING_STASH_MAX_SECS * 30) as usize);
        // The shared note is what every worker says about it; prove it is callable and
        // silent when nothing was evicted.
        note_opening_evictions(0);
        note_opening_evictions(u64::from(ever));
    }

    /// The opening bound really is the bigger one: the same burst that the steady bound
    /// thins to eight is carried whole while the pre-flight runs.
    #[test]
    fn the_opening_bound_carries_a_preflight_the_steady_one_would_thin() {
        let t0 = Instant::now();
        let feed = |bound: FrameBound| {
            let mut q = VecDeque::new();
            // 40ms apart, so every frame is its own 30fps tick and nothing coalesces: the
            // bound is the only thing deciding how much survives.
            for i in 0u8..30 {
                stash_frame(&mut q, frame_at(i, t0 + Duration::from_millis(40 * i as u64)), bound);
            }
            q.len()
        };
        assert_eq!(feed(FrameBound::steady(30)), FRAME_CHANNEL_DEPTH);
        assert_eq!(feed(FrameBound::opening(30)), 30, "a 1.2s pre-flight is carried whole");
    }

    /// The hand-over: retuning to the steady bound must trim what the opening left behind,
    /// there and then, so the session does not carry a second of frames for its whole run.
    #[test]
    fn retuning_to_the_steady_bound_trims_the_opening_queue() {
        let t0 = Instant::now();
        let (sink, src) = channel::<TestFrame>(FrameBound::opening(30));
        for i in 0u8..30 {
            sink.push(frame_at(i, t0 + Duration::from_millis(40 * i as u64)));
        }
        src.retune(FrameBound::steady(30));
        let mut seen = Vec::new();
        src.drain_each(|f| seen.push(f.mark));
        assert_eq!(seen, vec![22, 23, 24, 25, 26, 27, 28, 29], "trimmed to the newest eight");
    }

    /// `recv_timeout` keeps the bounded channel's contract the workers' first-frame waits and
    /// steady loops are written against: oldest first, `Timeout` on a quiet source.
    #[test]
    fn recv_timeout_takes_the_oldest_then_times_out() {
        let t0 = Instant::now();
        let (sink, src) = channel::<TestFrame>(FrameBound::steady(30));
        sink.push(frame_at(1, t0));
        sink.push(frame_at(2, t0 + Duration::from_millis(40)));
        assert_eq!(src.recv_timeout(Duration::from_millis(50)).expect("queued").mark, 1);
        assert_eq!(src.recv_timeout(Duration::from_millis(50)).expect("queued").mark, 2);
        assert!(matches!(
            src.recv_timeout(Duration::from_millis(20)),
            Err(RecvTimeoutError::Timeout)
        ));
    }

    /// A frame already captured outranks a dead source: the disconnect is only reported once
    /// the queue is empty, so a stream that ends the instant it delivers loses nothing.
    #[test]
    fn recv_timeout_drains_before_it_reports_a_disconnect() {
        let t0 = Instant::now();
        let (sink, src) = channel::<TestFrame>(FrameBound::steady(30));
        sink.push(frame_at(7, t0));
        drop(sink);
        assert_eq!(src.recv_timeout(Duration::from_millis(50)).expect("queued").mark, 7);
        assert!(matches!(
            src.recv_timeout(Duration::from_millis(50)),
            Err(RecvTimeoutError::Disconnected)
        ));
    }

    /// A push from another thread wakes a waiting consumer through the doorbell, which is the
    /// only thing the `Mutex<VecDeque>` cannot do by itself.
    #[test]
    fn a_push_wakes_a_waiting_consumer() {
        let (sink, src) = channel::<TestFrame>(FrameBound::steady(30));
        let t0 = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            sink.push(frame_at(42, t0));
        });
        let waited = Instant::now();
        let got = src.recv_timeout(Duration::from_secs(5)).expect("the push must wake us");
        assert_eq!(got.mark, 42);
        assert!(waited.elapsed() < Duration::from_secs(4), "woke on the push, not the timeout");
    }

    /// An empty queue leaves the consumer with nothing to visit, which is how a static screen
    /// reads: the caller re-feeds its last frame and CFR holds.
    #[test]
    fn draining_an_empty_queue_visits_nothing() {
        let (_sink, src) = channel::<TestFrame>(FrameBound::steady(30));
        let mut seen = 0u32;
        assert_eq!(src.drain_each(|_| seen += 1), 0);
        assert_eq!(seen, 0);
    }

    /// A zero fps must not divide by zero anywhere in the bound arithmetic.
    #[test]
    fn a_zero_fps_is_treated_as_one() {
        let bound = FrameBound::opening(0);
        assert_eq!(bound.max_frames, OPENING_STASH_MAX_SECS as usize);
        let t0 = Instant::now();
        let mut q = VecDeque::new();
        stash_frame(&mut q, frame_at(1, t0), bound);
        stash_frame(&mut q, frame_at(2, t0 + Duration::from_millis(500)), bound);
        assert_eq!(q.len(), 1, "half a second is inside a 1fps tick");
    }
}
