//! The media-clock pump (DRAGON-125 chunk B1): the integration layer wiring the
//! phase-3/4 mixer (`crate::mixer`) into the PipeWire OWNED recording path
//! (`record::pipewire`'s runtime fork) — the ONLY consumer; the legacy
//! wallclock+CFR+segments path is untouched and stays the runtime fallback.
//!
//! ## One clock (the DRAGON-125 B1 field regression's fix)
//!
//! There is exactly ONE [`MediaClock`] per session (`Arc<Mutex<_>>`), shared by
//! everything that maps wall→media time: the pump applies `Pause`/`Resume` to it
//! the INSTANT they're detected, the [`VideoTicker`] reads it for video ticks, the
//! render cycle reads it for audio horizons, and the [`Mixer`] shares the SAME
//! object (`Mixer::with_clock`) for chunk placement and control-event resolution.
//! B1 originally gave the mixer its own lazily-caught-up instance ("two clocks,
//! provably in agreement") — the instances did agree, but the design still had one
//! authority per timeline, and the first real-world stall in control detection
//! (below) shipped a recording whose audio and video disagreed about the pause.
//! One shared object makes that class structurally impossible: there is no second
//! clock to disagree. The `ControlLane` still queues `Pause`/`Resume` for the
//! render horizon to consume in media order — re-applying them to the
//! already-updated clock is a no-op (`MediaClock`'s idempotence) that yields the
//! identical `AppliedEvent`s chunk A's tests pin.
//!
//! ## Why `render_cycle`/`finish` nudge their target by [`RENDER_EPSILON_SECS`]
//!
//! While paused, the shared clock is FROZEN, so `render_cycle`'s `until_media =
//! clock.media_at(now - LAG)` computes the exact SAME value every cycle for the
//! whole pause. That value is also EXACTLY what a pending `Pause` control event
//! resolves to when `consume_through` reaches it (both are `media_at` of the same
//! wall instant). `consume_through`'s test is `media >= until_media ⇒ defer`, so an
//! EXACT tie would defer the event — and its appearance in the automation record —
//! until `Resume` lets the clock advance past that value. A sample-scale epsilon
//! breaks the tie: the first cycle at/after the pause consumes it. (Since the
//! one-clock fix this is automation bookkeeping only — tap placement and
//! pause-discard read the shared clock directly and never wait on consumption.)
//!
//! ## Threading model
//!
//! The pump is TWO threads (both spawned inside the caller's scope by [`spawn`]),
//! neither of which is the video thread:
//!
//! - The CONTROL thread (`run`) samples the pause flag + toggle events, feeds taps
//!   to the mixer, and renders every [`CYCLE`]. It performs NO blocking I/O — its
//!   render output goes into an unbounded channel — so no ffmpeg stall can ever
//!   delay pause/toggle detection. Load-bearing, learned live (the B1 field
//!   failure): ffmpeg n8.1.2 was observed to stop draining its audio FIFOs for ~9
//!   SECONDS mid-recording (encoder/scheduler startup burp), which blocked the
//!   original single-thread pump inside `write_all` for that whole stretch — a
//!   pause pressed during such a stall was applied seconds late (or missed
//!   entirely, when pause AND resume both fit inside one stall), silently shifting
//!   the recorded timeline against the user's intent.
//! - The WRITER thread ([`writer_loop`]) owns both FIFO write ends and performs
//!   every blocking write. If ffmpeg stops draining, THIS thread waits, deep in
//!   `write_all`, while the control thread keeps perfect time. Its exit is what
//!   delivers the FIFOs' EOF (see [`MediaClockPump::finish`]).
//!
//! The video thread stays decoupled for the same reason as ever: a `write_all` to
//! ffmpeg's stdin can block for as long as ffmpeg is slow to drain it (the
//! `MuxerWatchdog` hazard), and ffmpeg reads all three inputs via its own internal
//! threads, so a starved input can itself contribute to a scheduler wedge (see
//! CLAUDE.md's ffmpeg-8 notes). [`VideoTicker`] is the small, cheap-to-share
//! (`Arc<Mutex<MediaClock>>`) piece that lives on the VIDEO thread — reading
//! `media_at` is a microseconds-long lock, never blocking I/O.
//!
//! ## Every fd here is close-on-exec (the stop-wedge fix)
//!
//! The other half of the B1 field failure: the FIFO write ends were opened without
//! `O_CLOEXEC`, so child processes the app spawned while they were open (the audio
//! level meter, capture helpers) inherited duplicate write ends. The pump closing
//! ITS fds then never delivered EOF — ffmpeg's audio demuxer threads stayed blocked
//! in `read()` (observed live: `anon_pipe_read`, 30 seconds after the close), the
//! muxer couldn't flush its tail, and the session died at the DRAGON-118 reap bound
//! with the temp deleted. A leaked READ end is as bad in the other direction: it
//! would keep a blocked writer from ever seeing EPIPE after ffmpeg dies, defeating
//! the bounded-teardown chain (`PumpHandle::join`'s doc). So every open in this
//! module carries `O_CLOEXEC`, and a regression test pins it.
//!
//! ## Audio timing (the DRAGON-122-integration model, one sentence per stage)
//!
//! Sources stamp contiguously — one wall anchor per stream, then sample count
//! (see `crate::audio::capture`'s module doc, the model's home); the pump shifts
//! each tap's `audible_time` by PER-SESSION CONSTANTS only (the base
//! `audio_offset_ms`, plus — system track, auto mode — the device latency latched
//! once by [`latch_decision`]); `mixer::Track` places by `audible_time` against the
//! shared [`MediaClock`]. Constants preserve the sources' contiguity end-to-end, so
//! placement gap/truncation stats stay ~0 on a healthy session; anything
//! time-varying (arrival jitter, latency re-measures) is logged as drift, never
//! placed.
//!
//! ## Track indices
//!
//! Fixed by construction (see [`spawn`]): track 0 is the mono mic, track 1 the
//! stereo system audio — matching ffmpeg input order 1/2 in
//! [`crate::encode::spawn_ffmpeg_media_clock`].

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::audio::capture::{CaptureChunk, MonitorCapture};
use crate::audio::clean_mic::MicTapHandle;
use crate::audio::filters::duck::Ducker;
use crate::audio::filters::StreamTap;
use crate::mixer::clock::MediaClock;
use crate::mixer::control::{AppliedEvent, ControlEvent, ControlKind};
use crate::mixer::track::MixerStats;
use crate::mixer::{MixMode, Mixer, TrackSpec};

use super::{AudioChannel, ToggleEvent};

/// Track index for the mono mic (see the module doc).
const TRACK_MIC: usize = 0;
/// Track index for the stereo system audio (see the module doc).
const TRACK_SYS: usize = 1;

/// How far behind "now" the mixer's render horizon stays: control events (pause,
/// resume, per-channel mute) must be PUSHED before the horizon reaches their media
/// position, so this is the budget for "pushed" to mean "observed by the pump's own
/// polling cadence and queued" — comfortably wider than many cycles. A recorder
/// buffers deep; unlike a live-monitoring mixer there is no low-latency budget to
/// protect here (see `mixer::Mixer`'s own module doc).
const RENDER_LAG_SECS: f64 = 0.5;

/// How often the control thread drains its inputs and renders. One cycle is also
/// the pause/resume/toggle DETECTION latency (edges are stamped at the polling
/// instant), i.e. how far a recorded pause edge can land from the user's actual
/// press — so it's kept tight. That's affordable because the control thread
/// performs no blocking I/O (the writer split; module doc): a cycle is
/// microseconds of mixer work regardless of ffmpeg's state. (It was 100ms when
/// the same thread also carried the FIFO writes.)
const CYCLE: Duration = Duration::from_millis(10);

/// A sample-scale nudge added to every `render`/`finish` target derived from
/// `clock.media_at(..)` — see the module doc's "why the epsilon" section. Far below
/// one 48kHz sample period (~20.8µs), so it never affects a rendered frame position,
/// but large enough to break an exact-tie deadlock in `f64` terms at any realistic
/// session length.
const RENDER_EPSILON_SECS: f64 = 1e-6;

