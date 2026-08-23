//! PipeWire portal recording (CPU/readback path): consume a portal ScreenCast
//! stream and pipe its frames to ffmpeg.

use super::owned::{
    make_frame_writer, mark_first_frame, mark_settled, media_zero, run_video_stop_tail,
    try_start_owned_audio,
    MuteIntervals, OwnedAudioStart, SessionVideoSink,
};
use super::frame_queue::{self, CapturedFrame, FrameBound, FrameSource};
use super::{ToggleEvent, median_offset_ms, monotonic_ns};
use crate::audio::capture::MonitorCapture;
use crate::audio::clean_mic::MicTapHandle;
use std::collections::VecDeque;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One frame as the PipeWire consumer delivers it into the bounded channel:
/// `(width_px, height_px, tightly-packed RGBA, pts_ns, pw_delay_ns, recv_ns)`. The
/// last three are the per-frame timing state the auto-cal lag measurement reads (see
/// [`frame_lag_ms`]); the pixels are what the video ticker feeds ffmpeg. `recv_ns` is
/// also what the opening phase places each frame by (see [`frame_media_tick`]).
type Frame = (u32, u32, Vec<u8>, i64, i64, i64);

/// What the shared frame queue (`record::frame_queue`, DRAGON-713) needs to know about a
/// PipeWire frame. The queue evicts from the OLDEST end and coalesces arrivals that share a
/// media tick, so both of those are answered here and nowhere else on this path.
///
/// A coalesced entry keeps its own `recv_ns` (the group's opening instant, which is what
/// [`frame_media_tick`] places it by and what makes the grouping terminate) and takes the
/// newer frame's pixels AND its `pts`/`pw_delay`. The timing state must travel with the
/// pixels: [`frame_lag_ms`] measures how long THESE pixels took to reach the encoder, so
/// pairing new pixels with a superseded frame's capture timestamp would report a lag that
/// belongs to neither. The superseded frame contributes no lag sample, exactly as a frame
/// the old bounded channel refused contributed none.
impl CapturedFrame for Frame {
    fn frame_bytes(&self) -> usize {
        self.2.len()
    }
    fn since(&self, older: &Self) -> Duration {
        Duration::from_nanos((self.5 - older.5).max(0) as u64)
    }
    fn absorb(&mut self, newer: Self) {
        let (w, h, rgba, pts, pw_delay, _recv_ns) = newer;
        self.0 = w;
        self.1 = h;
        self.2 = rgba;
        self.3 = pts;
        self.4 = pw_delay;
    }
}

/// How long the opening phase may spend getting level with the media clock before it
/// hands over to the steady-state loop. A BOUND, not a target (DRAGON-118: nothing in the
/// record path waits unboundedly): the phase normally ends within a few hundred
/// milliseconds, the moment the clock owes it nothing. It can only fail to get level if
/// the encoder cannot take frames as fast as they come due, and that is the steady-state
/// loop's case to handle, not the opening's. Deliberately a LOCAL constant with the same
/// value as the mac SCK and Windows WGC workers' own: these are four independent capture
/// shapes, and hoisting one shared number would tie their tuning together for nothing.
const OPENING_COVER_BUDGET: Duration = Duration::from_secs(2);

/// How long the worker waits for the portal stream's FIRST frame before failing the
/// recording. A BOUND, not a target (DRAGON-118). Paused waits do not count against it.
///
/// It is the OUTER of the two video bring-up bounds, and it can only ever report that
/// nothing arrived. The INNER one is the negotiation watch inside the consumer
/// (`platform::PW_STREAMING_BUDGET`), which can say WHY, so it has to expire first.
const FIRST_FRAME_BUDGET_SECS: u32 = 10;

// DRAGON-714: the two bring-up bounds nest, inner (named diagnosis) inside outer (generic
// "nothing arrived"). Inverting them would not hang anything, which is exactly why it needs
// an assert: it would silently cost the diagnosis and leave the old symptom in its place.
const _: () = assert!(
    crate::platform::PW_STREAMING_BUDGET.as_secs() < FIRST_FRAME_BUDGET_SECS as u64,
    "DRAGON-714: the PipeWire negotiation watch must expire INSIDE the worker's \
     first-frame wait, or a failed negotiation reports 'no frames' instead of its cause"
);

/// The A/V lag (ms) a single delivered PipeWire `frame` implies — how long it took to
/// reach the encoder, measured EXACTLY as the in-loop path always has (the
/// capture-timestamp form when the frame carried a monotonic `pts`, else PipeWire's
/// reported source delay plus the queue transit since `recv_ns`). Factored out so the
/// drain below can sample EVERY frame it consumes, not just the one whose pixels it
/// keeps: each arrived frame is an independent, equally-valid latency observation for
/// the median (`median_offset_ms`), so draining stale frames must not silently drop
/// their lag samples. Returns `None` for a sample outside the trusted `1..=2000` ms band
/// (the same filter the loop applied inline).
///
/// DRAGON-713 costs this two small, bounded things, and both are worth naming. Arrivals that
/// share a media tick COALESCE in the capture queue, so a tick yields one sample rather than
/// several: still ~30 a second at 30fps, against the five the median needs, and the ones it
/// loses were frames no tick would ever have shown. And a coalesced entry keeps the tick's
/// OPENING recv stamp (it has to: that is what places it), so on the `pts == 0` form its
/// transit term can read up to one tick long. Both sit well inside the residence time this
/// measurement already includes by design, since a frame may legitimately wait in the queue
/// for several ticks before the encoder takes it.
fn frame_lag_ms(pts: i64, pw_delay: i64, recv_ns: i64) -> Option<i64> {
    let lag_ms = if pts > 0 {
        (monotonic_ns() - pts) / 1_000_000
    } else {
        (pw_delay + (monotonic_ns() - recv_ns)) / 1_000_000
    };
    (lag_ms > 0 && lag_ms <= 2000).then_some(lag_ms)
}

/// Drain `frames` to the FRESHEST currently-queued frame, updating `last` in place to its
/// dimensions + pixels and pushing EVERY consumed frame's lag sample into `lag_samples`
/// (see [`frame_lag_ms`]); report whether ANY frame was consumed (DRAGON-167). This is
/// the CFR contract's "feed the freshest frame" made explicit and headlessly testable,
/// generalizing the mac SCK `latch_freshest` (DRAGON-164) to the PipeWire tuple's
/// per-frame timing state:
///
/// The capture queue is bounded (8 deep in the steady state). When a slow encoder
/// backpressures the video write, the loop parks IN that write while the PipeWire consumer
/// thread keeps pushing. Consuming a single frame per tick (the loop's bare `recv_timeout`)
/// would then feed the OLDEST queued frame (the queue is FIFO), so the video would show
/// stale content while the newest changes sat behind it: the reported near-total freeze on
/// a loaded capture (the Linux twin of the mac SCK bug). Draining to the freshest holds
/// `last` current every tick.
///
/// DRAGON-713 fixed the producer's half of the same argument: a full queue used to REFUSE
/// the incoming frame, so the eight it kept were the eight stalest. It now evicts from the
/// oldest end instead, which is the end this function throws away anyway.
///
/// pts/lag semantics are PRESERVED, not corrupted: the intermediate frames' PIXELS are
/// the only thing discarded (they're stale by definition — a newer frame supersedes
/// them); their TIMING each contributes a lag sample exactly as if the old single-recv
/// loop had processed them one by one, so the median offset is unchanged (arguably more
/// robust — more samples). The media clock itself is index-stamped and never reads these
/// timestamps, so dropping stale pixels cannot skew it.
///
/// Returns `false` only when the queue was empty (nothing new / not yet delivered),
/// leaving `last` untouched so the caller re-feeds it to hold CFR.
fn latch_freshest(
    frames: &FrameSource<Frame>,
    last: &mut (u32, u32, Vec<u8>),
    lag_samples: &mut Vec<i64>,
) -> bool {
    frames.drain_each(|(w, h, rgba, pts, pw_delay, recv_ns)| {
        if let Some(ms) = frame_lag_ms(pts, pw_delay, recv_ns) {
            lag_samples.push(ms);
        }
        *last = (w, h, rgba);
    }) > 0
}

