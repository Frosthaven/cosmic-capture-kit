//! PipeWire portal recording (CPU/readback path): consume a portal ScreenCast
//! stream and pipe its frames to ffmpeg.

use super::owned::{
    make_frame_writer, mark_first_frame, mark_settled, media_zero, run_video_stop_tail,
    try_start_owned_audio,
    MuteIntervals, OwnedAudioStart,
};
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

/// How many captured frames the bounded capture channel holds. Small on purpose: one
/// frame is `w*h*4` bytes (tens of megabytes at 4K), and depth here buys latency, not
/// smoothness: the media clock decides how many ticks are owed regardless of how much is
/// queued. Also caps the opening phase's pending queue, which drains through the same
/// frames.
const FRAME_CHANNEL_DEPTH: usize = 8;

/// How long the opening phase may spend getting level with the media clock before it
/// hands over to the steady-state loop. A BOUND, not a target (DRAGON-118: nothing in the
/// record path waits unboundedly): the phase normally ends within a few hundred
/// milliseconds, the moment the clock owes it nothing. It can only fail to get level if
/// the encoder cannot take frames as fast as they come due, and that is the steady-state
/// loop's case to handle, not the opening's. Deliberately a LOCAL constant with the same
/// value as the mac SCK and Windows WGC workers' own: these are four independent capture
/// shapes, and hoisting one shared number would tie their tuning together for nothing.
const OPENING_COVER_BUDGET: Duration = Duration::from_secs(2);

/// The A/V lag (ms) a single delivered PipeWire `frame` implies — how long it took to
/// reach the encoder, measured EXACTLY as the in-loop path always has (the
/// capture-timestamp form when the frame carried a monotonic `pts`, else PipeWire's
/// reported source delay plus the channel transit since `recv_ns`). Factored out so the
/// drain below can sample EVERY frame it consumes, not just the one whose pixels it
/// keeps: each arrived frame is an independent, equally-valid latency observation for
/// the median (`median_offset_ms`), so draining stale frames must not silently drop
/// their lag samples. Returns `None` for a sample outside the trusted `1..=2000` ms band
/// (the same filter the loop applied inline).
fn frame_lag_ms(pts: i64, pw_delay: i64, recv_ns: i64) -> Option<i64> {
    let lag_ms = if pts > 0 {
        (monotonic_ns() - pts) / 1_000_000
    } else {
        (pw_delay + (monotonic_ns() - recv_ns)) / 1_000_000
    };
    (lag_ms > 0 && lag_ms <= 2000).then_some(lag_ms)
}