/// How long the pump's FIFO write-ends retry opening before giving up (mirrors
/// `system_relay`'s bounded, non-blocking rendezvous — see `open_fifo_write_end`'s
/// doc for why this can't simply reuse that helper's exact "retry until told to
/// stop" shape).
const FIFO_OPEN_BUDGET: Duration = Duration::from_secs(15);

/// How long `run` holds early system chunks back waiting for the capture client's
/// FIRST device-latency sample before latching 0.0 — see [`latch_decision`]. Must
/// stay comfortably under [`RENDER_LAG_SECS`]: chunks released after the latch are
/// placed at media positions this much in the past, and the render horizon (now −
/// LAG) must not have passed them yet. The capture client's first sample lands
/// ~300ms after ITS start, which precedes the pump (pre-flight order), so in
/// practice the latch resolves on the first drain; the budget only matters for a
/// server that never reports (suspended/virtual sinks).
const SYS_LATENCY_LATCH_BUDGET: Duration = Duration::from_millis(350);

/// The per-session device-latency latch (the DRAGON-122-integration timing model —
/// see `crate::audio::capture`'s module doc): the system track's audible-time shift
/// must be ONE constant per session, because `audible = capture + shift` only
/// preserves the capture stream's contiguity (and so `mixer::Track`'s gapless
/// placement) if `shift` never moves mid-stream — feeding the LIVE latency reading
/// into every chunk re-chops the track at every ~2s re-measure. Decide the constant
/// from the first available sample, or 0.0 once `waited` exhausts the budget
/// (matching the legacy path's fail-open). Pure, so the decision is unit-tested.
fn latch_decision(live: Option<f64>, waited: Duration) -> Option<f64> {
    live.or((waited >= SYS_LATENCY_LATCH_BUDGET).then_some(0.0))
}

/// Everything a fresh pump session needs besides its FIFOs (which the caller opens
/// separately — see [`spawn`]).
pub(crate) struct PumpConfig {
    pub(crate) fps: u32,
    /// The persisted base A/V-sync offset (ms; may be negative) — shifts EVERY
    /// audio tap's audible time by the same amount, mirroring the legacy
    /// `audio_offset_ms` base that both channels used to share at finalize.
    pub(crate) audio_offset_ms: i32,
    /// Whether to fold the system capture client's measured device latency into
    /// the system track's audible time — latched ONCE per session (see
    /// [`latch_decision`]; a moving shift would re-chop the contiguous stream).
    /// Off in manual mode (mirrors the legacy `monitor_extra_ms` gate,
    /// DRAGON-119): the user's `audio_offset_ms` then stands exactly as set.
    pub(crate) auto_device_compensation: bool,
    /// The mic channel's state at t=0 (`RecordSettings::mic` / `mic_armed()`) — the
    /// `on_at_start` `finish` needs to seed its mute-interval build correctly when
    /// no toggle ever happens.
    pub(crate) mic_on0: bool,
    /// The system channel's state at t=0 (`RecordSettings::system_audio`).
    pub(crate) sys_on0: bool,
    /// Duck the system track while the mic hears speech (DRAGON-128): a
    /// [`Ducker`] fed by the mic taps and applied to each system chunk BEFORE it
    /// reaches the mixer — a capture-time filter baked into the recorded system
    /// track (like the mic's own cleanup chain), NOT control-lane automation.
    pub(crate) duck_system: bool,
}

/// [`MediaClockPump::finish`]'s summary: per-channel mute intervals (media seconds,
/// the same shape [`super::finalize::off_intervals`] produces) plus diagnostics.
pub(crate) struct PumpOut {
    pub(crate) mic_off: Vec<(f64, f64)>,
    pub(crate) sys_off: Vec<(f64, f64)>,
    pub(crate) mic_stats: MixerStats,
    pub(crate) sys_stats: MixerStats,
    /// The session's final media length (seconds) — what the owned session's video
    /// loop must feed AT LEAST this many `1/fps` ticks to cover (see
    /// [`VideoTicker::ticks_to_cover`]) so `-shortest` trims video down to audio's
    /// end instead of truncating audio.
    pub(crate) final_media: f64,
}

impl PumpOut {
    /// The least-bad fallback when the pump thread is unrecoverably wedged (see
    /// [`PumpHandle::join`]): no mute intervals — the caller's own salvage path
    /// already treats a wedged muxer as a lost segment, so an unmuted channel here
    /// is a strictly smaller problem than hanging the recorder.
    fn empty() -> Self {
        Self {
            mic_off: Vec::new(),
            sys_off: Vec::new(),
            mic_stats: MixerStats::default(),
            sys_stats: MixerStats::default(),
            final_media: 0.0,
        }
    }
}

/// Shift `base` by `ms` (may be negative) — the one signed-instant-shift idiom every
/// audio-tap timing adjustment in this module goes through (`Duration` is unsigned,
/// so the sign needs explicit handling).
fn offset_instant(base: Instant, ms: f64) -> Instant {
    if ms >= 0.0 {
        base + Duration::from_secs_f64(ms / 1000.0)
    } else {
        base - Duration::from_secs_f64(-ms / 1000.0)
    }
}

/// Interleaved `samples` → little-endian bytes, written to `w`. `false` (write
/// failed — ffmpeg gone) is distinguished from "nothing to write" (empty slice,
/// always `true`) so a quiet cycle never counts as a failure.
fn write_pcm(w: &mut dyn Write, samples: &[f32]) -> bool {
    if samples.is_empty() {
        return true;
    }
    let mut buf = vec![0u8; samples.len() * 4];
    for (i, s) in samples.iter().enumerate() {
        buf[i * 4..i * 4 + 4].copy_from_slice(&s.to_le_bytes());
    }
    w.write_all(&buf).is_ok()
}

