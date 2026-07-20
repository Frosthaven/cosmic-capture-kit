//! System-audio monitor capture via the libpulse async client (DRAGON-123): a
//! background thread records a PulseAudio/PipeWire-Pulse monitor source directly,
//! instead of shelling out to ffmpeg's `-f pulse` (`pa_simple`) input. This module is
//! ONLY the reusable capture client; `record::pump` (DRAGON-125) is the consumer
//! that feeds its chunks straight to the mixer as part of the media-clock owned
//! recording pipeline. (DRAGON-127 retired the `system_relay` sibling module that
//! used to wire this client into the legacy recording path's FIFO, along with that
//! path.)
//!
//! ## Why capture it ourselves
//!
//! The eventual mixer needs sample-accurate control over the system-audio stream (to
//! mix/gate/delay it in-process instead of leaning on ffmpeg filtergraphs at
//! finalize). `pa_simple` (what ffmpeg's `-f pulse` input uses) hands over decoded
//! PCM with no useful per-chunk timing; the async client API used here gives us the
//! raw stream plus `pa_stream_get_latency`, on the same threaded-mainloop
//! foundation as [`crate::audio::monitor_latency`]'s device-latency probe (its shared
//! FFI surface now lives in [`crate::audio::pulse_ffi`], so neither module
//! re-declares the `extern` block or re-derives the bounded-wait/teardown
//! discipline).
//!
//! ## What a chunk means — the contiguous timing model (DRAGON-122 integration)
//!
//! Each [`CaptureChunk`] is one `pa_stream_peek`/`pa_stream_drop` cycle's worth of
//! interleaved stereo f32 samples at 48 kHz (pulse negotiates/converts to this exact
//! spec server-side), stamped with [`CaptureChunk::capture_wall`] = the instant its
//! FIRST sample was captured. That instant comes from ONE wall anchor per stream
//! ([`StreamAnchor`]), not from each chunk's own arrival: the first delivery
//! back-dates its arrival by its own duration (the OBS convention) to establish the
//! anchor, and every later chunk is stamped `anchor + delivered_frames/48000` — for
//! a gapless PCM stream the sample count IS the clock (holes deliver zeroed samples,
//! so the count never lies).
//!
//! Per-chunk wall stamping (`arrival − duration` on EVERY chunk — the
//! pre-integration model) is wrong for a consumer that places chunks
//! sample-accurately: with 25 ms fragments (DRAGON-126), scheduler/IPC jitter of a
//! few ms makes consecutive stamps disagree with the stream's own sample count
//! ~40×/second, and `mixer::Track` then micro-truncates the overlaps and
//! silence-fills the micro-gaps — audible garbling (measured: ~268 discontinuities
//! in 6.5 s of a pure tone; ~0 under this model). Arrival timing still matters, but
//! only as a MONITOR: each delivery's implied anchor is compared against the live
//! one, and only a drift beyond [`StreamAnchor::REANCHOR_THRESHOLD_SECS`] (a genuine
//! discontinuity: a sound-server hiccup, a stalled stretch) re-anchors the stream —
//! logged loudly, never chopped silently. The stream's device latency is NOT folded
//! in here; it is a constant audible-time shift `record::pump` applies once, per
//! session, via its own latch (DRAGON-119/125).
//!
//! ## Teardown discipline (DRAGON-118, same as the latency probe)
//!
//! [`MonitorCapture::stop`] bounded-joins its thread (≤2s) and detaches it (keeping
//! whatever was already measured/counted) rather than ever risking an unbounded wait
//! on a wedged sound server.
//!
//! The whole real implementation is `#[cfg(target_os = "linux")]`; a non-Linux stub
//! returns `None` from `start`, matching [`crate::audio::MonitorLatencyProbe`].

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Delivered-chunk channel capacity. The read callback must never block, so this is
/// sized generously: ~2ms is a plausible worst-case-small average callback
/// granularity for a negotiated low-latency record stream, so 4096 chunks is
/// ≈ 4096 × 2ms = 8.19s of buffering headroom before a stalled consumer starts
/// costing dropped chunks instead of unbounded memory growth.
const CAPTURE_CHANNEL_CAPACITY: usize = 4096;

/// A secondary, capture-thread-side consumer of every delivered chunk's samples
/// (real data AND hole silence), called BEFORE the chunk is sent to the primary
/// consumer — the seam DRAGON-128 uses to feed the AEC far-end reference off the
/// SAME capture that records the system track (see
/// `crate::audio::filters::aec::FarEndFeeder`). Runs inside the pulse read
/// callback, so it must never block: bounded, drop-oldest buffering only.
pub(crate) type CaptureTee = Box<dyn FnMut(&[f32]) + Send>;

/// One delivered chunk of monitor audio.
#[derive(Debug)]
pub(crate) struct CaptureChunk {
    /// Interleaved stereo f32 samples (`[L, R, L, R, ...]`) at 48 kHz.
    pub(crate) samples: Vec<f32>,
    /// When the FIRST sample of this chunk was captured, on the stream's contiguous
    /// clock (see the module doc's timing model): the stream's [`StreamAnchor`] plus
    /// the frames delivered before this chunk — NOT this chunk's own arrival time,
    /// whose jitter must never reach placement.
    pub(crate) capture_wall: Instant,
}

/// [`MonitorCapture::stop`]'s summary of a finished capture run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CaptureStats {
    /// Median of the signed device-latency samples collected during the run (ms),
    /// via the same [`crate::audio::monitor_latency`] machinery the A/V-sync probe
    /// uses. 0.0 fail-open (no samples, all non-finite, or a suspended/virtual sink).
    pub(crate) device_latency_ms: f64,
    /// Chunks the consumer was too slow to receive (the channel was full).
    pub(crate) dropped_chunks: u64,
    /// Peak implied delivery lag observed during the run (seconds): how far behind real
    /// time the delivered sample count fell (wall elapsed since the first chunk minus
    /// `frames_delivered / 48000`). A healthy negotiated stream stays near zero; a large
    /// value means the server was buffering chunks stale (the DRAGON-126 class of bug —
    /// the `w0` sync anchor over-states capture time by roughly this much). 0.0 if
    /// nothing was ever delivered.
    pub(crate) peak_lag_secs: f64,
}