// ---------------------------------------------------------------------------
// The OPENING PHASE's clock arithmetic (DRAGON-658, the Linux port of DRAGON-656).
//
// The frames carry `monotonic_ns()` arrival stamps and the media clock is anchored on an
// `Instant`. Both are CLOCK_MONOTONIC here, but they share no type, so the two helpers
// below pair them once per session instead of guessing. Everything downstream of the
// pairing is pure `i64` nanosecond arithmetic and unit-tested.
// ---------------------------------------------------------------------------

/// `inst` on `monotonic_ns`'s scale. Reads both clocks back to back, so the pairing is
/// exact to within the few microseconds between the two reads.
fn ns_of_instant(inst: Instant) -> i64 {
    monotonic_ns() - Instant::now().saturating_duration_since(inst).as_nanos() as i64
}

/// The wall instant a `monotonic_ns` stamp names, the inverse of [`ns_of_instant`], for
/// reporting a frame's OWN delivery time (`owned::mark_first_frame`) rather than the
/// moment the worker got round to it. Saturates rather than panicking on a stamp that
/// would land before the `Instant` epoch.
fn instant_of_ns(ns: i64) -> Instant {
    let now = Instant::now();
    let ago = (monotonic_ns() - ns).max(0) as u64;
    now.checked_sub(Duration::from_nanos(ago)).unwrap_or(now)
}

/// The media tick a frame that ARRIVED at `recv_ns` belongs to. Video is INDEX-stamped
/// (`-r fps` on the rawvideo input), so tick k IS media time `k/fps`; a frame whose pixels
/// were on screen at media `m` therefore belongs at `floor(m * fps)`. `session_start_ns`
/// is media 0 (DRAGON-417: the instant audio capture began), on the frames' own clock.
///
/// Reads ARRIVAL (wall) time, so it is media time only while the clock is RUNNING. That is
/// why its one caller is the opening phase, which ends as soon as the clock stops
/// advancing; a pause makes `due_video_ticks` return zero and the phase exits.
///
/// Pure, and unit-tested below.
fn frame_media_tick(recv_ns: i64, session_start_ns: i64, fps: u32) -> u64 {
    let secs = (recv_ns - session_start_ns).max(0) as f64 / 1_000_000_000.0;
    (secs * fps.max(1) as f64).floor().max(0.0) as u64
}

/// Advance `last` to the frame that belongs at media `tick`: the NEWEST queued frame whose
/// own media tick is at or before it (DRAGON-656). Frames still AHEAD of `tick` stay
/// queued for the ticks they belong to, which is the whole point: spending them early is
/// what pulls live content into elapsed slots and leaves the later ticks empty. An empty
/// queue, or one holding only future frames, leaves `last` alone so the tick re-feeds it
/// and CFR holds. Reports whether `last` moved.
///
/// Pure, and unit-tested below.
fn advance_to_tick(
    pending: &mut VecDeque<Frame>,
    last: &mut (u32, u32, Vec<u8>),
    tick: u64,
    session_start_ns: i64,
    fps: u32,
) -> bool {
    let mut moved = false;
    while pending
        .front()
        .is_some_and(|f| frame_media_tick(f.5, session_start_ns, fps) <= tick)
    {
        let (w, h, rgba, ..) = pending.pop_front().expect("front was Some");
        *last = (w, h, rgba);
        moved = true;
    }
    moved
}

/// Drain every frame currently queued in `frames` into `pending`, crediting each one's lag
/// sample exactly as [`latch_freshest`] does, and holding `pending` to `bound` through the
/// shared rule (`frame_queue::stash_frame`: coalesce a tick, evict the OLDEST).
///
/// The opening phase's counterpart to `latch_freshest`: it KEEPS the arrived frames
/// instead of collapsing them onto the newest, because each of them belongs at a media
/// tick of its own (see [`advance_to_tick`]).
///
/// DRAGON-713: the cap used to be a flat eight frames, which is a fifth of a second at
/// 30fps, so an opening span longer than that arrived already thinned. `bound` is the
/// opening bound (96MB / 2s, floored at the steady depth) while the phase runs, which is
/// the same bound the capture queue itself is under, so the frames the phase holds and the
/// frames still in flight obey one rule.
fn drain_arrivals(
    frames: &FrameSource<Frame>,
    pending: &mut VecDeque<Frame>,
    lag_samples: &mut Vec<i64>,
    bound: FrameBound,
) {
    frames.drain_each(|f| {
        if let Some(ms) = frame_lag_ms(f.3, f.4, f.5) {
            lag_samples.push(ms);
        }
        frame_queue::stash_frame(pending, f, bound);
    });
}

// ---------------------------------------------------------------------------
// Media-clock OWNED path (DRAGON-125 chunk B1; the legacy wallclock+CFR+segments
// fallback retired in DRAGON-127 — this is now the ONLY recording path):
// `record_pipewire` (the public entry point) starts the PipeWire consumer thread,
// waits for a genuinely real first frame, REPORTS it (`owned::mark_first_frame` →
// `RecordHandle::warm_at`), and only THEN runs the audio pre-flight inline
// (DRAGON-658). If the pre-flight fails, the recording fails outright with its
// named, actionable reason (pulse connection / FIFO / mic chain). Any failure
// after that point (ffmpeg won't spawn, a wedged muxer) is an ordinary recording
// failure. The OTHER end of the bring-up, where the opening phase has paid its media
// debt and the session is steady, is reported separately (`owned::mark_settled` →
// `RecordHandle::settled_at`, DRAGON-661); that later signal is the one the UI calls
// "live".
//
// The ORDER is the point, and it is deliberately the slower one. The pre-flight
// used to be started on its own thread as the worker's first act (DRAGON-554's
// `owned::AudioPreflight`) so the stream's connect + format negotiation +
// first-buffer latency overlapped it and the opening span was the MAX of the two
// rather than their sum. But that meant audio capture, and so media 0, and so the
// app's own "recording" state, all began before anything proved the portal stream
// would ever deliver a pixel. DRAGON-657 chose strict sequencing on the mac and
// Windows workers for that reason and DRAGON-658 brought this one over, retiring
// the seam entirely. The cost is the video bring-up's latency at every start,
// which the UI shows as a warmup instead of the file showing it as a frozen
// opening.
// ---------------------------------------------------------------------------

/// Fold an owned-session startup failure into a single `Err` message, tearing
/// everything already-committed down first: stop the video capture (`stop` +
/// joining the PipeWire consumer thread), the mic tap, and the system monitor, and
/// remove the FIFOs. Used for every failure between "the pre-flight check passed"
/// and "the pump took ownership of the FIFOs/tap/monitor" — past that point the
/// pump's own teardown (via `PumpHandle`/`Drop`) takes over instead.
fn owned_start_failure<T>(
    stop: &Arc<AtomicBool>,
    pw: std::thread::JoinHandle<T>,
    mic_tap: MicTapHandle,
    monitor: MonitorCapture,
    mic_fifo_path: &std::path::Path,
    sys_fifo_path: &std::path::Path,
    msg: String,
) -> String {
    stop.store(true, Ordering::Relaxed);
    let _ = pw.join();
    drop(mic_tap);
    let _ = monitor.stop();
    let _ = std::fs::remove_file(mic_fifo_path);
    let _ = std::fs::remove_file(sys_fifo_path);
    msg
}