/// Drain `rx` to the FRESHEST currently-queued frame, updating `last` in place to its
/// dimensions + pixels and pushing EVERY consumed frame's lag sample into `lag_samples`
/// (see [`frame_lag_ms`]); report whether ANY frame was consumed (DRAGON-167). This is
/// the CFR contract's "feed the freshest frame" made explicit and headlessly testable,
/// generalizing the mac SCK `latch_freshest` (DRAGON-164) to the PipeWire tuple's
/// per-frame timing state:
///
/// The capture channel is bounded (8 deep). When a slow encoder backpressures the
/// blocking video write, the loop parks IN that write while the PipeWire consumer thread
/// keeps pushing — the channel fills and the consumer DROPS the newest frames
/// (`try_send` fails), leaving only stale ones. Consuming a single frame per tick (the
/// loop's bare `recv_timeout`) would then feed the OLDEST stale frame (the channel is
/// FIFO), so the video shows seconds-old content while the newest changes are discarded
/// upstream — the reported near-total freeze on a loaded capture (the Linux twin of the
/// mac SCK bug). Draining to the freshest holds `last` current every tick and keeps the
/// channel from staying full (so the consumer stops dropping fresh frames).
///
/// pts/lag semantics are PRESERVED, not corrupted: the intermediate frames' PIXELS are
/// the only thing discarded (they're stale by definition — a newer frame supersedes
/// them); their TIMING each contributes a lag sample exactly as if the old single-recv
/// loop had processed them one by one, so the median offset is unchanged (arguably more
/// robust — more samples). The media clock itself is index-stamped and never reads these
/// timestamps, so dropping stale pixels cannot skew it.
///
/// Returns `false` only when the channel was empty (nothing new / not yet delivered),
/// leaving `last` untouched so the caller re-feeds it to hold CFR.
fn latch_freshest(
    rx: &std::sync::mpsc::Receiver<Frame>,
    last: &mut (u32, u32, Vec<u8>),
    lag_samples: &mut Vec<i64>,
) -> bool {
    let mut got = false;
    while let Ok((w, h, rgba, pts, pw_delay, recv_ns)) = rx.try_recv() {
        if let Some(ms) = frame_lag_ms(pts, pw_delay, recv_ns) {
            lag_samples.push(ms);
        }
        *last = (w, h, rgba);
        got = true;
    }
    got
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

/// Drain every frame currently queued in `rx` into `pending`, crediting each one's lag
/// sample exactly as [`latch_freshest`] does, and holding the queue to
/// [`FRAME_CHANNEL_DEPTH`] by dropping from the OLDEST end (the most stale content).
///
/// The opening phase's counterpart to `latch_freshest`: it KEEPS the arrived frames
/// instead of collapsing them onto the newest, because each of them belongs at a media
/// tick of its own (see [`advance_to_tick`]).
fn drain_arrivals(
    rx: &std::sync::mpsc::Receiver<Frame>,
    pending: &mut VecDeque<Frame>,
    lag_samples: &mut Vec<i64>,
) {
    while let Ok(f) = rx.try_recv() {
        if let Some(ms) = frame_lag_ms(f.3, f.4, f.5) {
            lag_samples.push(ms);
        }
        pending.push_back(f);
    }
    while pending.len() > FRAME_CHANNEL_DEPTH {
        pending.pop_front();
    }
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
    settled_at: &Mutex<Option<Instant>>,
    metadata: &str,
) -> Result<PathBuf, String> {
    // Same PipeWire consumer shape as ever: a small jitter-buffered channel, dropping
    // (never blocking) when the encoder side is behind, discarding frames while
    // paused — nothing is captured then; the pump's video ticks independently freeze
    // for the same span (see `super::pump::VideoTicker`). It is the FIRST thing the
    // worker starts, and nothing audio-side exists until it has delivered a frame
    // (DRAGON-658).
    let (ftx, frx) = std::sync::mpsc::sync_channel::<Frame>(FRAME_CHANNEL_DEPTH);
    let pw_stop = stop.clone();
    let pw_paused = paused.clone();
    let pw = std::thread::spawn(move || {
        let mut dropped: u64 = 0;
        crate::platform::pipewire::consume_frames(fd, node_id, crop, pw_stop, move |rgba, w, h, pts, pw_delay| {
            if pw_paused.load(Ordering::Relaxed) {
                return;
            }
            if ftx.try_send((w, h, rgba, pts, pw_delay, monotonic_ns())).is_err() {
                dropped += 1;
                if dropped == 1 || dropped.is_multiple_of(60) {
                    log::warn!(
                        "pipewire capture (media-clock): encoder backlog — {dropped} frames \
                         dropped so far (video will look choppy if this keeps up)"
                    );
                }
            }
        })
    });
    record_pipewire_owned(
        fps, preferred_encoder, encoder_hint, presets, mic, system_audio, bitrate_kbps,
        audio_offset_ms, auto_device_compensation, max_res, start_gate, out_path, stop, paused,
        events, measured, dims, warm_at, settled_at, metadata, frx, pw,
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
    settled_at: &Mutex<Option<Instant>>,
    metadata: &str,
    frx: std::sync::mpsc::Receiver<Frame>,
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
        let _ = pw.join();
        "no frames from the PipeWire stream".to_string()
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
                    if waited_s >= 10 || stop.load(Ordering::Relaxed) {
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
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return Err(owned_start_failure(
            &stop, pw, mic_tap, monitor, &mic_fifo_path, &sys_fifo_path,
            "ffmpeg stdin unavailable".to_string(),
        ));
    };

    let mut write_frame = make_frame_writer(w0, h0, nv12);
    let mut last: (u32, u32, Vec<u8>) = (w0, h0, first);
    // NOT drained to the freshest here (DRAGON-658). Whatever the consumer queued while
    // ffmpeg was starting up (encoder + FIFO opens, which do not read the video pipe) is
    // the session's OPENING, and the opening phase below places each of those frames at
    // the media tick it actually belongs to instead of throwing all but the newest away.
    // Media 0 is HERE, not at the audio pre-flight (DRAGON-672): the file begins where
    // the warming spinner clears, which is the same instant the user is told recording
    // started. See `owned::media_zero` for the whole argument.
    let session_start = media_zero(capture_start, start_gate.as_deref(), &mic_rx, &sys_rx);
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
    let scope_result: Result<(MuteIntervals, MuteIntervals), String> = std::thread::scope(|scope| {
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
                drain_arrivals(&frx, &mut pending, &mut all_lag_samples);
                if advance_to_tick(&mut pending, &mut last, tick, session_start_ns, fps.max(1)) {
                    ticks_from_capture += 1;
                }
                if !write_frame(last.0, last.1, &last.2, &mut stdin) {
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
            mark_settled(settled_at, Instant::now());
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
                match frx.recv_timeout(std::time::Duration::from_millis(200)) {
                    Ok((w, h, rgba, pts, pw_delay, recv_ns)) => {
                        if stall_logged {
                            log::warn!(
                                "pipewire capture (media-clock) resumed after a \
                                 {stalled_secs:.0}s stall"
                            );
                        }
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
                        stalled_secs += 0.2;
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
                // Feed however many video ticks the media clock owes since the
                // last feed — 0 most cycles (paused, or not yet due), a small
                // burst after a scheduling hiccup or a slow-arriving frame.
                let due = ticker.due_video_ticks(std::time::Instant::now());
                for _ in 0..due {
                    if !write_frame(last.0, last.1, &last.2, &mut stdin) {
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
        // (whose EOFs follow as it flushes; `-shortest` ends the mux at the
        // shorter STREAM, so video EOF landing first is fine — both streams are
        // fed to the same media end either way).
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
        // media position, so `-shortest` trims video down to audio's end rather
        // than truncating audio.
        //
        // Shared stop tail (DRAGON-161): the covering-tick writes are NON-BLOCKING
        // now — an ffmpeg whose audio was the shorter `-shortest` stream stops
        // draining the video pipe without closing it, so a plain blocking write of a
        // full-monitor frame could park in the kernel forever (the DRAGON-160 wedge,
        // reproduced headlessly). A covering-write failure is BENIGN (the video is
        // already long enough), so — unlike this path's OLD blocking loop, which set
        // `muxer_wedged` on a covering-write failure and then DELETED the temp — it
        // just stops the loop. Same reap + salvage decision the SCK and screencopy
        // workers run, one implementation.
        let more = ticker.ticks_to_cover(pump_out.final_media);
        let outcome = run_video_stop_tail(stdin, &mut child, &temp, muxer_wedged, more, |sd| {
            write_frame(last.0, last.1, &last.2, sd)
        });
        stop.store(true, Ordering::Relaxed); // release the PipeWire consumer thread
        let _ = pw.join();
        outcome.map(|()| (pump_out.mic_off, pump_out.sys_off))
    });

    let (mic_off, sys_off) = scope_result?;
    if let Ok(mut g) = measured.lock() {
        *g = median_offset_ms(all_lag_samples);
    }
    // The intervals are already on the file's OWN media timeline (the pump's FIFO
    // output IS the final audio) — no delay shift, no segment-offset mapping.
    super::finalize::finalize_with_intervals(&temp, out_path, &mic_off, &sys_off, is_hevc, metadata)
}

#[cfg(test)]
mod opening_phase_tests {
    use super::{advance_to_tick, drain_arrivals, frame_media_tick, Frame, FRAME_CHANNEL_DEPTH};
    use std::collections::VecDeque;
    use std::sync::mpsc::sync_channel;

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
    /// pending queue may never grow past what the capture channel itself would hold, and
    /// what it drops is the OLDEST (most stale) content, never the newest.
    #[test]
    fn drain_arrivals_holds_the_queue_to_the_channel_depth_and_drops_the_stalest() {
        let (tx, rx) = sync_channel::<Frame>(64);
        for i in 0u8..20 {
            tx.try_send(frame_at(i, i as i64 * SEC / 30)).expect("room for 20");
        }
        let mut pending: VecDeque<Frame> = VecDeque::new();
        let mut lag: Vec<i64> = Vec::new();
        drain_arrivals(&rx, &mut pending, &mut lag);
        assert_eq!(pending.len(), FRAME_CHANNEL_DEPTH, "capped at the channel's own depth");
        assert_eq!(pending.front().expect("non-empty").2[0], 12, "the stalest went first");
        assert_eq!(pending.back().expect("non-empty").2[0], 19, "the newest is always kept");
    }

    /// Every ARRIVED frame still contributes its own lag sample, exactly as `latch_freshest`
    /// credits them, including the ones the opening phase later drops for staleness. Each
    /// arrival is an independent latency observation, and `median_offset_ms` needs five of
    /// them before it will report anything at all.
    #[test]
    fn drain_arrivals_credits_every_arrived_frames_lag_sample() {
        let (tx, rx) = sync_channel::<Frame>(8);
        let now = super::monotonic_ns();
        for i in 0u8..6 {
            // A pts ~100ms in the past gives a ~100ms sample, inside the trusted band.
            tx.try_send((4, 4, vec![i; 4 * 4 * 4], now - 100_000_000, 0, 0)).expect("room");
        }
        let mut pending: VecDeque<Frame> = VecDeque::new();
        let mut lag: Vec<i64> = Vec::new();
        drain_arrivals(&rx, &mut pending, &mut lag);
        assert_eq!(lag.len(), 6, "every arrival is a sample, not just the kept one");
        assert!(lag.iter().all(|&m| (1..=2000).contains(&m)), "plausible latencies: {lag:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::{latch_freshest, Frame};
    use std::sync::mpsc::sync_channel;

    /// Build a PipeWire-shaped frame whose first pixel byte is `marker` (so we can tell
    /// which one was latched) and whose `pts` is 0 (so `frame_lag_ms` uses the
    /// delay+transit form; here delay/recv are 0, giving no valid sample — kept out of the
    /// way of the pixel-latch assertions).
    fn frame(marker: u8) -> Frame {
        (4, 4, vec![marker; 4 * 4 * 4], 0, 0, 0)
    }

    /// The DRAGON-167 invariant, headless (the Linux twin of the mac SCK
    /// `latch_freshest_takes_the_newest_queued_frame`): given several frames already
    /// queued in the bounded capture channel — as happens the moment a slow encoder
    /// backpressures the video write and the PipeWire consumer keeps pushing — the worker
    /// MUST latch the FRESHEST one, not the oldest. A bare single-recv consumes the oldest
    /// (the channel is FIFO), which is exactly the stale-frame feed that froze the
    /// recording. Pins the fix with no portal, PipeWire, or ffmpeg.
    #[test]
    fn latch_freshest_takes_the_newest_queued_frame() {
        let (tx, rx) = sync_channel::<Frame>(8);
        for i in 0u8..8 {
            tx.try_send(frame(i)).expect("channel has room for 8");
        }
        let mut last: (u32, u32, Vec<u8>) = (4, 4, vec![255u8; 4 * 4 * 4]);
        let mut lag: Vec<i64> = Vec::new();
        assert!(latch_freshest(&rx, &mut last, &mut lag), "consumed at least one frame");
        assert_eq!(last.2[0], 7, "must latch the FRESHEST (last-pushed) frame, not the oldest");
        // Drained, so the consumer can push fresh frames again instead of hitting a full
        // channel and dropping the newest.
        assert!(!latch_freshest(&rx, &mut last, &mut lag), "empty channel leaves `last` untouched");
        assert_eq!(last.2[0], 7, "an empty drain must not disturb the held frame");
    }

    /// An empty channel (a static screen / not-yet-delivered: the PipeWire consumer sent
    /// nothing) must leave the held frame alone so the CFR loop re-feeds it — the "a tick
    /// with no fresh frame re-feeds the last" half of the contract.
    #[test]
    fn latch_freshest_holds_last_when_nothing_queued() {
        let (_tx, rx) = sync_channel::<Frame>(8);
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
        let (tx, rx) = sync_channel::<Frame>(8);
        // A pts ~100ms in the past (relative to `monotonic_ns()` at drain time) yields a
        // ~100ms lag sample per frame; well inside the trusted 1..=2000ms band.
        let now = super::monotonic_ns();
        for i in 0u8..6 {
            // (w, h, rgba, pts, pw_delay, recv_ns): a valid past pts, so the lag form is
            // `monotonic_ns() - pts` — positive and small.
            tx.try_send((4, 4, vec![i; 4 * 4 * 4], now - 100_000_000, 0, 0))
                .expect("room");
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
    /// fills the bounded channel faster than a SLOW consumer drains it — the loaded-encoder
    /// case where the blocking video write throttles the loop. The stale-feed bug consumed
    /// ONE frame per iteration (the oldest), so the "video" lagged far behind the producer;
    /// `latch_freshest` keeps the consumed frame close to the newest sent. Pins the exact
    /// staleness the fix removes, with no portal/PipeWire/ffmpeg.
    #[test]
    fn draining_keeps_the_consumed_frame_recent_under_backpressure() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        fn seq_frame(seq: u32) -> Frame {
            let mut px = vec![0u8; 4 * 4 * 4];
            px[0..4].copy_from_slice(&seq.to_le_bytes());
            (4, 4, px, 0, 0, 0)
        }
        fn seq_of(f: &(u32, u32, Vec<u8>)) -> u32 {
            u32::from_le_bytes([f.2[0], f.2[1], f.2[2], f.2[3]])
        }

        // Run the SAME producer against two consumers: `drain=false` mimics the old
        // single-recv (consume one, oldest first); `drain=true` uses `latch_freshest`.
        // Return the worst gap between what the producer had sent and what the consumer
        // holds (staleness).
        let worst_gap = |drain: bool| -> u32 {
            let (tx, rx) = sync_channel::<Frame>(8);
            let sent = Arc::new(AtomicU32::new(0));
            let sent_p = sent.clone();
            let prod = std::thread::spawn(move || {
                for seq in 0..400u32 {
                    if tx.send(seq_frame(seq)).is_err() {
                        break;
                    }
                    sent_p.store(seq, Ordering::Relaxed);
                }
            });
            let mut last: (u32, u32, Vec<u8>) = (4, 4, vec![0u8; 4 * 4 * 4]);
            let mut lag: Vec<i64> = Vec::new();
            let mut worst = 0u32;
            for _ in 0..60 {
                if let Ok((w, h, rgba, ..)) = rx.recv() {
                    last = (w, h, rgba);
                }
                if drain {
                    latch_freshest(&rx, &mut last, &mut lag);
                }
                let ahead = sent.load(Ordering::Relaxed).saturating_sub(seq_of(&last));
                worst = worst.max(ahead);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            drop(rx);
            let _ = prod.join();
            worst
        };

        let stale_gap = worst_gap(false);
        let fresh_gap = worst_gap(true);
        assert!(
            fresh_gap * 4 < stale_gap.max(1),
            "draining should stay far fresher: fresh_gap={fresh_gap} stale_gap={stale_gap}"
        );
    }
}