/// f32le byte slice → samples: the exact conversion idiom `clean_mic.rs`'s PCM reader
/// threads use post-DRAGON-115 (`as_chunks::<4>()` + `f32::from_le_bytes`).
/// Linux-only: consumed by the `pulse` capture callback (and its tests).
#[cfg(target_os = "linux")]
pub(super) fn f32le_to_samples(bytes: &[u8]) -> Vec<f32> {
    bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

/// How many (zeroed) samples a HOLE of `nbytes` becomes — see
/// `pulse::capture_read_cb`'s case (b).
/// Linux-only: consumed by the `pulse` capture callback (and its tests).
#[cfg(target_os = "linux")]
fn hole_sample_count(nbytes: usize) -> usize {
    nbytes / 4
}

/// Back-date `now` to when this chunk's FIRST sample was actually captured, given its
/// frame count (stereo pairs) at 48 kHz.
fn chunk_capture_wall(frames: usize, now: Instant) -> Instant {
    now - Duration::from_secs_f64(frames as f64 / 48000.0)
}

/// One stream's contiguous wall-clock stamping state — the module doc's timing model,
/// shared by this capture client and [`crate::audio::clean_mic`]'s tap reader so both
/// audio sources stamp identically: anchor ONCE (first delivery, back-dated by its own
/// duration), then stamp every chunk at `anchor + frames_delivered/48000`, using
/// arrival times only to detect a genuine discontinuity worth re-anchoring on. Pure
/// (the caller passes `now`), so the placement math is unit-testable without pulse.
pub(super) struct StreamAnchor {
    /// Wall instant of the contiguous clock's frame 0.
    anchor: Instant,
    /// Frames (at 48 kHz) already stamped since `anchor`.
    frames: u64,
    reanchors: u64,
}

impl StreamAnchor {
    /// Arrival-implied drift beyond which the stream re-anchors instead of absorbing:
    /// genuine discontinuities (a sound-server restart, a stalled consumer's dropped
    /// stretch, a device switch) show up as ~seconds; scheduler/IPC jitter is ~ms.
    /// Half a second sits comfortably between them (and matches the capture backlog
    /// guard's own [`LAG_WARN_THRESHOLD_SECS`]).
    pub(super) const REANCHOR_THRESHOLD_SECS: f64 = 0.5;

    /// Anchor at the first delivery: `now` back-dated by that first chunk's own
    /// duration (`frames` at 48 kHz). The chunk itself is then stamped via
    /// [`stamp`](Self::stamp) like every later one (its drift is 0 by construction).
    pub(super) fn new(first_chunk_frames: usize, now: Instant) -> Self {
        Self { anchor: chunk_capture_wall(first_chunk_frames, now), frames: 0, reanchors: 0 }
    }

    /// Stamp a chunk of `frames` frames arriving at `now`: returns its first sample's
    /// capture instant on the contiguous clock, plus the arrival-implied drift
    /// (seconds; positive = arrivals running behind the contiguous clock) observed for
    /// this delivery — the caller's monitoring signal. Advances the frame count; a
    /// drift beyond [`REANCHOR_THRESHOLD_SECS`](Self::REANCHOR_THRESHOLD_SECS)
    /// re-anchors to the arrival-implied position instead (counted, for the caller to
    /// log).
    pub(super) fn stamp(&mut self, frames: usize, now: Instant) -> (Instant, f64) {
        let contiguous = self.anchor + Duration::from_secs_f64(self.frames as f64 / 48000.0);
        let implied = chunk_capture_wall(frames, now);
        let drift = if implied >= contiguous {
            implied.duration_since(contiguous).as_secs_f64()
        } else {
            -contiguous.duration_since(implied).as_secs_f64()
        };
        if drift.abs() > Self::REANCHOR_THRESHOLD_SECS {
            self.reanchors += 1;
            self.anchor = implied;
            self.frames = frames as u64;
            (implied, drift)
        } else {
            self.frames += frames as u64;
            (contiguous, drift)
        }
    }

    /// How many times [`stamp`](Self::stamp) has re-anchored so far.
    pub(super) fn reanchor_count(&self) -> u64 {
        self.reanchors
    }
}

/// Warn (once per run) when the capture is running further than this behind real time.
/// A correctly negotiated 25ms-fragment stream sits near zero lag; crossing this is the
/// DRAGON-126 signature (server handing back ~2s-stale chunks), which would throw the
/// relay's `w0` sync anchor off by the lag.
const LAG_WARN_THRESHOLD_SECS: f64 = 0.5;

/// Implied delivery lag (seconds): how far the delivered audio has fallen behind real
/// time — wall `elapsed` since the first chunk minus the audio duration of the frames
/// delivered so far (`frames / 48000`). Positive = behind; ~zero = keeping up. Pure so
/// the backlog guard's math is unit-tested away from the pulse callback.
fn delivery_lag_secs(elapsed: Duration, frames_delivered: u64) -> f64 {
    elapsed.as_secs_f64() - frames_delivered as f64 / 48000.0
}

/// Whether a measured lag crosses the warn threshold — the boundary the capture thread's
/// one-shot backlog warning fires on.
fn lag_exceeds_warn(lag_secs: f64) -> bool {
    lag_secs > LAG_WARN_THRESHOLD_SECS
}

/// A running monitor-audio capture. [`start`](Self::start) spawns one background
/// thread that records the requested source (or the default sink's monitor) and
/// streams [`CaptureChunk`]s through the returned channel; [`stop`](Self::stop) tears
/// it down (bounded, DRAGON-118) and returns summary [`CaptureStats`].
pub(crate) struct MonitorCapture {
    stop: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<f64>>>,
    dropped: Arc<AtomicU64>,
    /// Peak implied delivery lag in MICROSECONDS (`fetch_max`ed by the read callback),
    /// surfaced as [`CaptureStats::peak_lag_secs`] — the backlog guard (DRAGON-126).
    peak_lag_us: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MonitorCapture {
    /// Spawn the capture. `source = None` resolves the default sink's monitor exactly
    /// like [`crate::audio::MonitorLatencyProbe`] (`server_info` → `"<sink>.monitor"`);
    /// `Some(name)` records that source directly. `tee`, when given, additionally
    /// receives every delivered chunk's samples on the capture thread itself (see
    /// [`CaptureTee`]). Returns `None` only when the
    /// platform is unsupported or the thread can't be spawned; a connect/stream
    /// failure on a spawned thread simply delivers nothing (the channel then reports
    /// disconnected) and [`stop`](Self::stop) yields zeroed stats.
    #[cfg(target_os = "linux")]
    pub(crate) fn start(
        source: Option<String>,
        tee: Option<CaptureTee>,
    ) -> Option<(Self, std::sync::mpsc::Receiver<CaptureChunk>)> {
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicU64::new(0));
        let peak_lag_us = Arc::new(AtomicU64::new(0));
        let (tx, rx) = std::sync::mpsc::sync_channel::<CaptureChunk>(CAPTURE_CHANNEL_CAPACITY);
        let thread_stop = stop.clone();
        let thread_samples = samples.clone();
        let thread_dropped = dropped.clone();
        let thread_peak_lag = peak_lag_us.clone();
        let thread = std::thread::Builder::new()
            .name("cck-monitor-capture".to_string())
            .spawn(move || {
                pulse::run_capture(
                    source,
                    tee,
                    &thread_stop,
                    &thread_samples,
                    &thread_dropped,
                    &thread_peak_lag,
                    tx,
                )
            })
            .ok()?;
        Some((Self { stop, samples, dropped, peak_lag_us, thread: Some(thread) }, rx))
    }

    /// macOS: an audio-only ScreenCaptureKit stream (DRAGON-130 WP4). Spawns a worker
    /// thread that owns the (non-`Send`) `SCStream` for the whole session; SCK delivers
    /// `CMSampleBuffer`s to a delegate on its own serial queue, which the delegate's
    /// audio closure converts (planar/interleaved f32 → interleaved stereo 48 kHz) and
    /// stamps CONTIGUOUSLY through the SAME [`StreamAnchor`] the Linux path uses, feeds
    /// the [`CaptureTee`] identically, and delivers on the SAME bounded channel. SCK
    /// exposes no signed device latency, so `samples` stays empty (see
    /// [`latest_signed_latency_ms`](Self::latest_signed_latency_ms)) — the pump's latch
    /// then fails open to 0.0 (no device compensation), exactly the "source that never
    /// reports" case its budget exists for. `source` is ignored: SCK captures the
    /// display's system-audio mix, not a named monitor source. `None` only if the
    /// thread can't be spawned.
    #[cfg(target_os = "macos")]
    pub(crate) fn start(
        source: Option<String>,
        tee: Option<CaptureTee>,
    ) -> Option<(Self, std::sync::mpsc::Receiver<CaptureChunk>)> {
        let stop = Arc::new(AtomicBool::new(false));
        // Stays empty on macOS: SCK gives no signed device latency, so `median` and
        // `latest_signed_latency_ms` both fail open (0.0 / None) — no compensation.
        let samples = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicU64::new(0));
        let peak_lag_us = Arc::new(AtomicU64::new(0));
        let (tx, rx) = std::sync::mpsc::sync_channel::<CaptureChunk>(CAPTURE_CHANNEL_CAPACITY);
        let thread_stop = stop.clone();
        let thread_dropped = dropped.clone();
        let thread_peak_lag = peak_lag_us.clone();
        let thread = std::thread::Builder::new()
            .name("cck-monitor-capture".to_string())
            .spawn(move || {
                sck::run_capture(source, tee, thread_stop, thread_dropped, thread_peak_lag, tx)
            })
            .ok()?;
        Some((Self { stop, samples, dropped, peak_lag_us, thread: Some(thread) }, rx))
    }

    /// Windows (DRAGON-229 M3): an in-process WASAPI LOOPBACK capture of the default
    /// render endpoint. Spawns a worker thread that owns the (single-thread-affine)
    /// `IAudioClient`/`IAudioCaptureClient` for the whole session, converts each packet to
    /// interleaved-stereo-f32 @ 48 kHz, and stamps CONTIGUOUSLY through the SAME
    /// [`StreamAnchor`] the Linux/mac paths use — feeding the [`CaptureTee`] and delivering
    /// on the SAME bounded channel. WASAPI exposes no signed device latency to fold in, so
    /// `samples` stays empty (the pump's latch fails open to 0.0 — the "source that never
    /// reports" case, exactly like the mac SCK arm). `source` (DRAGON-282) is the chosen
    /// WASAPI render-endpoint id — the Output-device picker's choice; `None`/empty/`"default"`
    /// follow the OS default endpoint, and a stale id falls back to the default with a warn.
    /// `None` return only if the thread can't spawn.
    #[cfg(windows)]
    pub(crate) fn start(
        source: Option<String>,
        tee: Option<CaptureTee>,
    ) -> Option<(Self, std::sync::mpsc::Receiver<CaptureChunk>)> {
        let stop = Arc::new(AtomicBool::new(false));
        // Stays empty on Windows (no signed device latency) — `median`/
        // `latest_signed_latency_ms` fail open to 0.0/None, no compensation.
        let samples = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicU64::new(0));
        let peak_lag_us = Arc::new(AtomicU64::new(0));
        let (tx, rx) = std::sync::mpsc::sync_channel::<CaptureChunk>(CAPTURE_CHANNEL_CAPACITY);
        let thread_stop = stop.clone();
        let thread_dropped = dropped.clone();
        let thread_peak_lag = peak_lag_us.clone();
        let thread = std::thread::Builder::new()
            .name("cck-monitor-capture".to_string())
            .spawn(move || {
                crate::platform::windows::wasapi_loopback::run_capture(
                    source,
                    tee,
                    thread_stop,
                    thread_dropped,
                    thread_peak_lag,
                    tx,
                )
            })
            .ok()?;
        Some((Self { stop, samples, dropped, peak_lag_us, thread: Some(thread) }, rx))
    }

    /// Other platforms: no capture client. (Linux, macOS, and Windows are handled above.)
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    pub(crate) fn start(
        _source: Option<String>,
        _tee: Option<CaptureTee>,
    ) -> Option<(Self, std::sync::mpsc::Receiver<CaptureChunk>)> {
        None
    }

    /// The most recent signed device-latency sample (ms) collected so far this run
    /// (DRAGON-125) — `None` before the first sample lands (~300ms after `start`,
    /// then every ~2s), so a consumer can tell "not measured yet" apart from a
    /// genuine 0.0 (a suspended/virtual sink). Lets a LIVE consumer
    /// (`record::pump`'s per-session latency latch) read one without waiting for
    /// `stop()`'s full-run median; `&self` (unlike `stop`), so it can be polled
    /// repeatedly while the capture keeps running, from another thread (the shared
    /// `Arc<Mutex<_>>` is exactly what the background thread itself writes into).
    pub(crate) fn latest_signed_latency_ms(&self) -> Option<f64> {
        self.samples.lock().ok().and_then(|g| g.last().copied())
    }

    /// Signal the capture thread to tear down, bounded-join it (≤2s), and return
    /// summary stats. A wedged sound server must never hang the caller (DRAGON-118):
    /// if the thread won't exit in time it is DETACHED and whatever it already
    /// measured/counted is returned.
    pub(crate) fn stop(mut self) -> CaptureStats {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                log::warn!("monitor capture did not exit within 2s; detaching it (DRAGON-118)");
            }
        }
        let samples = self.samples.lock().map(|g| g.clone()).unwrap_or_default();
        CaptureStats {
            device_latency_ms: super::monitor_latency::median(samples),
            dropped_chunks: self.dropped.load(Ordering::Relaxed),
            peak_lag_secs: self.peak_lag_us.load(Ordering::Relaxed) as f64 / 1_000_000.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Linux: the raw libpulse async client. The shared FFI (extern block, opaque types,
// PaGuard, the bounded-wait helpers) lives in `crate::audio::pulse_ffi`, shared with
// the latency probe. This module owns only what differs for a data-consuming record
// stream: the sample spec, the read callback, and the send-or-drop delivery.
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod pulse {
    use super::{
        delivery_lag_secs, f32le_to_samples, hole_sample_count, lag_exceeds_warn, AtomicBool,
        AtomicU64, CaptureChunk, CaptureTee, Duration, Instant, Mutex, Ordering, StreamAnchor,
    };
    use super::super::pulse_ffi::*;
    use std::cell::{Cell, RefCell};
    use std::ffi::{c_int, c_void, CString};
    use std::ptr;
    use std::sync::mpsc::SyncSender;

    /// Read-callback userdata: owns the delivery sender, borrows the shared drop
    /// counter. Declared on the thread's stack BEFORE `guard` in [`run_capture`] (the
    /// same discipline as the latency probe's `sink_slot`) so it only drops AFTER the
    /// mainloop has been stopped — no callback can still be holding this pointer by
    /// then.
    struct CbState<'a> {
        tx: SyncSender<CaptureChunk>,
        /// Secondary capture-thread consumer (the AEC far-end feeder, DRAGON-128)
        /// — see [`CaptureTee`]. `RefCell`: touched ONLY by the read callback,
        /// like `anchor` below.
        tee: RefCell<Option<CaptureTee>>,
        dropped: &'a AtomicU64,
        /// The stream's contiguous stamping clock (the module doc's timing model) —
        /// established at the first delivery, re-anchored only on a logged
        /// discontinuity. `RefCell`: touched ONLY by the read callback (a single
        /// pulse IO thread), like the `Cell`s below.
        anchor: RefCell<Option<StreamAnchor>>,
        // Backlog guard (DRAGON-126): implied delivery lag = wall elapsed since the
        // FIRST delivered chunk minus the audio duration of the frames delivered so far.
        // These `Cell`s are touched ONLY by the read callback (a single pulse IO
        // thread), so plain interior mutability is sound; the peak is `fetch_max`ed into
        // the shared atomic below so `stop()` can read it after (or without) a join.
        first_chunk: Cell<Option<Instant>>,
        frames_delivered: Cell<u64>,
        warned: Cell<bool>,
        peak_lag_us: &'a AtomicU64,
    }

    /// Hand one converted (or hole-silenced) buffer to the consumer. The callback
    /// this runs inside must never block, so a full channel just counts the drop —
    /// logged once, then every 100th time (mirrors the PipeWire capture callback's
    /// backlog logging in `record::pipewire::record_pipewire`).
    fn deliver(state: &CbState<'_>, samples: Vec<f32>) {
        let frames = samples.len() / 2; // interleaved stereo
        let now = Instant::now();
        // Contiguous stamping (the module doc's timing model): one anchor per
        // stream, each chunk placed by the delivered sample count; a drift past the
        // re-anchor threshold is a real discontinuity — re-anchored and logged, so
        // it can never silently shift (or chop) the placed stream again.
        let capture_wall = {
            let mut slot = state.anchor.borrow_mut();
            let anchor = slot.get_or_insert_with(|| StreamAnchor::new(frames, now));
            let before = anchor.reanchor_count();
            let (stamp, drift) = anchor.stamp(frames, now);
            if anchor.reanchor_count() > before {
                log::warn!(
                    "system-audio capture discontinuity: arrivals drifted {:.0} ms from the \
                     stream's contiguous clock — re-anchoring (#{})",
                    drift * 1000.0,
                    anchor.reanchor_count()
                );
            }
            stamp
        };
        // Backlog guard (DRAGON-126): measure how far behind real time the delivered
        // sample count has fallen. Anchored at the FIRST chunk; every chunk (real data
        // OR hole silence) counts, so the frame total stays a faithful audio clock. The
        // peak is recorded always; the warning fires at most once per run.
        let first = state.first_chunk.get().unwrap_or_else(|| {
            state.first_chunk.set(Some(now));
            now
        });
        let frames_total = state.frames_delivered.get() + frames as u64;
        state.frames_delivered.set(frames_total);
        let lag = delivery_lag_secs(now.saturating_duration_since(first), frames_total);
        if lag > 0.0 {
            state.peak_lag_us.fetch_max((lag * 1_000_000.0) as u64, Ordering::Relaxed);
        }
        if lag_exceeds_warn(lag) && !state.warned.get() {
            state.warned.set(true);
            log::warn!(
                "system-audio capture is running {} ms behind real time — sync anchor \
                 may be off; buffer negotiation problem?",
                (lag * 1000.0).round()
            );
        }
        // The tee sees every chunk's samples (data and hole silence alike — a hole
        // is real elapsed playback time the far-end reference must keep counting)
        // BEFORE delivery consumes them. Bounded work only (see `CaptureTee`).
        if let Some(tee) = state.tee.borrow_mut().as_mut() {
            tee(&samples);
        }
        if state.tx.try_send(CaptureChunk { samples, capture_wall }).is_err() {
            let n = state.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if n == 1 || n.is_multiple_of(100) {
                log::warn!(
                    "monitor capture: consumer backlog — {n} chunks dropped so far (system \
                     audio will have gaps if this keeps up)"
                );
            }
        }
    }

    /// Read callback: drain every readable chunk with `pa_stream_peek`/`pa_stream_drop`.
    /// THREE cases: (a) real data → f32le-decode and deliver; (b) a HOLE (`data` NULL,
    /// `nbytes` > 0 — pulse's way of reporting a gap, e.g. an overrun) → still drop it,
    /// but ALSO deliver `nbytes/4` zeroed samples, so downstream sample continuity
    /// holds (silently skipping a hole would shift every later sample's
    /// `capture_wall` earlier by the hole's duration); (c) `rc < 0 || nbytes == 0` →
    /// nothing left to peek, stop (no drop — nothing was peeked).
    unsafe extern "C" fn capture_read_cb(s: *mut PaStream, _nbytes: usize, userdata: *mut c_void) {
        if userdata.is_null() {
            return;
        }
        // SAFETY: `userdata` is `&CbState` passed to `pa_stream_set_read_callback` in
        // `run_capture`; it outlives the mainloop (dropped only after the mainloop is
        // stopped, so no callback can still reference it).
        let state = unsafe { &*(userdata as *const CbState<'_>) };
        loop {
            let mut data: *const c_void = ptr::null();
            let mut nbytes: usize = 0;
            // SAFETY: `s` is the live stream pulse handed us, called under the lock.
            let rc = unsafe { pa_stream_peek(s, &mut data, &mut nbytes) };
            if rc < 0 || nbytes == 0 {
                break;
            }
            if data.is_null() {
                deliver(state, vec![0.0f32; hole_sample_count(nbytes)]);
            } else {
                // SAFETY: `data` is valid for `nbytes` bytes until the matching drop.
                let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, nbytes) };
                deliver(state, f32le_to_samples(bytes));
            }
            // SAFETY: a successful peek is matched by exactly one drop, hole or not.
            unsafe { pa_stream_drop(s) };
        }
    }

    /// The record-stream buffer attributes we hand `pa_stream_connect_record`
    /// (DRAGON-126). Everything but `fragsize` is `u32::MAX` = "let the server pick its
    /// default" — those fields only govern PLAYBACK buffering, irrelevant to a record
    /// stream. `fragsize` is what matters: it asks the server to hand back a fragment
    /// this small (25 ms of the negotiated stereo f32 48 kHz spec), so chunks arrive ~one
    /// fragment stale instead of at the server's default ≈2 s record latency — WITHOUT
    /// it (a NULL attr, as before), `capture_wall`/`w0` land ~1.9 s late and system audio
    /// is finalized that far behind the video. This is exactly why ffmpeg's own `-f
    /// pulse` input always requests a small fragment. Paired with
    /// [`PA_STREAM_ADJUST_LATENCY`] so the server sizes source-side buffering to match.
    ///
    /// 25 ms of stereo f32 48 kHz = 48000 × 0.025 × 2 ch × 4 B = 9600 bytes.
    pub(super) fn record_buffer_attr() -> PaBufferAttr {
        PaBufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: (48000.0 * 0.025 * 2.0 * 4.0) as u32,
        }
    }

    /// The capture thread body: connect a threaded-mainloop pulse client (the same
    /// connect flow / bounded waits as `monitor_latency`'s `run_probe`), resolve the
    /// requested source (or the default sink's monitor), open an f32
    /// stereo 48kHz record stream, and forward every delivered chunk through `tx`
    /// until `stop`. Every wait is deadline-bounded; a connection/stream failure exits
    /// early (nothing was ever set up to collect from).
    pub(super) fn run_capture(
        source: Option<String>,
        tee: Option<CaptureTee>,
        stop: &AtomicBool,
        samples: &Mutex<Vec<f64>>,
        dropped: &AtomicU64,
        peak_lag_us: &AtomicU64,
        tx: SyncSender<CaptureChunk>,
    ) {
        // Generous on connect (runs once at capture START, off the stop path); short
        // per-sample — identical to the latency probe's own constants.
        const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
        const OP_TIMEOUT: Duration = Duration::from_secs(1);
        const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

        // Declared before `guard` so both drop AFTER it (after the mainloop is
        // stopped and no callback can still touch them) — mirrors `run_probe`'s
        // `sink_slot`. `cb_state` owns `tx` and borrows `dropped` directly (no
        // separate counter to reconcile later): the callback increments the SAME
        // atomic `stop()` eventually reads.
        let sink_slot: Mutex<Option<CString>> = Mutex::new(None);
        let cb_state = CbState {
            tx,
            tee: RefCell::new(tee),
            dropped,
            anchor: RefCell::new(None),
            first_chunk: Cell::new(None),
            frames_delivered: Cell::new(0),
            warned: Cell::new(false),
            peak_lag_us,
        };

        // SAFETY: every pulse call below runs on this one thread; all access to the
        // context/stream after `start` is under the mainloop lock, and `guard` tears
        // the client down on any return path.
        unsafe {
            let m = pa_threaded_mainloop_new();
            if m.is_null() {
                return;
            }
            let mut guard = PaGuard { m, c: ptr::null_mut(), s: ptr::null_mut(), started: false };

            let api = pa_threaded_mainloop_get_api(m);
            if api.is_null() {
                return;
            }
            let ctx_name = CString::new("cosmic-capture-kit-capture").unwrap();
            let c = pa_context_new(api, ctx_name.as_ptr());
            if c.is_null() {
                return;
            }
            guard.c = c;

            // Connect before the loop runs (nothing else touches the context yet),
            // then start the IO thread and poll to READY under the lock.
            if pa_context_connect(c, ptr::null(), 0, ptr::null()) < 0 {
                return;
            }
            if pa_threaded_mainloop_start(m) != 0 {
                return;
            }
            guard.started = true;
            if !wait_context_ready(m, c, stop, CONNECT_TIMEOUT) {
                return;
            }

            // Resolve the device: the caller's explicit source, or the default
            // sink's monitor (exactly like the latency probe).
            let device = match source {
                Some(name) => CString::new(name).ok(),
                None => {
                    pa_threaded_mainloop_lock(m);
                    let op = pa_context_get_server_info(
                        c,
                        server_info_cb,
                        &sink_slot as *const Mutex<Option<CString>> as *mut c_void,
                    );
                    pa_threaded_mainloop_unlock(m);
                    if !await_op(m, op, Instant::now() + OP_TIMEOUT, stop) {
                        return;
                    }
                    let Some(sink) = sink_slot.lock().ok().and_then(|g| g.clone()) else {
                        return;
                    };
                    make_monitor_name(&sink)
                }
            };
            let Some(device) = device else {
                return;
            };

            // f32 stereo 48kHz: pulse converts server-side, so the negotiated stream
            // IS this spec — no client-side resampling/format conversion needed.
            let ss = PaSampleSpec { format: PA_SAMPLE_FLOAT32LE, rate: 48000, channels: 2 };
            let stream_name = CString::new("cosmic-capture-kit-capture").unwrap();
            // Built OUTSIDE the mainloop lock (trivially cheap; the connect call below
            // runs under the lock). The buffer attr + ADJUST_LATENCY flag are THE
            // DRAGON-126 fix: without them the server applies its default ~2s record
            // latency and chunks arrive ~1.9s stale, which back-dates every
            // `capture_wall` — and so the relay's `w0` anchor — that far off.
            let attr = record_buffer_attr();
            pa_threaded_mainloop_lock(m);
            let s = pa_stream_new(c, stream_name.as_ptr(), &ss, ptr::null());
            if s.is_null() {
                pa_threaded_mainloop_unlock(m);
                return;
            }
            guard.s = s;
            pa_stream_set_read_callback(
                s,
                Some(capture_read_cb),
                &cb_state as *const CbState<'_> as *mut c_void,
            );
            // ADJUST_LATENCY makes the server size the source-side buffering to `attr`'s
            // small fragsize (the actual fix); the two timing flags are unchanged. This
            // makes the ONE-chunk back-date in `chunk_capture_wall` correct again — it
            // does NOT touch device-latency compensation, which the DRAGON-119
            // audible-time model still folds in ONCE at finalize (`device_latency_ms`);
            // fixing the fragment must not be mistaken for, or double-count, that.
            let flags =
                PA_STREAM_INTERPOLATE_TIMING | PA_STREAM_AUTO_TIMING_UPDATE | PA_STREAM_ADJUST_LATENCY;
            let rc = pa_stream_connect_record(s, device.as_ptr(), &attr, flags);
            pa_threaded_mainloop_unlock(m);
            if rc < 0 {
                return;
            }
            if !wait_stream_ready(m, s, stop, CONNECT_TIMEOUT) {
                return;
            }

            // Same settle as the latency probe: the first timing update should
            // reflect real device data (and the smoke test still gets a latency
            // sample quickly).
            sleep_or_stop(Duration::from_millis(300), stop);
            while !stop.load(Ordering::Relaxed) {
                // Unlike the latency probe, no `pa_stream_flush` here: that probe
                // discards its data yet still keeps a stream open, so it must flush
                // its own accumulating client-side backlog before every timing read
                // or the backlog hides the sign. Our read callback (above) actually
                // CONSUMES every chunk continuously, so no backlog ever accumulates
                // client-side — the flush's precondition already holds for free.
                pa_threaded_mainloop_lock(m);
                let op = pa_stream_update_timing_info(s, None, ptr::null_mut());
                pa_threaded_mainloop_unlock(m);
                if await_op(m, op, Instant::now() + OP_TIMEOUT, stop) {
                    let mut usec: u64 = 0;
                    let mut negative: c_int = 0;
                    pa_threaded_mainloop_lock(m);
                    let got = pa_stream_get_latency(s, &mut usec, &mut negative);
                    pa_threaded_mainloop_unlock(m);
                    if got >= 0 {
                        let sample = if negative != 0 { usec as f64 / 1000.0 } else { 0.0 };
                        if let Ok(mut g) = samples.lock() {
                            g.push(sample);
                        }
                    }
                }
                // Bail if the stream or context dropped out.
                pa_threaded_mainloop_lock(m);
                let sst = pa_stream_get_state(s);
                let cst = pa_context_get_state(c);
                pa_threaded_mainloop_unlock(m);
                if sst == PA_STREAM_FAILED
                    || sst == PA_STREAM_TERMINATED
                    || cst == PA_CONTEXT_FAILED
                    || cst == PA_CONTEXT_TERMINATED
                {
                    break;
                }
                sleep_or_stop(SAMPLE_INTERVAL, stop);
            }
            // `guard` drops here → bounded teardown; `cb_state`/`sink_slot` drop
            // after it, in reverse declaration order.
        }
    }
}