/// Record a portal/PipeWire stream through the media-clock OWNED pipeline
/// (DRAGON-125; the ONLY recording path since DRAGON-127) — the public entry point
/// `record::mod`'s `start_pipewire_recording` calls. Starts the PipeWire consumer
/// thread FIRST and the audio pre-flight only once that stream has delivered a real
/// frame (DRAGON-658; see the section comment above for why that order, and what it
/// costs). If the pre-flight can't start, the recording fails outright with a named,
/// actionable reason.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_pipewire(
    fd: OwnedFd,
    node_id: u32,
    crop: Option<(u32, u32, u32, u32)>,
    fps: u32,
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
    measured: &Mutex<Option<i32>>,
    dims: &Mutex<Option<(u32, u32)>>,
    warm_at: &Mutex<Option<Instant>>,
    media_zero_at: &Mutex<Option<Instant>>,
    settled_at: &Mutex<Option<Instant>>,
    // DRAGON-736: the live annotation ink, in both of its forms, because a PORTAL WINDOW cast
    // delivers the toplevel's OWN buffer and our ink surface is structurally not in it. The
    // NATIVE screencopy worker next door takes neither, and must not: it crops the SCREEN, so
    // the compositor already put the ink in the frame and compositing it again would draw
    // every stroke twice. This slot is the FALLBACK bake, read once at finalize.
    ink: &Mutex<Option<crate::record::ink_bake::InkBake>>,
    // DRAGON-736: and the same ink published live, so it can be painted onto each captured
    // frame on its way to the encoder (DRAGON-724's path, which this worker joins). When the
    // size gate admits the session this is what puts the drawing in the file and `ink` above
    // is dropped unread.
    live_ink: &crate::record::ink_live::LiveInk,
    metadata: &str,
    // DRAGON-693 (Bronze): the armed annotation window's one-shot request for the freshest
    // captured frame. Nothing here waits on it and an unserved request costs nothing.
    peek: &super::FramePeek,
) -> Result<PathBuf, String> {
    // Same PipeWire consumer shape as ever: a small jitter-buffered queue, dropping
    // (never blocking) when the encoder side is behind, discarding frames while
    // paused — nothing is captured then; the pump's video ticks independently freeze
    // for the same span (see `super::pump::VideoTicker`). It is the FIRST thing the
    // worker starts, and nothing audio-side exists until it has delivered a frame
    // (DRAGON-658).
    //
    // DRAGON-713: the queue is the shared `record::frame_queue`, so what a full queue drops
    // is its OLDEST frame and not the one just captured. It starts under the OPENING bound,
    // because the audio pre-flight runs inline on the worker thread with nothing draining
    // and every frame delivered during it belongs to a real opening tick; the worker retunes
    // it to the steady depth the moment the opening phase hands over.
    let (ftx, frx) = frame_queue::channel::<Frame>(FrameBound::opening(fps));
    let evicted = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let pw_evicted = evicted.clone();
    let pw_stop = stop.clone();
    let pw_paused = paused.clone();
    let pw = std::thread::spawn(move || {
        crate::platform::pipewire::consume_frames(fd, node_id, crop, pw_stop, move |rgba, w, h, pts, pw_delay| {
            if pw_paused.load(Ordering::Relaxed) {
                return;
            }
            let out = ftx.push((w, h, rgba, pts, pw_delay, monotonic_ns()));
            if out.evicted > 0 {
                let n = pw_evicted.fetch_add(u64::from(out.evicted), Ordering::Relaxed)
                    + u64::from(out.evicted);
                if n <= u64::from(out.evicted) || n.is_multiple_of(60) {
                    log::warn!(
                        "pipewire capture (media-clock): encoder backlog, {n} stale frames \
                         dropped so far (video will look choppy if this keeps up)"
                    );
                }
            }
        })
    });
    record_pipewire_owned(
        fps, preferred_encoder, encoder_hint, presets, mic, system_audio, bitrate_kbps,
        audio_offset_ms, auto_device_compensation, max_res, start_gate, out_path, stop, paused,
        events, measured, dims, warm_at, media_zero_at, settled_at, ink, live_ink, metadata, peek,
        frx, evicted, pw,
    )
}