/// Open a FIFO's write end for the pump's own output: a bounded, non-blocking retry
/// (never an indefinite blocking open with nothing to release it), mirroring
/// `system_relay`'s rendezvous contract — but with a HARD deadline from the moment
/// this is called, rather than that module's "retry forever until an external stop
/// is requested, then 15s grace" (this one ALSO gives up as soon as `stop` trips, so
/// an aborted session start never rides out the whole budget). Clears `O_NONBLOCK`
/// on success so writes block normally (natural FIFO backpressure), matching every
/// other FIFO writer in this codebase. `O_CLOEXEC` is load-bearing (see the module
/// doc's close-on-exec section): a write fd leaked into any child process keeps
/// ffmpeg's audio input from ever seeing EOF.
///
/// MUST run on the pump's own thread, concurrently with the caller's video loop
/// already feeding ffmpeg's stdin — never on the thread that writes video, and
/// never BEFORE video bytes are flowing. Learned from a live wedge: ffmpeg opens
/// its inputs strictly in order, and finishing input 0 (the rawvideo pipe) means
/// PROBING it, which reads actual frame bytes from stdin — with no video written
/// yet, ffmpeg never reaches these FIFOs' read-side opens, this retry times out its
/// whole budget, and the session dies at start. (The legacy path never had the
/// hazard: clean_mic/system_relay open their FIFOs on their own threads while the
/// worker writes the first frame immediately after spawn.)
fn open_fifo_write_end(
    fifo: &std::path::Path,
    budget: Duration,
    stop: &AtomicBool,
) -> Option<std::fs::File> {
    use rustix::fs::{fcntl_getfl, fcntl_setfl, Mode, OFlags};
    let deadline = Instant::now() + budget;
    let fd = loop {
        match rustix::fs::open(
            fifo,
            OFlags::WRONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => break fd,
            Err(_) => {
                if Instant::now() >= deadline || stop.load(Ordering::Relaxed) {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    if let Ok(flags) = fcntl_getfl(&fd) {
        let _ = fcntl_setfl(&fd, flags & !OFlags::NONBLOCK);
    }
    Some(std::fs::File::from(fd))
}

/// The video-thread half of the pump (see the module doc's threading section): a
/// cheap, `Arc<Mutex<MediaClock>>`-backed tick counter the owned session's frame
/// loop queries to know how many copies of its newest frame to feed ffmpeg.
pub(crate) struct VideoTicker {
    clock: Arc<Mutex<MediaClock>>,
    fps: u32,
    /// Next video frame slot (index) not yet emitted.
    next_tick: u64,
}

impl VideoTicker {
    fn advance_to(&mut self, k: u64) -> u32 {
        let due = k.saturating_sub(self.next_tick);
        self.next_tick = self.next_tick.max(k); // never goes backwards
        due.min(u32::MAX as u64) as u32
    }

    /// How many frame slots (each `1/fps` of MEDIA time) have elapsed since the
    /// last call, at wall time `now`. Paused ⇒ media frozen ⇒ zero. `k = floor(media
    /// * fps)` is monotone non-decreasing (media time never runs backwards), so this
    /// never needs to go negative — a slow wakeup just returns a larger `due` (the
    /// caller feeds that many duplicate frames, the same "catch-up" shape
    /// `-fps_mode cfr` produces on the legacy path).
    pub(crate) fn due_video_ticks(&mut self, now: Instant) -> u32 {
        let media = self.clock.lock().map(|c| c.media_at(now)).unwrap_or(0.0);
        self.advance_to((media * self.fps as f64).floor() as u64)
    }

    /// Ticks still needed so the video's total frame count covers AT LEAST
    /// `media_secs` (rounds UP, unlike [`due_video_ticks`](Self::due_video_ticks)) —
    /// the media-clock mirror of the legacy last-frame-rewrite: called with the
    /// pump's `PumpOut::final_media` AFTER the pump's own final render, this
    /// guarantees the video stream's total length is at least the audio's, so
    /// `-shortest` (which ends the mux at the SHORTER stream) trims video down to
    /// audio's exact length instead of truncating audio (see `record::pipewire`'s
    /// owned-path stop tail for where the historical comment on this lived).
    pub(crate) fn ticks_to_cover(&mut self, media_secs: f64) -> u32 {
        self.advance_to((media_secs * self.fps as f64).ceil() as u64)
    }
}

/// One control-thread render step's PCM, handed to the writer thread. Both tracks
/// travel together so the writer interleaves them in the same order every cycle
/// (mic then system, matching ffmpeg input order).
struct PcmStep {
    mic: Vec<f32>,
    sys: Vec<f32>,
}

/// The writer half of the pump (see the module doc's threading section): owns both
/// FIFO write ends and performs EVERY blocking write, so the control thread can
/// never be stalled by ffmpeg's draining pace. On a failed write (ffmpeg gone) it
/// warns, trips the shared `stop` (a dead muxer ends the session regardless of what
/// the app-level flag says), and keeps DRAINING the channel — dropping the data —
/// so the control thread's sends stay cheap until it notices and finishes. Exit —
/// the channel hanging up after [`MediaClockPump::finish`]'s final render — is what
/// closes the FIFOs and delivers ffmpeg's audio EOF.
fn writer_loop(
    rx: Receiver<PcmStep>,
    mut mic_fifo: Box<dyn Write + Send>,
    mut sys_fifo: Box<dyn Write + Send>,
    stop: &AtomicBool,
) {
    let mut failed = false;
    while let Ok(step) = rx.recv() {
        if failed {
            continue;
        }
        if !write_pcm(&mut *mic_fifo, &step.mic) || !write_pcm(&mut *sys_fifo, &step.sys) {
            failed = true;
            log::warn!("media-clock pump: FIFO write failed (ffmpeg gone); stopping the session");
            stop.store(true, Ordering::Relaxed);
        }
    }
    // FIFOs drop here: EOF for ffmpeg's audio inputs.
}

/// The mixer-integration engine (the CONTROL half — see the module doc's threading
/// section): owns the [`Mixer`] (sharing the session's ONE [`MediaClock`], which the
/// [`VideoTicker`] also reads) and the running conversion of external signals (pause
/// flag, toggle events, audio taps) into mixer input. Rendered PCM goes to the
/// writer thread through `writer_tx` — never written here.
struct MediaClockPump {
    mixer: Mixer,
    clock: Arc<Mutex<MediaClock>>,
    writer_tx: std::sync::mpsc::Sender<PcmStep>,
    audio_offset_ms: i32,
    was_paused: bool,
    mic_on0: bool,
    sys_on0: bool,
    /// Every `TrackGain`/`Pause`/`Resume` the mixer's `Final`-mode render has
    /// reported consumed so far — converted to per-track mute intervals at
    /// [`finish`](Self::finish).
    automation: Vec<AppliedEvent>,
    /// The system-track sidechain ducker (DRAGON-128), when enabled — see
    /// [`PumpConfig::duck_system`]. Fed and applied on this control thread, the
    /// one place both audio streams meet.
    ducker: Option<Ducker>,
    /// The mic channel's CURRENT toggle state (seeded from `mic_on0`, tracked
    /// through [`push_toggle`](Self::push_toggle)) — the ducker's `live` gate: a
    /// muted mic's speech is cut from the file at finalize, so it must not carve
    /// ducks into the system track either.
    mic_live: bool,
}

impl MediaClockPump {
    /// `clock` is the session's single shared [`MediaClock`] — the mixer is built
    /// against the SAME object (`Mixer::with_clock`), so pause/resume applied here
    /// are instantly visible to chunk placement, render horizons, and the video
    /// ticker alike (module doc). Construction happens on the PUMP THREAD in
    /// production (after its FIFO rendezvous — see [`spawn`]), which is why the
    /// [`VideoTicker`] is NOT built here: the video thread needs it before the
    /// FIFOs are even open.
    fn new(
        clock: Arc<Mutex<MediaClock>>,
        cfg: &PumpConfig,
        writer_tx: std::sync::mpsc::Sender<PcmStep>,
    ) -> Self {
        let mixer = Mixer::with_clock(MixMode::Final, clock.clone(), &[
            TrackSpec { channels: 1, initial_gain: 1.0 }, // TRACK_MIC
            TrackSpec { channels: 2, initial_gain: 1.0 }, // TRACK_SYS
        ]);
        Self {
            mixer,
            clock,
            writer_tx,
            audio_offset_ms: cfg.audio_offset_ms,
            was_paused: false,
            mic_on0: cfg.mic_on0,
            sys_on0: cfg.sys_on0,
            automation: Vec::new(),
            ducker: cfg.duck_system.then(Ducker::new),
            mic_live: cfg.mic_on0,
        }
    }

    /// Place one already-DSP-processed mic block (see
    /// `crate::audio::clean_mic::setup_clean_mic_tap`'s doc — the tap's
    /// `capture_time`/`audible_time` already bake in the chain's processing latency)
    /// onto the mic track, shifted further by the session's base A/V-sync offset.
    fn push_mic_tap(&mut self, tap: StreamTap) {
        if let Some(d) = self.ducker.as_mut() {
            // The tap IS the post-gate signal (clean_mic's DSP already ran), so a
            // closed gate feeds silence here — noise can never duck (DRAGON-128).
            d.feed_sidechain(&tap.samples, self.mic_live);
        }
        let audible = offset_instant(tap.audible_time, self.audio_offset_ms as f64);
        self.mixer.push_tap(TRACK_MIC, StreamTap::new(tap.samples, tap.capture_time, audible));
    }

    /// Place one system-monitor chunk onto the system track: audible time =
    /// capture time + `device_latency_ms` (0 when auto device compensation is off —
    /// the caller decides, see `run`'s `drain_external`) + the base A/V-sync offset.
    fn push_sys_chunk(&mut self, samples: Vec<f32>, capture_wall: Instant, device_latency_ms: f64) {
        let mut samples = samples;
        if let Some(d) = self.ducker.as_mut() {
            d.process(&mut samples, 2);
        }
        let with_device = offset_instant(capture_wall, device_latency_ms);
        let audible = offset_instant(with_device, self.audio_offset_ms as f64);
        self.mixer.push_tap(TRACK_SYS, StreamTap::new(samples, capture_wall, audible));
    }

    /// Convert a `RecordHandle.paused` edge into a `Pause`/`Resume` control event,
    /// stamped at `at` (the pump's own detection time) — applied to BOTH the mixer's
    /// control lane (lag-consumed later, for audio) and this pump's own `clock`
    /// (applied immediately, for video ticks); see the module doc for why both.
    /// A no-op when `paused` doesn't actually change anything (mirrors
    /// `MediaClock::pause`/`resume`'s own idempotence).
    fn set_paused(&mut self, paused: bool, at: Instant) {
        if paused == self.was_paused {
            return;
        }
        self.was_paused = paused;
        let kind = if paused { ControlKind::Pause } else { ControlKind::Resume };
        self.mixer.push_event(ControlEvent { at, kind });
        if let Ok(mut c) = self.clock.lock() {
            if paused {
                c.pause(at);
            } else {
                c.resume(at);
            }
        }
    }

    /// Convert one drained `ToggleEvent` into a `TrackGain` control event (gain 1.0
    /// on, 0.0 off) at its original wall instant.
    fn push_toggle(&mut self, at: Instant, chan: AudioChannel, on: bool) {
        let track = match chan {
            AudioChannel::Mic => {
                self.mic_live = on; // the ducker's live gate (see `mic_live`'s doc)
                TRACK_MIC
            }
            AudioChannel::Sys => TRACK_SYS,
        };
        let gain = if on { 1.0 } else { 0.0 };
        self.mixer.push_event(ControlEvent { at, kind: ControlKind::TrackGain { track, gain } });
    }

    /// Render the mixer up to `until_media`, handing each track's samples to the
    /// writer thread and accumulating the consumed automation. A no-op (matching
    /// `Mixer::render`) when `until_media` doesn't advance the horizon. The send
    /// never blocks (unbounded channel) — see the module doc's threading section;
    /// a hung-up writer (session already failing) just discards.
    fn render_to(&mut self, until_media: f64) {
        let out = self.mixer.render(until_media);
        self.automation.extend(out.automation);
        let mut tracks = out.tracks;
        let sys = tracks.pop().unwrap_or_default();
        let mic = tracks.pop().unwrap_or_default();
        if mic.is_empty() && sys.is_empty() {
            return;
        }
        let _ = self.writer_tx.send(PcmStep { mic, sys });
    }

    /// One render cycle at wall time `now`: render up to `media_at(now - LAG) +
    /// EPSILON` (see [`RENDER_LAG_SECS`] and the module doc's "why the epsilon").
    fn render_cycle(&mut self, now: Instant) {
        let target = self
            .clock
            .lock()
            .map(|c| c.media_at(now - Duration::from_secs_f64(RENDER_LAG_SECS)) + RENDER_EPSILON_SECS)
            .unwrap_or(0.0);
        self.render_to(target);
    }

    /// Final render (to `media_at(now) + EPSILON`, no LAG holdback) + convert the
    /// accumulated automation into per-track mute intervals. Consumes `self` —
    /// dropping `writer_tx` with it hangs up the writer's channel, which is the
    /// writer thread's cue to flush its backlog and close both FIFOs (ffmpeg's audio
    /// EOF). This is the session's last step before the caller reaps ffmpeg.
    fn finish(mut self, now: Instant) -> PumpOut {
        let final_media = self.clock.lock().map(|c| c.media_at(now)).unwrap_or(0.0);
        self.render_to(final_media + RENDER_EPSILON_SECS);
        let mic_stats = self.mixer.stats(TRACK_MIC);
        let sys_stats = self.mixer.stats(TRACK_SYS);
        let mic_off = build_intervals(&self.automation, TRACK_MIC, self.mic_on0);
        let sys_off = build_intervals(&self.automation, TRACK_SYS, self.sys_on0);
        log::info!(
            "media-clock pump finished: media={final_media:.3}s mic(late={} paused_drop={} \
             gap={}) sys(late={} paused_drop={} gap={})",
            mic_stats.late_chunks, mic_stats.discarded_paused_chunks, mic_stats.gap_samples,
            sys_stats.late_chunks, sys_stats.discarded_paused_chunks, sys_stats.gap_samples,
        );
        PumpOut { mic_off, sys_off, mic_stats, sys_stats, final_media }
    }
}

/// Turn `automation`'s `TrackGain` events for `track` into the
/// `finalize::off_intervals`-shaped mute-interval list, on MEDIA seconds (no
/// wall-clock mapping needed — `AppliedEvent::media` already is the media
/// position). Reuses `finalize::off_intervals_from_pts`'s interval-building core
/// rather than re-deriving the same on/off/trailing-open logic a second time.
fn build_intervals(automation: &[AppliedEvent], track: usize, on_at_start: bool) -> Vec<(f64, f64)> {
    let pts: Vec<(f64, bool)> = automation
        .iter()
        .filter_map(|ev| match ev.kind {
            ControlKind::TrackGain { track: t, gain } if t == track => Some((ev.media, gain >= 1.0)),
            _ => None,
        })
        .collect();
    super::finalize::off_intervals_from_pts(pts, on_at_start)
}

/// The CONTROL thread's body: drain external signals + render every [`CYCLE`] until
/// `stop` is observed (tripped by the app, or by the writer thread on a dead
/// muxer), then do one final drain + [`MediaClockPump::finish`]. No blocking I/O
/// happens on this thread (module doc) — a cycle is microseconds, so pause/toggle
/// detection latency is bounded by [`CYCLE`] alone, regardless of ffmpeg's state.
/// `_mic_tap` and `monitor` are kept alive for the session's whole duration (their
/// captures feed `mic_rx`/`sys_rx`) and torn down here at the end — `monitor.stop()`
/// explicitly (it has no `Drop` of its own; see `crate::audio::capture`), `_mic_tap`
/// by simply dropping out of scope (its `Drop` impl handles its own bounded
/// teardown).
#[allow(clippy::too_many_arguments)]
fn run(
    mut pump: MediaClockPump,
    auto_device_compensation: bool,
    _mic_tap: MicTapHandle,
    mic_rx: Receiver<StreamTap>,
    monitor: MonitorCapture,
    sys_rx: Receiver<CaptureChunk>,
    stop: &AtomicBool,
    paused: &AtomicBool,
    events: &Mutex<Vec<ToggleEvent>>,
) -> PumpOut {
    // The system track's audible-time shift, latched ONCE per session (see
    // [`latch_decision`]): `None` = still deciding — chunks wait in `sys_pending`
    // (bounded by the latch budget, far under the render lag) so the whole stream
    // gets the SAME constant and stays contiguous. Manual mode latches 0.0
    // immediately (the user's `audio_offset_ms` then stands exactly as set).
    let mut sys_latch: Option<f64> = (!auto_device_compensation).then_some(0.0);
    let mut sys_pending: Vec<CaptureChunk> = Vec::new();
    let mut sys_first_seen: Option<Instant> = None;
    let mut latch_drift_logged = false;
    let mut drain_external = |pump: &mut MediaClockPump, at: Instant| {
        pump.set_paused(paused.load(Ordering::Relaxed), at);
        let drained = events.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default();
        for (t, chan, on) in drained {
            pump.push_toggle(t, chan, on);
        }
        while let Ok(tap) = mic_rx.try_recv() {
            pump.push_mic_tap(tap);
        }
        while let Ok(chunk) = sys_rx.try_recv() {
            match sys_latch {
                Some(latency) => pump.push_sys_chunk(chunk.samples, chunk.capture_wall, latency),
                None => {
                    sys_first_seen.get_or_insert(at);
                    sys_pending.push(chunk);
                }
            }
        }
        if sys_latch.is_none() {
            let waited =
                sys_first_seen.map_or(Duration::ZERO, |t| at.saturating_duration_since(t));
            if let Some(l) = latch_decision(monitor.latest_signed_latency_ms(), waited) {
                sys_latch = Some(l);
                log::info!("system-audio device latency latched at {l:.1}ms for this session");
                for c in sys_pending.drain(..) {
                    pump.push_sys_chunk(c.samples, c.capture_wall, l);
                }
            }
        } else if auto_device_compensation && !latch_drift_logged {
            // Monitoring only (the timing model: drift is logged, never chopped):
            // a live reading that wanders from the latched constant is worth one
            // note, but re-applying it would re-chop the placed stream.
            if let (Some(live), Some(latched)) = (monitor.latest_signed_latency_ms(), sys_latch)
                && (live - latched).abs() > 15.0
            {
                latch_drift_logged = true;
                log::info!(
                    "system-audio device latency drifted from its latched value \
                     ({latched:.1}ms -> {live:.1}ms); keeping the latch (contiguity wins)"
                );
            }
        }
    };
    loop {
        let now = Instant::now();
        drain_external(&mut pump, now);
        pump.render_cycle(now);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(CYCLE);
    }
    let now = Instant::now();
    drain_external(&mut pump, now);
    // A session shorter than the latch budget may end with the latch still
    // undecided: fail open exactly like the budget expiring (any held chunks are
    // this short session's WHOLE system track — they must not be dropped).
    if sys_latch.is_none() {
        let l = monitor.latest_signed_latency_ms().unwrap_or(0.0);
        for c in sys_pending.drain(..) {
            pump.push_sys_chunk(c.samples, c.capture_wall, l);
        }
    }
    let stats = monitor.stop(); // bounded ≤2s (DRAGON-118)
    if stats.dropped_chunks > 0 {
        log::warn!(
            "media-clock pump: system relay dropped {} chunk(s) (consumer backlog)",
            stats.dropped_chunks
        );
    }
    pump.finish(now)
}

/// Teardown handle for a spawned pump: the caller signals `stop` (shared with the
/// video loop — see `record::pipewire`) and then calls [`join`](Self::join) to
/// retrieve the session's [`PumpOut`]. Scoped (`'scope`) rather than a plain
/// `JoinHandle`: `run` borrows `events` (a plain `&Mutex<..>` from
/// `record_pipewire`'s signature, not an `Arc` — the recording-worker call chain
/// owns it, not this pump), so the pump thread is spawned inside a
/// `std::thread::scope` block the caller opens (see [`spawn`]), which is what lets
/// a NON-`'static` borrow be sent to another thread — the scope API guarantees the
/// thread is joined before the block exits, which `join` below does explicitly
/// anyway.
pub(crate) struct PumpHandle<'scope> {
    thread: std::thread::ScopedJoinHandle<'scope, PumpOut>,
    mic_fifo_path: std::path::PathBuf,
    sys_fifo_path: std::path::PathBuf,
}

impl<'scope> PumpHandle<'scope> {
    /// Wait for the CONTROL thread's [`MediaClockPump::finish`] to complete and
    /// return its [`PumpOut`]. Bounded (DRAGON-118 discipline): the control thread
    /// performs no blocking I/O, so it finishes within one render cycle of `stop`
    /// being observed (well under a second) — the grace loop is belt-and-suspenders
    /// for a descheduled thread, not an I/O wait. The WRITER thread may still be
    /// flushing (or blocked on a wedged ffmpeg) when this returns; its exit — and
    /// with it the enclosing `std::thread::scope`'s implicit join — is guaranteed
    /// by the caller's stop tail running INSIDE the same scope: `wait_or_kill`
    /// reaps ffmpeg before the scope closes, a dead ffmpeg's FIFO read ends close,
    /// and the writer's blocked `write_all` fails with EPIPE (no phantom readers
    /// can exist: every fd here is `O_CLOEXEC` — see the module doc) and it drains
    /// its hung-up channel and exits. If even the control thread failed to finish
    /// (it panicked, or a busy box descheduled it past the bound), this returns a
    /// synthesized empty [`PumpOut`] (the caller's salvage path already treats a
    /// wedged session as lost).
    pub(crate) fn join(self) -> PumpOut {
        let PumpHandle { thread, mic_fifo_path, sys_fifo_path } = self;
        let grace = Instant::now() + Duration::from_secs(2);
        while !thread.is_finished() && Instant::now() < grace {
            std::thread::sleep(Duration::from_millis(10));
        }
        // The FIFO files' lifetime ends with the session either way — unlink them
        // here (safe even while the writer holds its fds: the name goes, the fds
        // stay valid until dropped), mirroring `CleanMicHandle`/
        // `SystemRelayHandle` removing their FIFOs at teardown.
        let _ = std::fs::remove_file(&mic_fifo_path);
        let _ = std::fs::remove_file(&sys_fifo_path);
        if thread.is_finished() {
            thread.join().unwrap_or_else(|_| {
                log::error!("media-clock pump thread panicked; using an empty result");
                PumpOut::empty()
            })
        } else {
            log::warn!("media-clock pump control thread never finished; using an empty result");
            PumpOut::empty()
        }
    }
}

/// Spawn the pump's two threads (inside `scope`, so the control thread can borrow
/// `events` for exactly its own lifetime — see [`PumpHandle`]'s doc; the writer
/// thread is spawned from WITHIN the control thread, after its FIFO rendezvous, on
/// the same scope) — the media-clock owned session's audio engine, running until
/// `stop` (shared with the video loop). `stop`/`paused` are plain borrows (`'env`,
/// like `events`) rather than owned `Arc`s: every caller runs this inside its own
/// `std::thread::scope`, which already outlives the pump's threads by construction
/// (`PumpHandle::join` is always awaited before that scope closes), so there is
/// nothing an `Arc` would buy here — and a borrow is what lets a caller holding only
/// a `&AtomicBool` (no `Arc` in hand at all — e.g. the zero-copy workers'
/// `record::zero_copy`, whose own stop/paused come in as plain references from
/// their caller) use this exact same entry point instead of maintaining a second,
/// easily-desynced atomic just to satisfy an `Arc` requirement. The caller
/// (`record::pipewire`/`record::screencopy`/`record::zero_copy`) must have already:
/// created both FIFOs (`mkfifo`), spawned `spawn_ffmpeg_media_clock` (or its
/// zero-copy sibling) referencing their paths, and started `monitor`/`mic_tap`
/// (kept alive across the pre-flight "did the owned path come up" check — see
/// `record::pipewire`'s runtime fork).
///
/// Returns IMMEDIATELY (the ticker in hand): the FIFO write-end rendezvous happens
/// ON the spawned thread, not here — load-bearing ordering, learned from a live
/// wedge (see `open_fifo_write_end`'s doc): ffmpeg only reaches its FIFO read-side
/// opens after PROBING input 0, which reads real video bytes from stdin — and those
/// bytes come from the caller's video loop, which can only start once this returns.
/// Blocking here would deadlock the session start for the FIFO budget, every time.
/// If either FIFO never opens (bounded), the thread trips `stop`, tears down its
/// captures, and resolves to an empty [`PumpOut`] — the session's video side then
/// winds down through its normal wedge machinery (the muxer-liveness check /
/// watchdog: an ffmpeg with unopened inputs never writes its header). `Err` here
/// means only that the THREAD couldn't spawn.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    start: Instant,
    cfg: PumpConfig,
    mic_fifo_path: std::path::PathBuf,
    sys_fifo_path: std::path::PathBuf,
    mic_tap: MicTapHandle,
    mic_rx: Receiver<StreamTap>,
    monitor: MonitorCapture,
    sys_rx: Receiver<CaptureChunk>,
    stop: &'env AtomicBool,
    paused: &'env AtomicBool,
    events: &'env Mutex<Vec<ToggleEvent>>,
) -> Result<(PumpHandle<'scope>, VideoTicker), String> {
    let clock = Arc::new(Mutex::new(MediaClock::new(start)));
    let ticker = VideoTicker { clock: clock.clone(), fps: cfg.fps.max(1), next_tick: 0 };
    let thread_stop = stop;
    let (mic_path, sys_path) = (mic_fifo_path.clone(), sys_fifo_path.clone());
    let spawned = std::thread::Builder::new().name("cck-media-clock-pump".to_string()).spawn_scoped(
        scope,
        move || {
            // The FIFO rendezvous, on THIS thread (see the fn doc for why). On
            // failure: trip `stop` so the video loop winds down, and tear the
            // captures down explicitly — `MonitorCapture` has no `Drop` of its own
            // (see `crate::audio::capture`), so an early return that merely
            // dropped it would leak its background thread; `mic_tap`'s `Drop`
            // handles itself.
            let Some(mic_fifo) = open_fifo_write_end(&mic_path, FIFO_OPEN_BUDGET, thread_stop)
            else {
                log::warn!("media-clock pump: mic FIFO write end never opened (wedged ffmpeg?)");
                thread_stop.store(true, Ordering::Relaxed);
                drop(mic_tap);
                let _ = monitor.stop();
                return PumpOut::empty();
            };
            let Some(sys_fifo) = open_fifo_write_end(&sys_path, FIFO_OPEN_BUDGET, thread_stop)
            else {
                log::warn!("media-clock pump: system FIFO write end never opened (wedged ffmpeg?)");
                thread_stop.store(true, Ordering::Relaxed);
                drop(mic_tap);
                let _ = monitor.stop();
                return PumpOut::empty();
            };
            // The writer thread (module doc): owns the freshly-opened FIFOs and
            // every blocking write, spawned on the same scope (nested scoped
            // spawns are joined at scope exit like any other; see
            // `PumpHandle::join`'s doc for why its exit is guaranteed bounded).
            let (writer_tx, writer_rx) = std::sync::mpsc::channel::<PcmStep>();
            let writer_stop = thread_stop;
            let writer = std::thread::Builder::new().name("cck-pump-writer".to_string()).spawn_scoped(
                scope,
                move || writer_loop(writer_rx, Box::new(mic_fifo), Box::new(sys_fifo), writer_stop),
            );
            if let Err(e) = writer {
                log::warn!("media-clock pump: could not spawn its writer thread: {e}");
                thread_stop.store(true, Ordering::Relaxed);
                drop(mic_tap);
                let _ = monitor.stop();
                return PumpOut::empty();
            }
            let auto_device_compensation = cfg.auto_device_compensation;
            let pump = MediaClockPump::new(clock, &cfg, writer_tx);
            run(pump, auto_device_compensation, mic_tap, mic_rx, monitor, sys_rx, thread_stop, paused, events)
        },
    );
    match spawned {
        Ok(thread) => Ok((PumpHandle { thread, mic_fifo_path, sys_fifo_path }, ticker)),
        // `mic_tap`/`monitor` were already moved into the closure above, so a
        // thread-spawn failure here drops them (leaking `monitor`'s capture
        // thread, since it has no `Drop`) rather than stopping them cleanly — an
        // OS-out-of-threads failure this deep into recording start is already an
        // exceedingly unlikely, degraded-system situation, so this residual leak
        // is accepted rather than restructured around.
        Err(e) => Err(format!("media-clock pump: could not spawn its thread: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    fn base() -> Instant {
        Instant::now()
    }
    fn secs(s: f64) -> StdDuration {
        StdDuration::from_secs_f64(s)
    }

    fn cfg(fps: u32) -> PumpConfig {
        PumpConfig {
            fps,
            audio_offset_ms: 0,
            auto_device_compensation: true,
            mic_on0: true,
            sys_on0: true,
            duck_system: false,
        }
    }

    // ---- latch_decision ------------------------------------------------------

    #[test]
    fn latch_decision_takes_the_first_sample_or_fails_open_after_the_budget() {
        // A live sample decides immediately, whatever the wait so far (including a
        // genuine 0.0 — distinguishable from "no sample yet" since the getter
        // returns Option).
        assert_eq!(latch_decision(Some(12.5), StdDuration::ZERO), Some(12.5));
        assert_eq!(latch_decision(Some(0.0), StdDuration::ZERO), Some(0.0));
        // No sample yet: keep waiting inside the budget...
        assert_eq!(latch_decision(None, StdDuration::from_millis(100)), None);
        // ...and fail open to 0.0 once it's exhausted (suspended/virtual sinks
        // never report — the legacy path's same fail-open).
        assert_eq!(latch_decision(None, SYS_LATENCY_LATCH_BUDGET), Some(0.0));
    }

    // ---- offset_instant ------------------------------------------------------

    #[test]
    fn offset_instant_shifts_forward_and_backward() {
        let t0 = base();
        assert_eq!(offset_instant(t0, 250.0), t0 + secs(0.25));
        assert_eq!(offset_instant(t0, -250.0), t0 - secs(0.25));
        assert_eq!(offset_instant(t0, 0.0), t0);
    }

    // ---- VideoTicker: tick math -------------------------------------------------

    #[test]
    fn due_video_ticks_accumulates_correctly_across_multiple_polls() {
        let t0 = base();
        let clock = Arc::new(Mutex::new(MediaClock::new(t0)));
        let mut ticker = VideoTicker { clock, fps: 30, next_tick: 0 };
        // Query points comfortably clear of any tick boundary (33.3ms apart @
        // 30fps) so the assertions hold regardless of Duration/f64 rounding noise:
        // 101ms is solidly inside tick 3's window, 205ms solidly inside tick 6's.
        assert_eq!(ticker.due_video_ticks(t0 + secs(0.101)), 3); // floor(0.101*30) = 3
        assert_eq!(ticker.next_tick, 3);
        // Advancing further yields only the NEW ticks since the last call (6 total
        // minus the 3 already counted), not the same ones again.
        assert_eq!(ticker.due_video_ticks(t0 + secs(0.205)), 3); // floor(0.205*30) = 6
        assert_eq!(ticker.next_tick, 6);
        // No wall progress since the last query -> nothing new is due.
        assert_eq!(ticker.due_video_ticks(t0 + secs(0.205)), 0);
    }

    #[test]
    fn due_video_ticks_returns_a_burst_after_a_slow_wakeup() {
        let t0 = base();
        let clock = Arc::new(Mutex::new(MediaClock::new(t0)));
        let mut ticker = VideoTicker { clock, fps: 60, next_tick: 0 };
        assert_eq!(ticker.due_video_ticks(t0), 0);
        // A full second passes with no intermediate poll: one big catch-up burst.
        let due = ticker.due_video_ticks(t0 + secs(1.0));
        assert_eq!(due, 60);
        assert_eq!(ticker.next_tick, 60);
    }

    #[test]
    fn due_video_ticks_is_zero_across_a_pause() {
        let t0 = base();
        let clock = Arc::new(Mutex::new(MediaClock::new(t0)));
        {
            let mut c = clock.lock().unwrap();
            c.pause(t0 + secs(1.0));
        }
        let mut ticker = VideoTicker { clock: clock.clone(), fps: 30, next_tick: 0 };
        // Catch up to just before the pause: 30 ticks due (1.0s @ 30fps).
        assert_eq!(ticker.due_video_ticks(t0 + secs(1.0)), 30);
        // Deep into the pause: media is frozen at 1.0s, so no new ticks are due no
        // matter how much WALL time passes.
        assert_eq!(ticker.due_video_ticks(t0 + secs(5.0)), 0);
        assert_eq!(ticker.due_video_ticks(t0 + secs(50.0)), 0);
        // Resume 10s (wall) after the pause, at media 1.0 + 2.0 = 3.0s: exactly the
        // ticks for the 2s actually run resume, no credit for the paused stretch.
        {
            let mut c = clock.lock().unwrap();
            c.resume(t0 + secs(11.0));
        }
        let due = ticker.due_video_ticks(t0 + secs(13.0));
        assert_eq!(due, 60); // 2s of real running time @ 30fps
    }

    #[test]
    fn ticks_to_cover_rounds_up_and_is_monotone() {
        let t0 = base();
        let clock = Arc::new(Mutex::new(MediaClock::new(t0)));
        let mut ticker = VideoTicker { clock, fps: 30, next_tick: 10 };
        // 10 ticks already emitted (10/30 ~ 0.333s covered); covering 0.41s (well
        // clear of any tick boundary) needs ceil(0.41*30) = ceil(12.3) = 13, i.e. 3
        // more.
        assert_eq!(ticker.ticks_to_cover(0.41), 3);
        assert_eq!(ticker.next_tick, 13);
        // Asking for LESS than what's already covered emits nothing (never goes
        // backwards).
        assert_eq!(ticker.ticks_to_cover(0.0), 0);
        assert_eq!(ticker.next_tick, 13);
    }

    // ---- MediaClockPump: tap shifting + control conversion ---------------------

    /// A fake FIFO writer for tests: just accumulates bytes, like a real FIFO's
    /// reader would consume them, so pushed samples can be spot-checked.
    #[derive(Default, Clone)]
    struct FakeFifo(Arc<Mutex<Vec<u8>>>);
    impl Write for FakeFifo {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl FakeFifo {
        fn samples(&self) -> Vec<f32> {
            self.0
                .lock()
                .unwrap()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect()
        }
    }

    /// Everything `pump_with_fakes` builds, named: the pump + ticker, the two fake
    /// FIFOs, and the writer channel's receive side. Tests that assert on FIFO
    /// bytes run the REAL [`writer_loop`] over `writer_rx` after consuming the pump
    /// (`finish` drops the sender, hanging the channel up exactly like production)
    /// — the synchronous stand-in for the writer thread.
    struct PumpRig {
        pump: MediaClockPump,
        ticker: VideoTicker,
        mic: FakeFifo,
        sys: FakeFifo,
        writer_rx: Receiver<PcmStep>,
    }

    /// Test-side mirror of what `spawn`'s thread does for real: build the shared
    /// clock + ticker + pump as one consistent set (all anchored at `t0`), with the
    /// writer channel exposed instead of a live writer thread.
    fn pump_with_fakes(t0: Instant, fps: u32) -> PumpRig {
        let mic = FakeFifo::default();
        let sys = FakeFifo::default();
        let clock = Arc::new(Mutex::new(MediaClock::new(t0)));
        let ticker = VideoTicker { clock: clock.clone(), fps: fps.max(1), next_tick: 0 };
        let (writer_tx, writer_rx) = std::sync::mpsc::channel::<PcmStep>();
        let pump = MediaClockPump::new(clock, &cfg(fps), writer_tx);
        PumpRig { pump, ticker, mic, sys, writer_rx }
    }

    #[test]
    fn push_mic_tap_shifts_audible_time_by_the_base_offset_only() {
        let t0 = base();
        let PumpRig { mut pump, .. } = pump_with_fakes(t0, 30);
        pump.audio_offset_ms = 250;
        // capture_time == audible_time coming in (see clean_mic's tap doc) at media
        // 0.0; after the pump's shift it should land at media 0.25s.
        pump.push_mic_tap(StreamTap::new(vec![1.0; 480], t0, t0));
        let out = pump.mixer.render(1.0);
        // 0.25s of silence then the pushed 480 mono samples (10ms @ 48kHz).
        assert!(out.tracks[TRACK_MIC][..12_000].iter().all(|&s| s == 0.0));
        assert_eq!(&out.tracks[TRACK_MIC][12_000..12_480], &[1.0; 480][..]);
    }

    #[test]
    fn push_sys_chunk_shifts_by_device_latency_then_the_base_offset() {
        let t0 = base();
        let PumpRig { mut pump, .. } = pump_with_fakes(t0, 30);
        pump.audio_offset_ms = 100;
        // Device latency of 50ms (positive = system audio arrives later) + 100ms
        // base offset = 150ms total shift from `t0`.
        pump.push_sys_chunk(vec![1.0, 1.0, 1.0, 1.0], t0, 50.0);
        let out = pump.mixer.render(1.0);
        let ch = 2usize;
        let start_frame = (0.150 * crate::mixer::SAMPLE_RATE as f64).round() as usize;
        assert!(out.tracks[TRACK_SYS][..start_frame * ch].iter().all(|&s| s == 0.0));
        assert_eq!(
            &out.tracks[TRACK_SYS][start_frame * ch..start_frame * ch + 4],
            &[1.0, 1.0, 1.0, 1.0]
        );
    }

    #[test]
    fn push_sys_chunk_negative_device_latency_pulls_it_earlier() {
        let t0 = base();
        let PumpRig { mut pump, .. } = pump_with_fakes(t0, 30);
        // Push far enough in that a negative shift still lands at/after media 0.
        pump.push_sys_chunk(vec![2.0, 2.0], t0 + secs(1.0), -50.0);
        let out = pump.mixer.render(1.5);
        let start_frame = ((1.0 - 0.05) * crate::mixer::SAMPLE_RATE as f64).round() as usize;
        assert_eq!(&out.tracks[TRACK_SYS][start_frame * 2..start_frame * 2 + 2], &[2.0, 2.0]);
    }

    // ---- Ducking (DRAGON-128): the system-track sidechain filter ---------------

    /// A pump with ducking enabled, plus 20 frames of speech-level mic taps already
    /// fed (holding the duck engaged) — the shared setup for the ducking tests.
    fn ducked_pump_after_speech(t0: Instant, mic_live: bool) -> MediaClockPump {
        let clock = Arc::new(Mutex::new(MediaClock::new(t0)));
        let (writer_tx, _writer_rx) = std::sync::mpsc::channel::<PcmStep>();
        let mut c = cfg(30);
        c.duck_system = true;
        c.mic_on0 = mic_live;
        let mut pump = MediaClockPump::new(clock, &c, writer_tx);
        for i in 0..20 {
            let at = t0 + Duration::from_millis(i * 10);
            pump.push_mic_tap(StreamTap::new(vec![0.1; 480], at, at));
        }
        pump
    }

    #[test]
    fn ducker_lowers_system_chunks_while_the_mic_speaks() {
        let t0 = base();
        let mut pump = ducked_pump_after_speech(t0, true);
        // 0.3s of unity stereo system audio: the attack (0.1s) completes well inside
        // it, so the chunk starts near unity and ends fully ducked.
        pump.push_sys_chunk(vec![1.0; 28_800], t0, 0.0);
        let out = pump.mixer.render(0.4);
        let sys = &out.tracks[TRACK_SYS];
        assert!(sys[0] > 0.9, "the attack is a slew, not a jump (got {})", sys[0]);
        let last = sys[28_799];
        assert!(
            (0.2..0.3).contains(&last),
            "the tail should sit at the ducked gain (-12 dB), got {last}"
        );
        // Stereo frames carry one gain: L == R throughout.
        assert!(sys[..28_800].chunks(2).all(|f| f[0] == f[1]));
    }

    #[test]
    fn ducker_ignores_speech_while_the_mic_channel_is_muted() {
        let t0 = base();
        let mut pump = ducked_pump_after_speech(t0, false);
        pump.push_sys_chunk(vec![1.0; 28_800], t0, 0.0);
        let out = pump.mixer.render(0.4);
        assert!(
            out.tracks[TRACK_SYS][..28_800].iter().all(|&s| s == 1.0),
            "a muted mic must never duck the system track"
        );
    }

    #[test]
    fn ducking_off_leaves_system_chunks_byte_identical() {
        let t0 = base();
        let PumpRig { mut pump, .. } = pump_with_fakes(t0, 30);
        for i in 0..20 {
            let at = t0 + Duration::from_millis(i * 10);
            pump.push_mic_tap(StreamTap::new(vec![0.1; 480], at, at));
        }
        pump.push_sys_chunk(vec![1.0; 28_800], t0, 0.0);
        let out = pump.mixer.render(0.4);
        assert!(out.tracks[TRACK_SYS][..28_800].iter().all(|&s| s == 1.0));
    }

    #[test]
    fn set_paused_is_a_no_op_when_state_does_not_change() {
        let t0 = base();
        let PumpRig { mut pump, .. } = pump_with_fakes(t0, 30);
        pump.set_paused(false, t0 + secs(1.0)); // already running: ignored
        assert!(pump.automation.is_empty());
        let out = pump.mixer.render(2.0);
        assert!(out.automation.is_empty(), "no spurious control events reached the mixer");
    }

    #[test]
    fn set_paused_feeds_both_the_mixer_and_the_shared_clock_identically() {
        let t0 = base();
        let PumpRig { mut pump, mut ticker, .. } = pump_with_fakes(t0, 30);
        pump.set_paused(true, t0 + secs(1.0));
        pump.set_paused(false, t0 + secs(3.0));
        // Video ticker (immediate-apply clock): 1s ran, 2s paused, so at wall 3.5s
        // only 1.5s of MEDIA has elapsed -> 45 ticks @ 30fps.
        assert_eq!(ticker.due_video_ticks(t0 + secs(3.5)), 45);
        // Mixer (lag-consumed clock): rendering well past the pause/resume horizon
        // must report the SAME two control events, resolved to the SAME media
        // positions the immediate clock used.
        let out = pump.mixer.render(1.5);
        assert_eq!(out.automation.len(), 2);
        assert_eq!(out.automation[0].media, 1.0);
        assert_eq!(out.automation[1].media, 1.0);
        assert_eq!(out.tracks[TRACK_MIC].len(), (1.5 * crate::mixer::SAMPLE_RATE as f64) as usize);
    }

    #[test]
    fn push_toggle_maps_channels_to_the_fixed_track_indices() {
        let t0 = base();
        let PumpRig { mut pump, .. } = pump_with_fakes(t0, 30);
        pump.push_toggle(t0 + secs(0.5), AudioChannel::Mic, false);
        pump.push_toggle(t0 + secs(0.5), AudioChannel::Sys, false);
        let out = pump.mixer.render(1.0);
        let mut tracks: Vec<usize> = out
            .automation
            .iter()
            .filter_map(|ev| match ev.kind {
                ControlKind::TrackGain { track, .. } => Some(track),
                _ => None,
            })
            .collect();
        tracks.sort_unstable();
        assert_eq!(tracks, vec![TRACK_MIC, TRACK_SYS]);
    }

    // ---- build_intervals ---------------------------------------------------

    #[test]
    fn build_intervals_matches_off_intervals_from_pts_shape() {
        let automation = vec![
            AppliedEvent { media: 1.0, kind: ControlKind::TrackGain { track: TRACK_MIC, gain: 0.0 } },
            AppliedEvent { media: 3.0, kind: ControlKind::TrackGain { track: TRACK_MIC, gain: 1.0 } },
            AppliedEvent { media: 2.0, kind: ControlKind::TrackGain { track: TRACK_SYS, gain: 0.0 } },
        ];
        assert_eq!(build_intervals(&automation, TRACK_MIC, true), vec![(1.0, 3.0)]);
        assert_eq!(build_intervals(&automation, TRACK_SYS, true), vec![(2.0, 1.0e9)]);
        // Starting OFF merges the implicit [0, first-on) stretch into the interval.
        assert_eq!(build_intervals(&automation, TRACK_MIC, false), vec![(0.0, 3.0)]);
        // A track with no automation at all: on_at_start alone decides.
        assert_eq!(build_intervals(&automation, 5, true), Vec::<(f64, f64)>::new());
        assert_eq!(build_intervals(&automation, 5, false), vec![(0.0, 1.0e9)]);
    }

    // ---- One-clock pause awareness (the B1 field-regression fix) ----
    // `Track::push` decides retain-vs-discard through the session's ONE shared
    // clock (`Mixer::with_clock`), which `set_paused` updates the INSTANT an edge
    // is detected — so a chunk audible ANYWHERE inside a pause is discarded, no
    // matter how close to the pause edge it was captured. (Under the original
    // two-instance design the mixer's copy only learned of a pause via the lagged
    // render horizon, leaving a documented LAG-bounded window where a
    // barely-into-the-pause chunk was mistakenly retained — the scripted test
    // below pinned that leak; it now pins its absence.)
    fn step(pump: &mut MediaClockPump, t0: Instant, from_ms: u64, to_ms: u64) {
        let mut ms = from_ms;
        while ms <= to_ms {
            pump.render_cycle(t0 + Duration::from_millis(ms));
            ms += 100;
        }
    }

    #[test]
    fn scripted_session_end_to_end_taps_pause_toggle_finish() {
        let t0 = base();
        let PumpRig { mut pump, mut ticker, mic: mic_fifo, sys: sys_fifo, writer_rx } =
            pump_with_fakes(t0, 30);

        // 0.0s: mic + system audio flowing normally (comfortably clear of any pause).
        pump.push_mic_tap(StreamTap::new(vec![0.5; 480], t0, t0));
        pump.push_sys_chunk(vec![0.2, 0.2], t0, 0.0);
        step(&mut pump, t0, 100, 1000);

        // Pause for a full 2s (matching the real E2E scenario's magnitude).
        pump.set_paused(true, t0 + secs(1.0));
        // Captured barely into the pause: the shared clock is ALREADY paused (the
        // one-clock fix), so this is discarded — under the old two-instance design
        // this chunk leaked through the LAG window.
        pump.push_sys_chunk(vec![9.0, 9.0], t0 + secs(1.05), 0.0);
        step(&mut pump, t0, 1100, 2800);
        // Captured deep into the pause (1.9s in): discarded, same as ever.
        pump.push_sys_chunk(vec![8.0, 8.0], t0 + secs(2.9), 0.0);
        pump.set_paused(false, t0 + secs(3.0));
        step(&mut pump, t0, 2900, 4500);

        // 1.5s of media after resume: mute system audio (resolves to EXACTLY
        // media 1.0 + 1.5 = 2.5s — every wall offset above is a power-of-two
        // fraction, so this is exact float arithmetic, not an approximation).
        pump.push_toggle(t0 + secs(4.5), AudioChannel::Sys, false);
        step(&mut pump, t0, 4600, 6000);

        let out = pump.finish(t0 + secs(6.0));

        assert!((out.final_media - 4.0).abs() < 1e-6, "2s paused out of 6s wall = 4s media");
        assert_eq!(out.mic_off, Vec::<(f64, f64)>::new(), "mic was never toggled off");
        assert_eq!(out.sys_off, vec![(2.5, 1.0e9)], "sys muted from media 2.5s onward");
        assert_eq!(
            out.sys_stats.discarded_paused_chunks, 2,
            "BOTH chunks captured inside the pause are discarded — the shared clock is \
             already paused at push time (the one-clock fix; the near-edge chunk used to \
             leak through the old two-instance design's LAG window)"
        );

        // Video ticks must cover exactly the final media length (4.0s @ 30fps = 120).
        let already = ticker.due_video_ticks(t0 + secs(6.0));
        let more = ticker.ticks_to_cover(out.final_media);
        assert_eq!(already + more, 120, "total ticks must cover exactly the final media length");

        // Materialize the queued PCM through the REAL writer loop (`finish`
        // consumed the pump, hanging up the channel — production's EOF cue).
        let stop = AtomicBool::new(false);
        writer_loop(writer_rx, Box::new(mic_fifo.clone()), Box::new(sys_fifo.clone()), &stop);
        assert!(!stop.load(Ordering::Relaxed), "healthy fakes: the writer never trips stop");

        // PCM spot-check: the mic FIFO carries the pushed 0.5 block at the very
        // start; the sys FIFO carries the initial 0.2 block at the start; NEITHER
        // paused-window chunk appears anywhere in the output.
        let mic_samples = mic_fifo.samples();
        assert!(mic_samples[..480].iter().all(|&s| s == 0.5));
        let sys_samples = sys_fifo.samples();
        assert_eq!(&sys_samples[0..2], &[0.2, 0.2]);
        assert!(
            !sys_samples.chunks(2).any(|w| w == [9.0, 9.0]),
            "the near-edge paused chunk must NOT leak through (one-clock fix)"
        );
        assert!(
            !sys_samples.chunks(2).any(|w| w == [8.0, 8.0]),
            "the deep-in-pause chunk must never appear in the output"
        );
        // Length check: 4.0s media * 48kHz -> mic mono 192_000 samples, sys stereo
        // 384_000 (finish's epsilon never adds a frame; see RENDER_EPSILON_SECS).
        assert_eq!(mic_samples.len(), 192_000);
        assert_eq!(sys_samples.len(), 384_000);
    }

    // ---- The close-on-exec regression test (the B1 stop-wedge fix) ----
    // A FIFO write fd leaked into any spawned child keeps ffmpeg's audio input
    // from ever seeing EOF (observed live: its demuxer threads blocked in
    // `anon_pipe_read` 30s after the pump closed its own fds, killing the
    // session at the reap bound). Pin the flag on the fd `open_fifo_write_end`
    // actually returns.
    // FIFO write-end O_CLOEXEC discipline — a Linux-only concern (mkfifoat +
    // the record FIFO relay live on the Linux capture path; DRAGON-94).
    #[cfg(target_os = "linux")]
    #[test]
    fn fifo_write_end_is_close_on_exec() {
        use rustix::fs::{Mode, OFlags};
        let path = std::env::temp_dir().join(format!("cck-cloexec-{}.fifo", std::process::id()));
        let _ = std::fs::remove_file(&path);
        rustix::fs::mkfifoat(rustix::fs::CWD, &path, Mode::from_bits_truncate(0o600))
            .expect("mkfifo");
        // A read end must exist for the write-side open to succeed (FIFO
        // rendezvous) — non-blocking read opens succeed with no writer.
        let _read_end = rustix::fs::open(
            &path,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("read end");
        let stop = AtomicBool::new(false);
        let fifo = open_fifo_write_end(&path, Duration::from_secs(2), &stop)
            .expect("write end opens against the live read end");
        let flags = rustix::io::fcntl_getfd(&fifo).expect("F_GETFD");
        assert!(
            flags.contains(rustix::io::FdFlags::CLOEXEC),
            "the pump's FIFO write end must be close-on-exec: a leaked duplicate in any \
             child process silently blocks ffmpeg's EOF at stop (DRAGON-125 field failure)"
        );
        drop(fifo);
        let _ = std::fs::remove_file(&path);
    }
}