// ---------------------------------------------------------------------------
// macOS: an audio-only ScreenCaptureKit stream. The SCK delegate/session mechanics
// and the CMSampleBuffer→interleaved-f32 conversion live in the reusable
// `crate::platform::mac::sck_stream` (shared with the WP5 video worker). This module
// owns only what differs from the Linux pulse path: the audio SCStreamConfiguration,
// and the contiguous-stamping delivery — the SAME `StreamAnchor` timing model, tee
// hand-off, backlog guard, and send-or-drop as `pulse::deliver`, byte-for-byte in
// behavior (the Linux impl is untouched; this is an additive per-platform branch).
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod sck {
    use super::{
        delivery_lag_secs, lag_exceeds_warn, AtomicBool, AtomicU64, CaptureChunk, CaptureTee,
        Duration, Instant, Ordering, StreamAnchor,
    };
    use crate::platform::mac::sck_stream::{self, SampleHandlers, SampleSink, SckSession};
    use objc2::rc::Retained;
    use objc2_core_media::CMTime;
    use objc2_screen_capture_kit::{SCStreamConfiguration, SCStreamOutputType};
    use std::sync::mpsc::SyncSender;
    use std::sync::Arc;

    /// The delegate-side delivery state (mutated only on SCK's serial audio queue), a
    /// direct analogue of the Linux `pulse::CbState`: one `StreamAnchor` per stream,
    /// the backlog-lag counters, the tee, and the send-or-drop sender. Owned by the
    /// audio closure (so it is `'static` + `Send`), NOT borrowed — the closure outlives
    /// the `run_capture` stack frame through the session's delegate.
    struct AudioAccum {
        tx: SyncSender<CaptureChunk>,
        tee: Option<CaptureTee>,
        dropped: Arc<AtomicU64>,
        peak_lag_us: Arc<AtomicU64>,
        anchor: Option<StreamAnchor>,
        first_chunk: Option<Instant>,
        frames_delivered: u64,
        warned: bool,
        /// On-button meter accumulator (sum of squares / sample count of the current
        /// window): macOS has no pulse monitor to run a metering ffmpeg sidecar
        /// against, so the overlay's system level is published from THIS owned
        /// capture instead — one RMS reading per [`METER_WINDOW_SAMPLES`] window,
        /// mirroring the Linux sidecar's `asetnsamples=4800` (0.1s) cadence.
        meter_sq_sum: f64,
        meter_samples: usize,
    }

    /// Samples per published meter window: 0.1s of interleaved stereo at 48 kHz
    /// (4800 frames × 2 channels) — the Linux meter sidecar's cadence.
    const METER_WINDOW_SAMPLES: usize = 4800 * 2;

    impl AudioAccum {
        /// Stamp + deliver one converted interleaved-stereo chunk — the exact shape of
        /// `pulse::deliver` (see its doc): contiguous `StreamAnchor` placement with a
        /// loud re-anchor on a real discontinuity, the DRAGON-126 backlog guard, the
        /// tee before delivery, and a non-blocking send that counts drops.
        fn deliver(&mut self, samples: Vec<f32>) {
            if samples.is_empty() {
                return; // SCK's occasional empty/priming buffer — nothing to place
            }
            let frames = samples.len() / 2; // interleaved stereo
            let now = Instant::now();
            let capture_wall = {
                let anchor = self.anchor.get_or_insert_with(|| StreamAnchor::new(frames, now));
                let before = anchor.reanchor_count();
                let (stamp, drift) = anchor.stamp(frames, now);
                if anchor.reanchor_count() > before {
                    log::warn!(
                        "system-audio (SCK) capture discontinuity: arrivals drifted {:.0} ms \
                         from the stream's contiguous clock — re-anchoring (#{})",
                        drift * 1000.0,
                        anchor.reanchor_count()
                    );
                }
                stamp
            };
            let first = *self.first_chunk.get_or_insert(now);
            self.frames_delivered += frames as u64;
            let lag = delivery_lag_secs(now.saturating_duration_since(first), self.frames_delivered);
            if lag > 0.0 {
                self.peak_lag_us.fetch_max((lag * 1_000_000.0) as u64, Ordering::Relaxed);
            }
            if lag_exceeds_warn(lag) && !self.warned {
                self.warned = true;
                log::warn!(
                    "system-audio (SCK) capture is running {} ms behind real time — sync \
                     anchor may be off",
                    (lag * 1000.0).round()
                );
            }
            // Feed the overlay's system-audio button meter (struct doc): accumulate
            // this chunk into the running 0.1s window and publish its RMS when full.
            self.meter_sq_sum += samples.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>();
            self.meter_samples += samples.len();
            if self.meter_samples >= METER_WINDOW_SAMPLES {
                let rms = (self.meter_sq_sum / self.meter_samples as f64).sqrt() as f32;
                crate::audio::meters::publish_sys_level(crate::audio::meters::level_from_rms(rms));
                self.meter_sq_sum = 0.0;
                self.meter_samples = 0;
            }
            if let Some(tee) = self.tee.as_mut() {
                tee(&samples);
            }
            if self.tx.try_send(CaptureChunk { samples, capture_wall }).is_err() {
                let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if n == 1 || n.is_multiple_of(100) {
                    log::warn!(
                        "monitor capture (SCK): consumer backlog — {n} chunks dropped so far \
                         (system audio will have gaps if this keeps up)"
                    );
                }
            }
        }
    }

    /// The audio-only stream configuration: 48 kHz stereo f32 system audio, our own
    /// process excluded. SCK requires a valid VIDEO config even for audio-only capture,
    /// so we set a tiny frame at ~1 fps and attach NO screen output — SCK then discards
    /// video frames (no consumer) while still delivering the audio track.
    fn build_audio_config() -> Retained<SCStreamConfiguration> {
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setCapturesAudio(true);
            config.setSampleRate(48000);
            config.setChannelCount(2);
            config.setExcludesCurrentProcessAudio(true);
            config.setWidth(128);
            config.setHeight(128);
            config.setMinimumFrameInterval(CMTime::new(1, 1)); // ~1 fps (video ignored)
            config.setShowsCursor(false);
        }
        config
    }

    /// The macOS capture-thread body: build + start the audio SCStream, park until
    /// `stop`, then stop it (bounded, inside the session). Any setup failure logs and
    /// returns early (the channel then reports disconnected — the consumer's smoke
    /// check fails cleanly), mirroring the Linux path's "delivers nothing on failure".
    /// `source` is unused (SCK has no named monitor sources).
    pub(super) fn run_capture(
        _source: Option<String>,
        tee: Option<CaptureTee>,
        stop: Arc<AtomicBool>,
        dropped: Arc<AtomicU64>,
        peak_lag_us: Arc<AtomicU64>,
        tx: SyncSender<CaptureChunk>,
    ) {
        if !crate::platform::mac::ensure_permission() {
            log::error!(
                "system-audio capture: Screen Recording permission not granted — no audio"
            );
            return;
        }
        let Some(filter) = crate::platform::mac::primary_display_filter() else {
            log::error!("system-audio capture: no display to attach the audio SCStream to");
            return;
        };
        let config = build_audio_config();
        let mut accum = AudioAccum {
            tx,
            tee,
            dropped,
            peak_lag_us,
            anchor: None,
            first_chunk: None,
            frames_delivered: 0,
            warned: false,
            meter_sq_sum: 0.0,
            meter_samples: 0,
        };
        // Fresh session → fresh meter (a stale level from a prior capture in this
        // process must not linger on the overlay's button).
        crate::audio::meters::publish_sys_level(0.0);
        let on_audio: SampleSink = Box::new(move |sbuf| {
            // SAFETY: `sbuf` is a live audio CMSampleBuffer SCK delivered to the Audio
            // output; extraction copies out before returning.
            if let Some(samples) = unsafe { sck_stream::extract_interleaved_stereo_f32(sbuf) } {
                accum.deliver(samples);
            }
        });
        let session = match SckSession::build(
            &filter,
            &config,
            SampleHandlers::audio_only(on_audio),
            &[SCStreamOutputType::Audio],
        ) {
            Ok(s) => s,
            Err(e) => {
                log::error!("system-audio capture: {e}");
                return;
            }
        };
        if let Err(e) = session.start() {
            log::error!("system-audio capture: SCStream start failed: {e}");
            return;
        }
        log::info!("system-audio capture: SCK audio stream started (48 kHz stereo)");
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(20));
        }
        session.stop();
        // Capture over → the overlay's system meter falls flat instead of freezing
        // on the last published window.
        crate::audio::meters::publish_sys_level(0.0);
    }
}