/// The media-clock owned PipeWire session (DRAGON-125 chunk B1): ONE continuous
/// ffmpeg for the whole recording — index-stamped video
/// ([`crate::encode::spawn_ffmpeg_media_clock`]), audio rendered by
/// [`super::pump`]'s `Mixer`-backed engine — instead of the legacy
/// wallclock+CFR+per-pause-segment model. The consumer thread (`pw` + `frx`) is
/// already running (started by [`record_pipewire`]); the audio pre-flight runs HERE,
/// inline, once the first real frame has arrived and been reported (DRAGON-658), so
/// every failure above it is a video-side failure with no audio ever started. `settled_at`
/// is filled much later, where the opening phase hands over to the steady-state loop
/// (DRAGON-661).
#[allow(clippy::too_many_arguments)]
fn record_pipewire_owned(
    fps: u32,
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
    measured: &Mutex<Option<i32>>,
    dims: &Mutex<Option<(u32, u32)>>,
    warm_at: &Mutex<Option<Instant>>,
    media_zero_at: &Mutex<Option<Instant>>,
    settled_at: &Mutex<Option<Instant>>,
    // DRAGON-736: the fallback ink bake and the live ink scene; see `record_pipewire`.
    ink: &Mutex<Option<crate::record::ink_bake::InkBake>>,
    live_ink: &crate::record::ink_live::LiveInk,
    metadata: &str,
    peek: &super::FramePeek,
    frx: FrameSource<Frame>,
    // How many frames the capture queue has evicted from its oldest end (DRAGON-713). Read
    // once, where the opening phase begins, so the log can say ONCE that the opening span
    // outran its budget.
    evicted: Arc<std::sync::atomic::AtomicU64>,
    pw: std::thread::JoinHandle<Result<(), String>>,
) -> Result<PathBuf, String> {
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut all_lag_samples: Vec<i64> = Vec::new();

    // The first frame fixes the encode size — identical bounded wait to the
    // legacy path (paused waits don't count against the 10s budget). Nothing audio-side
    // exists yet (DRAGON-658), so a failure here just stops the consumer and returns.
    let no_frames = |stop: &Arc<AtomicBool>, pw: std::thread::JoinHandle<Result<(), String>>| {
        stop.store(true, Ordering::Relaxed);
        // DRAGON-714: the consumer thread's OWN reason wins when it has one. Its named
        // diagnoses (above all "the stream never reached Streaming", which is what a failed
        // format negotiation looks like from here, since no error is ever raised) say why;
        // the generic line below only says that nothing arrived.
        //
        // Noted HERE rather than left to the app, so `diag::root_failure` — which reports
        // the FIRST note of the session — names the negotiation rather than the symptom the
        // app records a moment later. Same closed vocabulary, same variant: a recording that
        // produced nothing is `RecordingFailed`, whose detail IS the user-facing copy.
        match pw.join() {
            Ok(Err(reason)) => {
                crate::diag::note_failure(crate::diag::Failure::RecordingFailed, &reason);
                reason
            }
            _ => "no frames from the PipeWire stream".to_string(),
        }
    };
    let (w0, h0, first, _pts0, _delay0, recv0) = {
        let mut waited_s = 0u32;
        loop {
            match frx.recv_timeout(Duration::from_secs(1)) {
                Ok(f) => break f,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    if paused.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    waited_s += 1;
                    if waited_s >= FIRST_FRAME_BUDGET_SECS || stop.load(Ordering::Relaxed) {
                        return Err(no_frames(&stop, pw));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(no_frames(&stop, pw));
                }
            }
        }
    };
    let (mw, mh) = crate::encode::codec_capped_resolution(max_res, &presets.codec);
    let (ew, eh) = crate::encode::fit_within(w0, h0, mw, mh);
    if let Ok(mut g) = dims.lock() {
        *g = Some((w0, h0));
    }
    let plan = super::resolve_session_plan(preferred_encoder, encoder_hint, ew, eh, presets);
    let nv12 = plan.nv12;
    let is_hevc = plan.is_hevc();

    // WARM (DRAGON-657/658). The frame above is a real captured frame, so the portal
    // stream has provably delivered pixels: report that instant, then start audio.
    // Reported with the FRAME's own arrival time (`recv0`, stamped by the consumer
    // thread), not `Instant::now()`, so the signal names when the pixels were real
    // rather than when this thread got round to them.
    //
    // This is the LAST thing before the pre-flight and nothing may come between them:
    // everything above is video bring-up that can still fail with no audio started (so
    // those paths just return), and everything below needs the pre-flight's FIFO paths.
    mark_first_frame(warm_at, instant_of_ns(recv0));
    // The pre-flight runs INLINE here. It is bounded by its own internal budgets (FIFO
    // creation, the capture-chain starts, two `OWNED_AUDIO_SMOKE_BUDGET` smoke checks),
    // so this adds no unbounded wait (DRAGON-118). Its entry instant is media 0.
    let owned = match try_start_owned_audio() {
        Ok(o) => {
            log::info!("recording pipeline: media-clock owned path (DRAGON-125)");
            o
        }
        Err(reason) => {
            log::error!("recording pipeline: audio pre-flight failed ({reason}); cannot record");
            stop.store(true, Ordering::Relaxed);
            let _ = pw.join();
            return Err(format!("could not start recording audio: {reason}"));
        }
    };
    let OwnedAudioStart {
        capture_start, mic_fifo_path, sys_fifo_path, mic_tap, mic_rx, monitor, sys_rx,
    } = owned;

    let temp = super::recording_temp_path(out_path);
    let mut child = match crate::encode::spawn_ffmpeg_media_clock(
        w0, h0, ew, eh, fps.max(1), &plan, bitrate_kbps, &temp, &mic_fifo_path, &sys_fifo_path,
    ) {
        Ok(c) => c,
        Err(e) => {
            return Err(owned_start_failure(
                &stop, pw, mic_tap, monitor, &mic_fifo_path, &sys_fifo_path, e,
            ));
        }
    };
    let Some(raw_stdin) = child.stdin.take() else {
        let _ = child.kill();
        return Err(owned_start_failure(
            &stop, pw, mic_tap, monitor, &mic_fifo_path, &sys_fifo_path,
            "ffmpeg stdin unavailable".to_string(),
        ));
    };
    // DRAGON-712: the capture loop's frames go through the ABANDONABLE sink, not the raw
    // blocking pipe. A blocking write here parks in the kernel on a jammed ffmpeg, and the
    // loop can then never reach the stdin EOF that is the only thing that unjams it. See
    // `owned::SessionVideoSink`.
    let mut stdin = SessionVideoSink::new(raw_stdin, &stop);

    let mut write_frame = make_frame_writer(w0, h0, nv12);
    // DRAGON-736: the live annotation is painted onto each captured frame HERE, immediately
    // before the frame writer, exactly where the macOS SCK and Windows WGC workers put it
    // (DRAGON-724). This is the PORTAL worker, so a WINDOW cast's frames are the toplevel's own
    // buffer and carry none of our surfaces. The gate inside decides ONCE whether this
    // session's frames are small enough to afford the per-tick copy; a refused session paints
    // nothing and the finalize bake runs instead. A session with no ink published pays one
    // `Option` test per tick and writes the captured bytes untouched, which is what keeps every
    // portal region and monitor recording byte-identical to before.
    let mut ink_paint = crate::record::ink_live::LiveInkPainter::new(
        live_ink.clone(),
        w0,
        h0,
        fps.max(1),
        plan.input_pix_fmt,
    );
    let mut last: (u32, u32, Vec<u8>) = (w0, h0, first);
    // NOT drained to the freshest here (DRAGON-658). Whatever the consumer queued while
    // ffmpeg was starting up (encoder + FIFO opens, which do not read the video pipe) is
    // the session's OPENING, and the opening phase below places each of those frames at
    // the media tick it actually belongs to instead of throwing all but the newest away.
    // Media 0 is HERE, not at the audio pre-flight (DRAGON-672), and the slot below is how
    // the UI learns it (DRAGON-725): the spinner does NOT clear here, the opening phase
    // still follows. See `owned::media_zero` for the whole argument.
    let session_start =
        media_zero(media_zero_at, capture_start, start_gate.as_deref(), &mic_rx, &sys_rx);
    // The frames' arrival stamps live on `monotonic_ns`'s clock and the media clock on
    // `Instant`'s; pair them once, here, so the opening phase can ask which tick a frame
    // belongs to (see `frame_media_tick`).
    let session_start_ns = ns_of_instant(session_start);
    let pump_cfg = super::pump::PumpConfig {
        fps: fps.max(1),
        audio_offset_ms,
        auto_device_compensation,
        mic_on0: mic,
        sys_on0: system_audio,
        duck_system: crate::audio::config::recording_duck_system(),
    };

    // The pump thread borrows `events` (a plain `&Mutex`, not an `Arc` — see
    // `super::pump::PumpHandle`'s doc) for its own lifetime, which is what
    // `std::thread::scope` makes sound: every thread spawned inside it is joined
    // before this block returns (and `pump_handle.join()` below does so
    // explicitly, well before that). `pump::spawn` returns IMMEDIATELY (its FIFO
    // rendezvous happens on the pump thread) — the opening video write a few lines
    // down must follow promptly, because those stdin bytes are what let ffmpeg get
    // past its input-0 probe and reach the FIFOs the pump is waiting to open (see
    // `pump::open_fifo_write_end`'s doc for the wedge this ordering prevents).
    // DRAGON-736: the third element is the video's own length in FRAMES, taken AFTER the stop
    // tail's covering ticks, which is the ceiling the fallback ink bake feeds up to. The mac
    // and Windows workers already carried it for the same reason.
    let scope_result: Result<(MuteIntervals, MuteIntervals, u64), String> = std::thread::scope(|scope| {
        let (pump_handle, mut ticker) = match super::pump::spawn(
            scope, session_start, pump_cfg, mic_fifo_path.clone(), sys_fifo_path.clone(), mic_tap,
            mic_rx, monitor, sys_rx, &stop, &paused, events,
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = child.kill();
                stop.store(true, Ordering::Relaxed);
                let _ = pw.join();
                return Err(e);
            }
        };

        // Stall detection + muxer liveness: identical shape/thresholds to the
        // legacy path (DRAGON-118) — except PAUSE-AWARE (unlike the legacy path,
        // whose pauses end the segment outright): this single session-long ffmpeg
        // is fed NOTHING on any input while paused, by design, so paused time can
        // never prove — or disprove — muxer liveness. The budget is spent only
        // while unpaused; a user pausing before the encoder's first output packet
        // (lookahead not yet filled) must not read as a wedge and get the
        // recording killed with its temp deleted (the DRAGON-122-integration
        // pause crash).
        const STALL_WARN_SECS: f32 = 4.0;
        let mut stalled_secs = 0.0f32;
        let mut stall_logged = false;
        let mut liveness_left =
            Some(std::time::Duration::from_secs(super::MUXER_LIVENESS_SECS));
        let mut liveness_tick = std::time::Instant::now();
        let mut muxer_wedged = false;

        // MuxerWatchdog armed BEFORE the first write (the DRAGON-118/123 wedge
        // strikes there): an ffmpeg whose scheduler wedged at start never reads
        // stdin, so a plain blocking write_all could hang forever. Pause-gated,
        // for the same reason as the in-loop liveness budget above.
        let watchdog = super::MuxerWatchdog::arm_gated(child.id(), temp.clone(), paused.clone());
        // The OPENING PHASE (DRAGON-658, the Linux port of the mac worker's DRAGON-656).
        // It covers media [0, level] and it is a PHASE, not a fixed-length burst: it
        // writes a tick, re-asks the clock, and ends only when the clock owes it nothing.
        //
        // Why it cannot be a fixed count. The count used to be decided once, up front,
        // and writing those frames takes real wall time (the first frames of a session
        // are what ffmpeg's encoder init absorbs). The clock keeps running through that,
        // so the burst finished owing more ticks, and the steady-state loop paid all of
        // them on one iteration with the single image it happened to hold: a hitch.
        //
        // Why each tick picks its OWN frame. Feeding queued frames in arrival order, one
        // per tick, as fast as the pipe takes them, pulls live content FORWARD into
        // elapsed slots, so the opening leads the audio and the ticks those frames
        // belonged to are left with nothing to show. `advance_to_tick` instead gives tick
        // k the newest frame that had actually arrived by k, so frames delivered while
        // ffmpeg was starting up land on the ticks that are waiting for them, and only
        // media [0, first arrival] (where no frame can exist) repeats the first frame.
        //
        // The steady-state loop below is UNCHANGED and still drains to the freshest:
        // `frame_media_tick` reads arrival time, which stops being media time the moment
        // anything pauses. This phase ends there anyway, since a paused clock owes zero.
        //
        // DRAGON-713: the phase inherits whatever the capture queue kept while the pre-flight
        // ran, which is now a real sliding window on the opening span rather than its first
        // eight frames, and it holds its own queue to the SAME bound.
        let opening_bound = FrameBound::opening(fps.max(1));
        frame_queue::note_opening_evictions(evicted.load(Ordering::Relaxed));
        let mut pending: VecDeque<Frame> = VecDeque::new();
        let mut tick: u64 = 0;
        let mut ticks_from_capture = 0u32;
        let opening_deadline = Instant::now() + OPENING_COVER_BUDGET;
        // `.max(1)` keeps the historical guarantee that at least one frame is written
        // here: ffmpeg must get past its input-0 probe before it will open the FIFOs
        // (see `pump::open_fifo_write_end`'s doc for the wedge that prevents).
        let mut owed = ticker.due_video_ticks(Instant::now()).max(1);
        let mut opening_ok = true;
        'opening: loop {
            while owed > 0 {
                drain_arrivals(&frx, &mut pending, &mut all_lag_samples, opening_bound);
                if advance_to_tick(&mut pending, &mut last, tick, session_start_ns, fps.max(1)) {
                    ticks_from_capture += 1;
                }
                if !write_frame(
                    last.0,
                    last.1,
                    ink_paint.paint(last.0, last.1, &last.2),
                    &mut stdin,
                ) {
                    opening_ok = false;
                    break 'opening;
                }
                tick += 1;
                owed -= 1;
            }
            // Bounded on both ends (DRAGON-118): the user stopping, and a budget for an
            // encoder that simply cannot keep up, in which case falling through to the
            // steady-state loop is the right answer anyway.
            if stop.load(Ordering::Relaxed) || Instant::now() >= opening_deadline {
                break;
            }
            owed = ticker.due_video_ticks(Instant::now());
            if owed == 0 {
                break; // level with the clock: the steady-state loop inherits no debt
            }
        }
        // Nothing may be stranded: the steady-state loop reads only `frx`. Fold what is
        // left down to the freshest, which is what that loop would have latched.
        if let Some((w, h, rgba, ..)) = pending.pop_back() {
            last = (w, h, rgba);
        }
        pending.clear(); // a queued frame is `w*h*4` bytes; do not hold them for the session
        // The opening is over, so the capture queue goes back to the steady depth it has
        // always had (DRAGON-713). The bigger opening bound exists only to cover the span
        // where nothing was draining; carrying it for the session would hold up to two
        // seconds of frames for no tick that will ever ask for them.
        frx.retune(FrameBound::steady(fps.max(1)));
        if crate::util::timing_on() {
            crate::util::timing_mark(&format!(
                "rec/pw: opening covered {tick} ticks, {ticks_from_capture} of them from a \
                 captured frame"
            ));
        }
        if opening_ok {
            // SETTLED (DRAGON-661), the same placement as the mac/Windows workers. The
            // opening phase is over and the clock owes nothing, so this is the first instant
            // the whole pipeline is steady, and it is what the UI's live declaration waits
            // for. Inside the `opening_ok` test on purpose: a failed write means ffmpeg is
            // not taking frames, so that session is dying rather than settling.
            mark_settled(settled_at, Instant::now(), session_start);
            // DRAGON-751: the stall clock measures wall time since the last REAL frame, not a
            // count of wakeups times their length. The wait below is no longer a fixed 200ms
            // when this session paints live ink, so the old `stalled_secs += 0.2` would have
            // reported a stall six times too short. Seeded here rather than at the
            // declaration above so the opening phase, which consumes frames of its own, can
            // never be charged to the first steady-state timeout.
            let mut last_frame_at = std::time::Instant::now();
            loop {
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
                // DRAGON-751: 200ms is the CEILING, and a session that paints live ink is also
                // bounded by its next media tick, exactly as in the WGC and SCK loops. A
                // portal WINDOW cast delivers the toplevel's own buffer and can be
                // damage-driven, so this loop can park through a stroke's whole fade and then
                // pay every owed tick off ONE read of the ink scene. This worker paints live
                // for that route (DRAGON-736). `capture_wait` holds the whole rule, and
                // `has_content()` (not `active()`, see its doc) is what keeps a session with
                // nothing published on exactly the pacing it has always had. It also keeps
                // BRONZE there, whose ink the app deliberately never publishes live.
                let wait = ticker.capture_wait(
                    std::time::Instant::now(),
                    std::time::Duration::from_millis(200),
                    ink_paint.has_content(),
                );
                match frx.recv_timeout(wait) {
                    Ok((w, h, rgba, pts, pw_delay, recv_ns)) => {
                        if stall_logged {
                            log::warn!(
                                "pipewire capture (media-clock) resumed after a \
                                 {stalled_secs:.0}s stall"
                            );
                        }
                        last_frame_at = std::time::Instant::now();
                        stalled_secs = 0.0;
                        stall_logged = false;
                        // Same auto-cal lag measurement as before (now via the shared
                        // `frame_lag_ms`): the median feeds `measured_offset_ms`.
                        if let Some(ms) = frame_lag_ms(pts, pw_delay, recv_ns) {
                            all_lag_samples.push(ms);
                        }
                        last = (w, h, rgba);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        stalled_secs = std::time::Instant::now()
                            .saturating_duration_since(last_frame_at)
                            .as_secs_f32();
                        if stalled_secs >= STALL_WARN_SECS && !stall_logged {
                            stall_logged = true;
                            log::error!(
                                "pipewire capture (media-clock) stalled: no video frame for \
                                 {stalled_secs:.0}s while recording — the video track will \
                                 appear frozen from here (audio continues)"
                            );
                        }
                    }
                    // The portal stream ended: the session is over regardless of
                    // the app's own `stop` (there is only ever one "segment"
                    // here, unlike the legacy path's per-pause restart).
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                // DRAGON-167: DRAIN to the freshest queued frame before feeding ticks.
                // The single `recv_timeout` above took the OLDEST queued frame (FIFO);
                // under encoder backpressure the channel backs up and that is stale by
                // seconds while the newest changes sit behind it (and get dropped as the
                // channel stays full). `latch_freshest` advances `last` to the newest
                // available frame and credits each skipped frame's lag sample, so the
                // video tracks the screen and the auto-cal median is unchanged. A quiet
                // channel leaves `last` alone (the CFR re-feed holds the timeline).
                latch_freshest(&frx, &mut last, &mut all_lag_samples);
                // DRAGON-693 (Bronze): serve the armed annotation window's snapshot request,
                // if one is outstanding. Exactly here, and exactly once per ask: `last` is the
                // freshest frame the recording has, and it is about to be written to the file,
                // so what the user draws on is literally what the video is about to show. One
                // frame copy per ARM, never per tick.
                if peek.wanted() {
                    peek.serve(last.0, last.1, &last.2);
                }
                // Feed however many video ticks the media clock owes since the
                // last feed — 0 most cycles (paused, or not yet due), a small
                // burst after a scheduling hiccup or a slow-arriving frame.
                let due = ticker.due_video_ticks(std::time::Instant::now());
                for _ in 0..due {
                    if !write_frame(
                        last.0,
                        last.1,
                        ink_paint.paint(last.0, last.1, &last.2),
                        &mut stdin,
                    ) {
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

        // Stop: the pump IS the audio drain (DRAGON-78's mic/system
        // drain-before-EOF, generalized) — join its control thread before
        // touching the video stdin, so the audio's true media end
        // (`final_media`) is known and every sample is queued to the writer
        // (whose EOFs follow as it flushes; video EOF landing first is fine —
        // both streams are fed to the same media end either way).
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

        // Feed enough FINAL ticks to cover AT LEAST the audio's true media end —
        // the mirror-image of the legacy path's stop-tail last-frame rewrite:
        // there, the video was extended PAST the drained audio so `-shortest`
        // couldn't cut it; here the audio (FIFOs) already closed with its own
        // true end, so the video must instead be extended to at least that same
        // media position, and nothing may end short of it. (`-shortest` used to
        // trim the surplus back off; DRAGON-710 removed the flag, so a tick or two
        // of surplus video is kept instead. See `encode::command`.)
        //
        // Shared stop tail (DRAGON-161): the covering-tick writes are NON-BLOCKING
        // now — with an ffmpeg that has stopped draining the video pipe without
        // closing it (which `-shortest` used to cause, and any wedge still can), a
        // plain blocking write of a
        // full-monitor frame could park in the kernel forever (the DRAGON-160 wedge,
        // reproduced headlessly). Non-blocking does NOT mean impatient: a full pipe is
        // retried on a bounded deadline (DRAGON-711), because these writes are the only
        // thing making the video as long as the audio. If they run out of that deadline
        // the loop stops — unlike this path's OLD blocking loop, which set
        // `muxer_wedged` on a covering-write failure and then DELETED the temp. Same
        // reap + salvage decision the SCK and screencopy workers run, one implementation.
        let more = ticker.ticks_to_cover(pump_out.final_media);
        let truncated = stdin.truncated();
        let stdin = stdin.into_stdin();
        let outcome = run_video_stop_tail(stdin, &mut child, &temp, muxer_wedged, truncated, more, |sd| {
            write_frame(last.0, last.1, ink_paint.paint(last.0, last.1, &last.2), sd)
        });
        stop.store(true, Ordering::Relaxed); // release the PipeWire consumer thread
        let _ = pw.join();
        outcome.map(|()| (pump_out.mic_off, pump_out.sys_off, ticker.ticks_emitted()))
    });

    let (mic_off, sys_off, frames_emitted) = scope_result?;
    if let Ok(mut g) = measured.lock() {
        *g = median_offset_ms(all_lag_samples);
    }
    // DRAGON-736: a PORTAL WINDOW cast captures the toplevel's own buffer, so the ink the user
    // drew on screen is not in the file. Read ONCE, after the stop tail, so the app has had the
    // whole session to fill it.
    let bake = ink.lock().ok().and_then(|mut g| g.take()).filter(|b| !b.is_empty());
    // Unless the painter already put it there. The app publishes the plan either way and does
    // not have to know which path ran, so the slot is drained above and simply dropped here:
    // with the ink already in the frames, a bake would draw every stroke twice AND cost a whole
    // re-encode.
    //
    // **This worker asks `painted()`, where the macOS and Windows workers ask `active()`, and
    // the difference is load-bearing.** `active()` only says the SIZE GATE admitted the
    // session. On those two platforms that is the same statement as "the ink is in the frames",
    // because every session reaching their painter is also one the app publishes a live scene
    // for. This worker also serves BRONZE (DRAGON-693), the tier for a portal window cast on a
    // session with no layer shell, and `App::publish_live_ink` deliberately publishes NOTHING
    // for it: its strokes are in the STREAM's own pixels, for the bake's identity mapping. So a
    // Bronze session the gate admitted paints zero ticks, and dropping its bake on `active()`
    // would silently cost the user the whole drawing.
    let bake = if ink_paint.painted() > 0 {
        if let Some(b) = &bake {
            log::debug!(
                "ink: {} strokes were composited onto {} captured frames, so finalize keeps \
                 its stream copy (DRAGON-724)",
                b.log.clips.len(),
                ink_paint.painted()
            );
        }
        None
    } else {
        bake
    };
    // DRAGON-739: the POINTER is a separate count on purpose. cosmic-comp only composites the
    // cursor into a toplevel capture while pointer focus is on that toplevel, and an armed ink
    // surface takes that focus, so this worker draws one itself. It has nothing to do with the
    // bake decision above and must never join it: a session that showed only a cursor painted
    // no ink, and dropping its bake would cost the user the drawing.
    if ink_paint.pointed() > 0 {
        log::debug!(
            "ink: the pointer was drawn onto {} captured frames, because an armed annotation \
             surface takes it out of the compositor's own window capture (DRAGON-739)",
            ink_paint.pointed()
        );
    }
    let job = bake.as_ref().map(|bake| super::finalize::InkJob {
        bake,
        video: (ew, eh),
        fps: fps.max(1),
        video_frames: frames_emitted,
        encoder: plan.encoder_id(),
        kbps: bitrate_kbps,
    });
    // The intervals are already on the file's OWN media timeline (the pump's FIFO
    // output IS the final audio) — no delay shift, no segment-offset mapping.
    // With `job: None` this is byte-for-byte the command finalize has always built, which is
    // what keeps an un-inked recording on this path unchanged.
    super::finalize::finalize_with_ink(
        &temp, out_path, &mic_off, &sys_off, is_hevc, metadata, job.as_ref(),
    )
}

#[cfg(test)]
mod opening_phase_tests {
    use super::frame_queue::{self, FrameBound, FRAME_CHANNEL_DEPTH};
    use super::{advance_to_tick, drain_arrivals, frame_media_tick, Frame};
    use std::collections::VecDeque;

    /// One second in nanoseconds, so the media arithmetic below reads as media time.
    const SEC: i64 = 1_000_000_000;

    /// A frame that ARRIVED at `recv_ns`, marked with `marker` in its first pixel byte so
    /// the assertions can say which one a tick was shown. `pts`/`pw_delay` are 0, which
    /// yields no lag sample (see `frame_lag_ms`) and keeps them out of the way.
    fn frame_at(marker: u8, recv_ns: i64) -> Frame {
        (4, 4, vec![marker; 4 * 4 * 4], 0, 0, recv_ns)
    }

    /// DRAGON-658: a frame belongs at the media tick its pixels were on screen for, and
    /// video is index-stamped, so that is `floor(media * fps)`. Pinned because the whole
    /// opening phase is this one arithmetic: get it wrong and frames land in ticks that
    /// had not happened yet, which is the lead-then-freeze the fix removes.
    #[test]
    fn frame_media_tick_is_the_index_stamped_slot() {
        let t0 = 5 * SEC; // an arbitrary non-zero anchor: only the delta may matter
        assert_eq!(frame_media_tick(t0, t0, 30), 0);
        // Media 33.3ms is still tick 0; tick 1 opens at exactly 1/30s.
        assert_eq!(frame_media_tick(t0 + 33_332_000, t0, 30), 0);
        assert_eq!(frame_media_tick(t0 + 33_334_000, t0, 30), 1);
        assert_eq!(frame_media_tick(t0 + 287_000_000, t0, 30), 8);
        assert_eq!(frame_media_tick(t0 + 574_000_000, t0, 30), 17);
        // fps scales the slot, and a zero fps must not divide by zero or panic.
        assert_eq!(frame_media_tick(t0 + SEC / 2, t0, 60), 30);
        assert_eq!(frame_media_tick(t0 + SEC / 2, t0, 0), 0);
        // A frame stamped before media 0 cannot exist, but must saturate rather than wrap.
        assert_eq!(frame_media_tick(t0, t0 + 100_000_000, 30), 0);
    }

    /// The DRAGON-656 hitch (ported here by DRAGON-658), reproduced headlessly: a 30fps
    /// session whose stream delivered its first frame at media 287ms, so ticks 0-7 predate
    /// any possible frame, and whose ffmpeg startup then took ~300ms, during which the
    /// PipeWire consumer queued the frames belonging to ticks 9-17.
    ///
    /// The old fixed-count burst wrote ONE image for every one of those ticks (it had
    /// dropped everything but the freshest first). Placing each frame at its own tick must
    /// instead put the duplicates on ticks 0-7, where nothing was ever captured, and give
    /// every tick from 8 on its own distinct frame.
    #[test]
    fn the_opening_places_each_frame_at_its_own_tick_not_the_earliest_free_one() {
        let fps = 30u32;
        let media0 = 7 * SEC;
        let mut last = (4u32, 4u32, vec![100u8; 4 * 4 * 4]); // the first frame, tick 8
        // What the consumer queued while ffmpeg was starting up: ticks 9..=17, one each.
        let mut pending: VecDeque<Frame> = (9u64..=17)
            .map(|t| frame_at(t as u8, media0 + (t as i64) * SEC / 30 + 2_000_000))
            .collect();

        let mut shown = Vec::new();
        for tick in 0u64..=17 {
            advance_to_tick(&mut pending, &mut last, tick, media0, fps);
            shown.push(last.2[0]);
        }

        // Ticks 0-8 all hold the first frame: media [0, first arrival] has no frame, and
        // tick 8 is where that one genuinely belongs.
        assert_eq!(&shown[0..=8], &[100u8; 9], "the head holds the first captured frame");
        // Every tick from 9 on shows the frame that arrived for it: no repeats, in order.
        assert_eq!(&shown[9..=17], &[9u8, 10, 11, 12, 13, 14, 15, 16, 17]);
        assert!(pending.is_empty(), "every delivered frame was placed");

        // And the property the bug violated: after the head, no two consecutive ticks
        // repeat an image while unplaced frames are still waiting.
        let repeats = shown[9..].windows(2).filter(|w| w[0] == w[1]).count();
        assert_eq!(repeats, 0, "no duplicate tick once real frames exist: {shown:?}");
    }

    /// The other half of the same rule: a tick must NOT be given a frame from its future.
    /// Spending queued frames one per tick regardless of when they arrived is what pulls
    /// the opening ahead of the audio and strands the ticks those frames belonged to.
    #[test]
    fn advance_to_tick_never_shows_a_frame_before_it_arrived() {
        let fps = 30u32;
        let media0 = 3 * SEC;
        let mut pending: VecDeque<Frame> =
            VecDeque::from(vec![frame_at(5, media0 + 200_000_000)]); // tick 6
        let mut last = (4u32, 4u32, vec![0u8; 4 * 4 * 4]);

        for tick in 0u64..6 {
            assert!(
                !advance_to_tick(&mut pending, &mut last, tick, media0, fps),
                "tick {tick} is before the frame arrived"
            );
            assert_eq!(last.2[0], 0, "tick {tick} must still hold the earlier frame");
        }
        assert!(advance_to_tick(&mut pending, &mut last, 6, media0, fps), "tick 6 owns it");
        assert_eq!(last.2[0], 5);
        assert!(pending.is_empty());
    }

    /// The opening's drain is a HAND-OFF, not a buffer: a frame is `w*h*4` bytes, so the
    /// pending queue is held to a bound, and what it drops is the OLDEST (most stale)
    /// content, never the newest.
    ///
    /// DRAGON-713: the bound is now the phase's, not a flat eight. Under the OPENING bound a
    /// 1s span is carried whole (which is the point: those frames are the opening ticks);
    /// under the STEADY bound the same drain caps at the depth it always had.
    #[test]
    fn drain_arrivals_holds_the_queue_to_its_bound_and_drops_the_stalest() {
        // 50ms apart, so every frame is its own 30fps tick and nothing coalesces.
        let queued = |bound: FrameBound| {
            let (tx, rx) = frame_queue::channel::<Frame>(FrameBound::opening(30));
            for i in 0u8..20 {
                tx.push(frame_at(i, i as i64 * SEC / 20));
            }
            let mut pending: VecDeque<Frame> = VecDeque::new();
            let mut lag: Vec<i64> = Vec::new();
            drain_arrivals(&rx, &mut pending, &mut lag, bound);
            pending
        };
        let opening = queued(FrameBound::opening(30));
        assert_eq!(opening.len(), 20, "the opening bound carries the whole span");
        assert_eq!(opening.back().expect("non-empty").2[0], 19);

        let steady = queued(FrameBound::steady(30));
        assert_eq!(steady.len(), FRAME_CHANNEL_DEPTH, "capped at the steady depth");
        assert_eq!(steady.front().expect("non-empty").2[0], 12, "the stalest went first");
        assert_eq!(steady.back().expect("non-empty").2[0], 19, "the newest is always kept");
    }

    /// Every DRAINED frame contributes its own lag sample, exactly as `latch_freshest`
    /// credits them, including the ones the opening phase later drops for staleness. Each
    /// arrival is an independent latency observation, and `median_offset_ms` needs five of
    /// them before it will report anything at all.
    #[test]
    fn drain_arrivals_credits_every_drained_frames_lag_sample() {
        let (tx, rx) = frame_queue::channel::<Frame>(FrameBound::opening(30));
        let now = super::monotonic_ns();
        for i in 0u8..6 {
            // A pts ~100ms in the past gives a ~100ms sample, inside the trusted band. The
            // recv stamps are 50ms apart, so these are six queue entries.
            tx.push((4, 4, vec![i; 4 * 4 * 4], now - 100_000_000, 0, i as i64 * 50_000_000));
        }
        let mut pending: VecDeque<Frame> = VecDeque::new();
        let mut lag: Vec<i64> = Vec::new();
        drain_arrivals(&rx, &mut pending, &mut lag, FrameBound::opening(30));
        assert_eq!(lag.len(), 6, "every drained frame is a sample, not just the kept one");
        assert!(lag.iter().all(|&m| (1..=2000).contains(&m)), "plausible latencies: {lag:?}");
        assert_eq!(pending.len(), 6, "six distinct ticks, six entries");
    }

    /// What DRAGON-713's coalescing costs the lag measurement, stated so nobody has to
    /// rediscover it: arrivals that share a media tick become ONE entry in the capture queue,
    /// so the tick contributes ONE sample instead of several. That is a sample the recording
    /// actually fed, and the ones it replaces were never going to be shown. A 30fps session
    /// still yields ~30 samples a second against the five `median_offset_ms` needs.
    #[test]
    fn coalesced_arrivals_contribute_one_lag_sample_for_the_tick() {
        let (tx, rx) = frame_queue::channel::<Frame>(FrameBound::opening(30));
        let now = super::monotonic_ns();
        for i in 0u8..6 {
            // All six inside one 30fps tick (4ms apart).
            tx.push((4, 4, vec![i; 4 * 4 * 4], now - 100_000_000, 0, i as i64 * 4_000_000));
        }
        let mut pending: VecDeque<Frame> = VecDeque::new();
        let mut lag: Vec<i64> = Vec::new();
        drain_arrivals(&rx, &mut pending, &mut lag, FrameBound::opening(30));
        assert_eq!(pending.len(), 1, "one media tick, one entry");
        assert_eq!(pending[0].2[0], 5, "carrying the freshest pixels of that tick");
        assert_eq!(lag.len(), 1, "and one sample, the kept frame's");
        assert!(lag.iter().all(|&m| (1..=2000).contains(&m)), "plausible latency: {lag:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::frame_queue::{self, FrameBound};
    use super::{latch_freshest, Frame};

    /// Build a PipeWire-shaped frame whose first pixel byte is `marker` (so we can tell
    /// which one was latched) and whose `pts` is 0 (so `frame_lag_ms` uses the
    /// delay+transit form; here delay is 0 and the recv stamp is a small number far in the
    /// monotonic past, giving no valid sample, kept out of the way of the pixel-latch
    /// assertions). The stamps are 50ms apart, so each frame is its own 30fps tick and the
    /// queue holds them as separate entries rather than coalescing them.
    fn frame(marker: u8) -> Frame {
        (4, 4, vec![marker; 4 * 4 * 4], 0, 0, marker as i64 * 50_000_000)
    }

    /// The DRAGON-167 invariant, headless (the Linux twin of the mac SCK
    /// `latch_freshest_takes_the_newest_queued_frame`): given several frames already
    /// queued in the bounded capture queue, as happens the moment a slow encoder
    /// backpressures the video write and the PipeWire consumer keeps pushing — the worker
    /// MUST latch the FRESHEST one, not the oldest. A bare single-recv consumes the oldest
    /// (the queue is FIFO), which is exactly the stale-frame feed that froze the
    /// recording. Pins the fix with no portal, PipeWire, or ffmpeg.
    #[test]
    fn latch_freshest_takes_the_newest_queued_frame() {
        let (tx, rx) = frame_queue::channel::<Frame>(FrameBound::steady(30));
        for i in 0u8..8 {
            tx.push(frame(i));
        }
        let mut last: (u32, u32, Vec<u8>) = (4, 4, vec![255u8; 4 * 4 * 4]);
        let mut lag: Vec<i64> = Vec::new();
        assert!(latch_freshest(&rx, &mut last, &mut lag), "consumed at least one frame");
        assert_eq!(last.2[0], 7, "must latch the FRESHEST (last-pushed) frame, not the oldest");
        // Drained, so the queue is empty again rather than holding stale content the next
        // push would have to evict.
        assert!(!latch_freshest(&rx, &mut last, &mut lag), "an empty queue leaves `last` untouched");
        assert_eq!(last.2[0], 7, "an empty drain must not disturb the held frame");
    }

    /// An empty queue (a static screen / not-yet-delivered: the PipeWire consumer sent
    /// nothing) must leave the held frame alone so the CFR loop re-feeds it — the "a tick
    /// with no fresh frame re-feeds the last" half of the contract.
    #[test]
    fn latch_freshest_holds_last_when_nothing_queued() {
        let (_tx, rx) = frame_queue::channel::<Frame>(FrameBound::steady(30));
        let mut last: (u32, u32, Vec<u8>) = (2, 2, vec![9u8; 2 * 2 * 4]);
        let mut lag: Vec<i64> = Vec::new();
        assert!(!latch_freshest(&rx, &mut last, &mut lag), "nothing to consume");
        assert_eq!(last.2[0], 9, "held frame is unchanged");
        assert!(lag.is_empty(), "no frames means no lag samples");
    }

    /// pts/lag semantics are PRESERVED across a multi-frame drain (the DRAGON-167 care
    /// point): every DRAINED frame — not just the freshest whose pixels we keep — must
    /// still contribute its own lag sample, because each arrived frame is an independent,
    /// equally-valid latency observation for the median. A drain that silently dropped the
    /// intermediate frames' timing would starve `median_offset_ms` (needs ≥5 samples) and
    /// skew auto-calibration. Uses `pts > 0` frames with a monotonic-past timestamp so
    /// `frame_lag_ms` yields a valid (positive, ≤2000ms) sample for each.
    #[test]
    fn latch_freshest_credits_every_drained_frames_lag_sample() {
        let (tx, rx) = frame_queue::channel::<Frame>(FrameBound::steady(30));
        // A pts ~100ms in the past (relative to `monotonic_ns()` at drain time) yields a
        // ~100ms lag sample per frame; well inside the trusted 1..=2000ms band.
        let now = super::monotonic_ns();
        for i in 0u8..6 {
            // (w, h, rgba, pts, pw_delay, recv_ns): a valid past pts, so the lag form is
            // `monotonic_ns() - pts`, positive and small. The recv stamps are 50ms apart,
            // so these are six queue entries and not one coalesced tick.
            tx.push((4, 4, vec![i; 4 * 4 * 4], now - 100_000_000, 0, i as i64 * 50_000_000));
        }
        let mut last: (u32, u32, Vec<u8>) = (4, 4, vec![0u8; 4 * 4 * 4]);
        let mut lag: Vec<i64> = Vec::new();
        assert!(latch_freshest(&rx, &mut last, &mut lag));
        assert_eq!(last.2[0], 5, "freshest pixels latched");
        assert_eq!(
            lag.len(),
            6,
            "EVERY drained frame must contribute a lag sample (got {}), not just the kept one",
            lag.len()
        );
        assert!(
            lag.iter().all(|&m| (1..=2000).contains(&m)),
            "each sample is a plausible latency: {lag:?}"
        );
    }

    /// The freeze dynamic in miniature, deterministic and headless (the Linux twin of the
    /// mac SCK `draining_keeps_the_consumed_frame_recent_under_backpressure`): a producer
    /// runs far ahead of a SLOW consumer: the loaded-encoder case where the video write
    /// throttles the loop. The stale-feed bug consumed ONE frame per iteration (the oldest
    /// surviving one), so the "video" lagged behind the producer by the whole queue depth;
    /// `latch_freshest` consumes the newest instead. Pins the exact staleness the fix
    /// removes, with no portal/PipeWire/ffmpeg.
    ///
    /// DRAGON-713 made this deterministic. It used to drive a real thread against a blocking
    /// `SyncSender`, because the depth the consumer lagged by depended on how far a blocking
    /// producer got. The sink never blocks now (a full queue evicts its oldest frame), so the
    /// producer's whole burst can simply be pushed up front and the gap is exact.
    #[test]
    fn draining_keeps_the_consumed_frame_recent_under_backpressure() {
        fn seq_frame(seq: u32) -> Frame {
            let mut px = vec![0u8; 4 * 4 * 4];
            px[0..4].copy_from_slice(&seq.to_le_bytes());
            // 50ms apart: one queue entry per frame, no coalescing.
            (4, 4, px, 0, 0, seq as i64 * 50_000_000)
        }
        fn seq_of(f: &(u32, u32, Vec<u8>)) -> u32 {
            u32::from_le_bytes([f.2[0], f.2[1], f.2[2], f.2[3]])
        }

        // Run the SAME producer against two consumers: `drain=false` mimics the old
        // single-recv (consume one, oldest first); `drain=true` uses `latch_freshest`.
        // Return the gap between what the producer had sent and what the consumer holds.
        let gap = |drain: bool| -> u32 {
            let (tx, rx) = frame_queue::channel::<Frame>(FrameBound::steady(30));
            for seq in 0..400u32 {
                tx.push(seq_frame(seq));
            }
            let mut last: (u32, u32, Vec<u8>) = (4, 4, vec![0u8; 4 * 4 * 4]);
            let mut lag: Vec<i64> = Vec::new();
            if let Ok((w, h, rgba, ..)) = rx.recv_timeout(std::time::Duration::from_millis(50)) {
                last = (w, h, rgba);
            }
            if drain {
                latch_freshest(&rx, &mut last, &mut lag);
            }
            399 - seq_of(&last)
        };

        let stale_gap = gap(false);
        let fresh_gap = gap(true);
        assert_eq!(fresh_gap, 0, "draining lands on the newest frame the producer sent");
        assert!(
            stale_gap > 0,
            "the single-recv consumer must be measurably behind: stale_gap={stale_gap}"
        );
        // And the DRAGON-713 half: even the STALE consumer is inside the queue depth of the
        // newest frame, because the queue evicts its oldest end. The container this replaces
        // refused the incoming frame instead, so a 400-frame burst left the consumer holding
        // frame 0 and every later frame was discarded.
        assert!(
            stale_gap < super::frame_queue::FRAME_CHANNEL_DEPTH as u32,
            "the queue holds the NEWEST eight: stale_gap={stale_gap}"
        );
    }
}