// ---------------------------------------------------------------------------
// Windows (DRAGON-229 M3): the delivery half of the WASAPI-loopback capture. The
// OS-facing IAudioClient/IAudioCaptureClient body lives under
// `platform/windows/wasapi_loopback.rs` (strict split, LOG law 7); this submodule is
// the pure stamping/tee/meter/send-or-drop sink it drives — a windows-only mirror of
// the mac `sck::AudioAccum::deliver` (that arm is untouched, so the mac build stays
// byte-identical). `#[cfg(windows)]` ⇒ compiles to NOTHING on Linux/macOS.
// ---------------------------------------------------------------------------
#[cfg(windows)]
pub(crate) mod win_delivery {
    use super::{delivery_lag_secs, lag_exceeds_warn, CaptureChunk, CaptureTee, StreamAnchor};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::SyncSender;
    use std::sync::Arc;
    use std::time::Instant;

    /// Samples per published meter window: 0.1s of interleaved stereo at 48 kHz — the
    /// Linux/mac meter cadence, so the overlay's system-audio button moves identically.
    const METER_WINDOW_SAMPLES: usize = 4800 * 2;

    /// The system-track delivery sink the WASAPI worker feeds one interleaved-stereo-f32
    /// packet at a time. Field-for-field the mac `AudioAccum` (contiguous `StreamAnchor`
    /// placement, the DRAGON-126 backlog guard, the 0.1s RMS meter, tee-before-delivery,
    /// send-or-drop) — reused here rather than re-touching the cfg(macos) copy.
    pub(crate) struct WasapiSink {
        tx: SyncSender<CaptureChunk>,
        tee: Option<CaptureTee>,
        dropped: Arc<AtomicU64>,
        peak_lag_us: Arc<AtomicU64>,
        anchor: Option<StreamAnchor>,
        first_chunk: Option<Instant>,
        frames_delivered: u64,
        warned: bool,
        meter_sq_sum: f64,
        meter_samples: usize,
    }

    impl WasapiSink {
        pub(crate) fn new(
            tx: SyncSender<CaptureChunk>,
            tee: Option<CaptureTee>,
            dropped: Arc<AtomicU64>,
            peak_lag_us: Arc<AtomicU64>,
        ) -> Self {
            Self {
                tx,
                tee,
                dropped,
                peak_lag_us,
                anchor: None,
                first_chunk: None,
                frames_delivered: 0,
                warned: false,
                meter_sq_sum: 0.0,
                meter_samples: 0,
            }
        }

        /// Stamp + deliver one converted interleaved-stereo chunk — the exact shape of
        /// `pulse::deliver` / `sck::AudioAccum::deliver`: contiguous placement with a
        /// loud re-anchor on a real discontinuity, the backlog guard, the overlay meter,
        /// the tee before delivery, and a non-blocking send that counts drops.
        pub(crate) fn deliver(&mut self, samples: Vec<f32>) {
            if samples.is_empty() {
                return;
            }
            let frames = samples.len() / 2; // interleaved stereo
            let now = Instant::now();
            let capture_wall = {
                let anchor = self.anchor.get_or_insert_with(|| StreamAnchor::new(frames, now));
                let before = anchor.reanchor_count();
                let (stamp, drift) = anchor.stamp(frames, now);
                if anchor.reanchor_count() > before {
                    log::warn!(
                        "system-audio (WASAPI) capture discontinuity: arrivals drifted {:.0} ms \
                         from the stream's contiguous clock — re-anchoring (#{})",
                        drift * 1000.0,
                        anchor.reanchor_count()
                    );
                }
                stamp
            };
            let first = *self.first_chunk.get_or_insert(now);
            self.frames_delivered += frames as u64;
            let lag =
                delivery_lag_secs(now.saturating_duration_since(first), self.frames_delivered);
            if lag > 0.0 {
                self.peak_lag_us.fetch_max((lag * 1_000_000.0) as u64, Ordering::Relaxed);
            }
            if lag_exceeds_warn(lag) && !self.warned {
                self.warned = true;
                log::warn!(
                    "system-audio (WASAPI) capture is running {} ms behind real time — sync \
                     anchor may be off",
                    (lag * 1000.0).round()
                );
            }
            self.meter_sq_sum += samples.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>();
            self.meter_samples += samples.len();
            if self.meter_samples >= METER_WINDOW_SAMPLES {
                let rms = (self.meter_sq_sum / self.meter_samples as f64).sqrt() as f32;
                crate::audio::meters::publish_sys_level(crate::audio::meters::level_from_rms(rms));
                self.meter_sq_sum = 0.0;
                self.meter_samples = 0;
            }
            if let Some(tee) = self.tee.as_mut() {
                tee(&samples);
            }
            if self.tx.try_send(CaptureChunk { samples, capture_wall }).is_err() {
                let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if n == 1 || n.is_multiple_of(100) {
                    log::warn!(
                        "monitor capture (WASAPI): consumer backlog — {n} chunks dropped so far \
                         (system audio will have gaps if this keeps up)"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    // `f32le_to_samples` / `hole_sample_count` are the Linux `pulse` capture helpers
    // (cfg'd to linux), so their tests are too.
    #[cfg(target_os = "linux")]
    #[rstest]
    #[case(&[0x00, 0x00, 0x00, 0x00], 0.0)]
    #[case(&[0x00, 0x00, 0x80, 0x3f], 1.0)] // 1.0f32 LE
    #[case(&[0x00, 0x00, 0x80, 0xbf], -1.0)] // -1.0f32 LE
    #[case(&[0xcd, 0xcc, 0x8c, 0x3f], 1.1)] // 1.1f32 LE (inexact bit pattern)
    fn f32le_to_samples_converts_exact_bytes(#[case] bytes: &[u8], #[case] expected: f32) {
        assert_eq!(f32le_to_samples(bytes), vec![expected]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn f32le_to_samples_handles_multiple_interleaved_samples() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1.5f32.to_le_bytes());
        bytes.extend_from_slice(&(-2.25f32).to_le_bytes());
        assert_eq!(f32le_to_samples(&bytes), vec![1.5, -2.25]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn f32le_to_samples_ignores_a_trailing_partial_sample() {
        // as_chunks::<4>() drops a trailing remainder shorter than 4 bytes; this
        // should never happen for a real f32 stream (nbytes is always a multiple of
        // 4), but the conversion must not panic if it somehow did.
        let mut bytes = 2.0f32.to_le_bytes().to_vec();
        bytes.push(0xff);
        assert_eq!(f32le_to_samples(&bytes), vec![2.0]);
    }

    #[cfg(target_os = "linux")]
    #[rstest]
    #[case(0, 0)]
    #[case(4, 1)]
    #[case(8, 2)]
    #[case(4096, 1024)]
    fn hole_sample_count_is_nbytes_over_four(#[case] nbytes: usize, #[case] samples: usize) {
        assert_eq!(hole_sample_count(nbytes), samples);
    }

    #[rstest]
    #[case(0, Duration::ZERO)]
    #[case(4800, Duration::from_millis(100))]
    #[case(24000, Duration::from_millis(500))]
    #[case(48000, Duration::from_secs(1))]
    fn chunk_capture_wall_backdates_by_frame_duration(
        #[case] frames: usize,
        #[case] back: Duration,
    ) {
        let now = Instant::now();
        assert_eq!(chunk_capture_wall(frames, now), now - back);
    }

    // ---- StreamAnchor: the contiguous timing model ----

    #[test]
    fn stream_anchor_stamps_jittery_arrivals_contiguously() {
        let t0 = Instant::now();
        // First chunk: 1200 frames (25ms) arriving at t0 → anchor = t0 − 25ms.
        let mut a = StreamAnchor::new(1200, t0);
        let (s0, d0) = a.stamp(1200, t0);
        assert_eq!(s0, t0 - Duration::from_millis(25));
        assert_eq!(d0, 0.0);
        // Second chunk arrives 3ms LATE (28ms after the first): its stamp must be
        // exactly contiguous (anchor + 25ms), not arrival − 25ms — the 3ms of
        // scheduler jitter is reported as drift, never placed.
        let (s1, d1) = a.stamp(1200, t0 + Duration::from_millis(28));
        assert_eq!(s1, t0, "stamp = anchor + 1200/48000 = t0 exactly");
        assert!((d1 - 0.003).abs() < 1e-9, "3ms of drift observed, not placed (got {d1})");
        // Third chunk arrives 2ms EARLY: still contiguous.
        let (s2, d2) = a.stamp(1200, t0 + Duration::from_millis(48));
        assert_eq!(s2, t0 + Duration::from_millis(25));
        assert!((d2 - -0.002).abs() < 1e-9);
        assert_eq!(a.reanchor_count(), 0, "jitter must never re-anchor");
    }

    #[test]
    fn stream_anchor_reanchors_on_a_real_discontinuity_and_counts_it() {
        let t0 = Instant::now();
        let mut a = StreamAnchor::new(1200, t0);
        let _ = a.stamp(1200, t0);
        // The next delivery arrives a full second later than contiguity implies (a
        // sound-server hiccup): past the 0.5s threshold → re-anchor to the
        // arrival-implied position.
        let late = t0 + Duration::from_millis(25 + 1000 + 25);
        let (s, d) = a.stamp(1200, late);
        assert_eq!(s, late - Duration::from_millis(25), "re-anchored to arrival − duration");
        assert!((d - 1.025).abs() < 1e-6, "the discontinuity's size is reported (got {d})");
        assert_eq!(a.reanchor_count(), 1);
        // After the re-anchor, stamping is contiguous from the NEW anchor.
        let (s2, _) = a.stamp(1200, late + Duration::from_millis(25));
        assert_eq!(s2, late, "next chunk continues from the new anchor");
    }

    #[test]
    fn stream_anchor_accumulates_frames_across_hole_and_data_chunks_alike() {
        let t0 = Instant::now();
        // Mixed chunk sizes (as pulse delivers them): stamps advance by exactly the
        // stamped frame totals regardless of arrival times' noise.
        let mut a = StreamAnchor::new(480, t0);
        let (s0, _) = a.stamp(480, t0);
        let (s1, _) = a.stamp(960, t0 + Duration::from_millis(31));
        let (s2, _) = a.stamp(240, t0 + Duration::from_millis(44));
        assert_eq!(s0, t0 - Duration::from_millis(10));
        assert_eq!(s1, s0 + Duration::from_millis(10)); // +480 frames
        assert_eq!(s2, s1 + Duration::from_millis(20)); // +960 frames
    }

    #[rstest]
    // Keeping up exactly: 1s wall, 48000 frames delivered → 0 lag.
    #[case(Duration::from_secs(1), 48_000, 0.0)]
    // Behind by 1.9s: 2s wall but only 0.1s (4800 frames) delivered.
    #[case(Duration::from_secs(2), 4_800, 1.9)]
    // Ahead (freshly back-dated chunk can momentarily over-count) → negative lag.
    #[case(Duration::from_millis(500), 48_000, -0.5)]
    #[case(Duration::ZERO, 0, 0.0)]
    fn delivery_lag_secs_is_wall_minus_audio_time(
        #[case] elapsed: Duration,
        #[case] frames: u64,
        #[case] want: f64,
    ) {
        assert!((delivery_lag_secs(elapsed, frames) - want).abs() < 1e-9);
    }

    #[rstest]
    // The threshold is 0.5s, strict >: exactly 0.5 does NOT warn, just past it does.
    #[case(0.0, false)]
    #[case(0.5, false)]
    #[case(0.5001, true)]
    #[case(1.9, true)]
    #[case(-1.0, false)]
    fn lag_exceeds_warn_at_half_second_boundary(#[case] lag: f64, #[case] want: bool) {
        assert_eq!(lag_exceeds_warn(lag), want);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn record_buffer_attr_requests_a_25ms_fragment_only() {
        let attr = pulse::record_buffer_attr();
        // 25ms of stereo f32 48kHz = 48000 * 0.025 * 2ch * 4B.
        assert_eq!(attr.fragsize, 9600);
        // Everything else stays server-default (playback fields, irrelevant to record).
        assert_eq!(attr.maxlength, u32::MAX);
        assert_eq!(attr.tlength, u32::MAX);
        assert_eq!(attr.prebuf, u32::MAX);
        assert_eq!(attr.minreq, u32::MAX);
    }

    #[test]
    fn capture_stats_device_latency_reuses_the_shared_median() {
        // CaptureStats::device_latency_ms is exactly `monitor_latency::median`
        // (DRAGON-123 reuses it, never a second copy) — empty fails open to 0.0,
        // matching `monitor_latency`'s own `median_of_empty_is_zero`.
        assert_eq!(super::super::monitor_latency::median(vec![]), 0.0);
        assert_eq!(super::super::monitor_latency::median(vec![10.0, 20.0]), 15.0);
    }

    /// Smoke test (loud-skip, mirroring `monitor_latency_probe_smoke`): on a box with
    /// a reachable pulse server, `MonitorCapture::start(None)` must yield ≥1 chunk
    /// within 3s (a suspended monitor auto-resumes and delivers silence chunks) and
    /// `stop()` must return within its own 2s bound. LOUDLY skips (never a silent
    /// green) when no server looks reachable.
    #[cfg(target_os = "linux")]
    #[test]
    fn monitor_capture_smoke() {
        let reachable = std::env::var_os("PULSE_SERVER").is_some()
            || std::env::var_os("XDG_RUNTIME_DIR")
                .map(|d| std::path::Path::new(&d).join("pulse/native").exists())
                .unwrap_or(false);
        if !reachable {
            eprintln!(
                "SKIPPED (loud): monitor_capture_smoke needs a reachable PulseAudio/\
                 PipeWire-Pulse server — the capture client was not exercised"
            );
            return;
        }
        let started = Instant::now();
        let Some((capture, rx)) = MonitorCapture::start(None, None) else {
            eprintln!("SKIPPED (loud): MonitorCapture unsupported on this platform");
            return;
        };
        let mut chunks = 0u32;
        let mut total_samples = 0usize;
        let deadline = Instant::now() + Duration::from_secs(3);
        while chunks == 0 && Instant::now() < deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                chunks += 1;
                total_samples += chunk.samples.len();
            }
        }
        // Drain whatever else is already queued (non-blocking) for a fuller count.
        while let Ok(chunk) = rx.try_recv() {
            chunks += 1;
            total_samples += chunk.samples.len();
        }
        let stop_started = Instant::now();
        let stats = capture.stop();
        let stop_elapsed = stop_started.elapsed();
        eprintln!(
            "monitor_capture_smoke: {chunks} chunk(s) / {total_samples} samples received, \
             device_latency={:.1}ms, dropped={}, stop() took {stop_elapsed:?}",
            stats.device_latency_ms, stats.dropped_chunks
        );
        assert!(chunks >= 1, "expected at least one chunk within 3s (got {chunks})");
        assert!(stop_elapsed < Duration::from_secs(3), "stop() must be bounded (took {stop_elapsed:?})");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "start+stop must be bounded (took {:?})",
            started.elapsed()
        );
    }
}
