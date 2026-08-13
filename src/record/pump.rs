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
use crate::audio::filters::duck::{DuckStats, Ducker};
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

/// How far behind "now" the mixer's render horizon stays. It buys two things:
///
/// 1. **The control budget.** Control events (pause, resume, per-channel mute) must be
///    PUSHED before the horizon reaches their media position, so this is the budget for
///    "pushed" to mean "observed by the pump's own polling cadence and queued" —
///    comfortably wider than many [`CYCLE`]s.
/// 2. **The source-lateness budget** (DRAGON-411, the reason this is no longer 0.5).
///    Once the horizon passes a media position, that position is rendered and gone:
///    audio arriving for it afterwards has nowhere to go and is DISCARDED
///    (`mixer::Track::push`'s late path). So this is also the maximum amount a source
///    may fall behind before its real audio starts being destroyed.
///
/// A source that falls behind recovers by RE-ANCHORING
/// ([`StreamAnchor::REANCHOR_THRESHOLD_SECS`](crate::audio::capture::StreamAnchor::REANCHOR_THRESHOLD_SECS)):
/// it jumps its contiguous stamp forward to the arrival-implied position, which fills
/// the missing stretch with silence ONCE, honestly, counted in `MixerStats::gap_samples`
/// — instead of drifting further and further behind until the horizon eats it. That
/// recovery is only worth anything if it can happen BEFORE the horizon does damage, so
/// the two constants must not sit at the same value (they both used to be 0.5, which is
/// the worst possible arrangement: loss could be counted but never avoided — the
/// DRAGON-411 field failure, 16.3% of a customer's mic track replaced by zeros). See
/// [`REANCHOR_HEADROOM`] for the margin this keeps.
///
/// **Why 1.5s, and why raise THIS side of the pair rather than lower the threshold.**
/// A recorder buffers deep: unlike a live-monitoring mixer there is no low-latency
/// budget to protect here (see `mixer::Mixer`'s own module doc), and nobody is
/// listening to the file while it is being written, so holding audio back costs
/// nothing a listener could perceive. OBS solves the same problem by GROWING its audio
/// buffer rather than discarding, up to `MAX_BUFFERING_TICKS` = 45 × 1024/48000 ≈ 960ms
/// — and it is a live mixer, feeding monitors and a stream. A file recorder can afford
/// more, so this is set to 3× the re-anchor threshold: the trigger fires a full second
/// of margin before anything can be dropped. Lowering the THRESHOLD to open the same
/// gap was rejected: `StreamAnchor` re-anchors on `drift.abs()`, and the NEGATIVE
/// direction (arrivals running ahead of the contiguous clock) is exactly what a
/// stalled-then-catching-up reader produces — the mic pipe alone can bank ~340ms of
/// audio (64KB at 192KB/s) and drain it in a burst. That burst is contiguous CAPTURE
/// data that the current threshold correctly absorbs; a lower one would re-anchor on
/// it, converting a delivery hiccup into a real forward jump and a real silence gap.
/// The threshold sits where the two hazards allow; the horizon is the side with room.
///
/// **What it costs.** MEMORY: each track's `VecDeque` holds roughly this much audio
/// between the render horizon and the write horizon — 1.5s is 288KB (mono mic) + 576KB
/// (stereo system) ≈ 0.9MB, up from ~0.3MB. STOP-TAIL LATENCY:
/// [`MediaClockPump::finish`] renders with no holdback, so the last ~1.5s of both
/// tracks is emitted in one step (≈864KB) and must reach ffmpeg before the FIFOs close.
/// That is memcpy-scale on the control thread (well inside `PumpHandle::join`'s 2s
/// grace) plus one extra second of AAC to encode in the writer/muxer — and the BOUND is
/// unchanged: the writer thread's exit is still guaranteed by the caller's bounded
/// `wait_or_kill` reap (DRAGON-118), which never grew.
const RENDER_LAG_SECS: f64 = 1.5;

/// The multiple of the source re-anchor threshold the render horizon must clear (see
/// [`RENDER_LAG_SECS`]): a source hits its re-anchor trigger, and still has
/// `(REANCHOR_HEADROOM − 1) × threshold` of further slippage available before the
/// horizon could discard anything it delivers. Pinned at compile time below AND by
/// `render_horizon_clears_the_reanchor_threshold` — the DRAGON-411 regression is
/// precisely these two numbers being allowed to converge again.
const REANCHOR_HEADROOM: f64 = 3.0;

const _: () = assert!(
    RENDER_LAG_SECS
        >= crate::audio::capture::StreamAnchor::REANCHOR_THRESHOLD_SECS * REANCHOR_HEADROOM,
    "the render horizon must clear the source re-anchor threshold by REANCHOR_HEADROOM \
     (DRAGON-411): with too little margin a lagging source can never re-anchor early \
     enough to be saved, and its real audio is discarded as `late_chunks`"
);

/// How many bytes ffmpeg 7.x reads from EACH f32le FIFO input before it will finish
/// opening that input and move on to the next (DRAGON-554, measured against the Flatpak
/// runtime's ffmpeg 7.1.3): EXACTLY 16384. 16320 bytes leave it blocked; 16384 open it.
/// It is a BYTE threshold, the same for the mono mic and the stereo system FIFO, and it
/// is avio-level buffering, not stream analysis: `-probesize 32`, `-analyzeduration 0`
/// and `-avioflags direct` do not lower it. ffmpeg 8 opens with zero bytes.
///
/// Why it matters: ffmpeg opens its inputs strictly in order, and while one hangs in
/// open it reads NOTHING from the video stdin either, so the worker's frame loop parks
/// in a blocking write. Without the opening prime (see [`OPENING_PRIME_FRAMES`]), the
/// first FIFO byte only exists once the render horizon reaches media 0 — at wall
/// [`RENDER_LAG_SECS`] — so every ffmpeg-7.x recording opened with ~1.1s of frozen
/// video ending in a content jump (the DRAGON-554 evidence file, measured
/// frame-by-frame: bit-identical frames from media 0.5s to 1.6s, then a jump).
pub(super) const FFMPEG7_INPUT_OPEN_BYTES: usize = 16384;

/// The opening prime (DRAGON-554): how much of the session's OPENING the control thread
/// renders IMMEDIATELY after the startup catch-up drain, instead of letting the render
/// horizon deliver the first byte at wall [`RENDER_LAG_SECS`]. Sized so the MONO mic
/// track (4 bytes per frame, the smaller stream) emits at least
/// [`FFMPEG7_INPUT_OPEN_BYTES`] (the stereo system track emits twice that), with a few
/// frames of slack so `f64` rounding can never come up one byte short of the threshold.
///
/// Honesty of the early render: the catch-up drain has ALREADY placed everything the
/// sources captured during the video-side startup (its whole job, DRAGON-417), so this
/// emits the same bytes the horizon would have emitted at wall 1.5s — real captured
/// audio plus each source's own lead-in silence — just early enough for ffmpeg's
/// input-open to eat them. What it gives up is the horizon's late-arrival protection
/// for the primed span ONLY: audio whose placement lands inside media [0, ~85ms) but
/// arrives after this render (a negative sync offset, or a large latched device latency
/// shifting a chunk earlier) is clipped and counted/logged like any late arrival.
/// Bounded by this constant, and the trade is deliberate: without it, EVERY ffmpeg-7.x
/// session (the Flatpak runtime) opens with ~[`RENDER_LAG_SECS`] of frozen video.
pub(super) const OPENING_PRIME_FRAMES: u64 = (FFMPEG7_INPUT_OPEN_BYTES as u64 / 4) + 8;

const _: () = assert!(
    (OPENING_PRIME_FRAMES as f64) / (crate::mixer::SAMPLE_RATE as f64) < RENDER_LAG_SECS / 4.0,
    "the opening prime must stay far under the render horizon (DRAGON-554): every primed \
     sample gives up the horizon's late-arrival protection at the session's head, and a \
     prime approaching RENDER_LAG_SECS would simply BE a shorter horizon, which is the \
     DRAGON-411 regression by another name"
);

/// MID-STREAM padded silence (seconds, either track) past which
/// [`MediaClockPump::finish`]'s summary is raised from `info` to `warn`. Lead-in is
/// excluded at the source (`MixerStats::leading_gap_samples`), so this counts only
/// silence substituted for audio that should have been there.
///
/// Chosen from measurement, not taste: across all seven E2E sessions a mid-stream hole
/// is 0.000s on an unpaused session and 16-45ms on a paused one (the resume landing a
/// few ms after the freeze — bounded, and per pause). A quarter second therefore sits
/// ~5x above the worst healthy value and tolerates a session with a dozen-plus pauses,
/// while still being far below the 5.07s the DRAGON-411 field failure wrote.
const GAP_NOTICE_SECS: f64 = 0.25;

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

/// How long [`PumpHandle::join`] waits for the control thread before synthesizing an
/// empty result.
///
/// On the NORMAL path the thread does no blocking I/O and finishes within one render
/// cycle, so almost all of this is headroom. It has to cover the ABANDON path too
/// though (see [`abandon_captures`]): a pump whose rendezvous failed tears its captures
/// down on this very thread, and each of those teardowns carries its own DRAGON-118
/// bound (the mic child's graceful stop, the tap reader's join, the monitor thread's
/// join). At 2s — shorter than the teardown it was waiting on — `join` reported "control
/// thread never finished" every single time a session failed to start, while the thread
/// was in fact doing exactly what it should. That is a false alarm in the one log a
/// failed recording leaves behind, so the grace covers the bound instead of contradicting
/// it. Nothing waits longer for it: the enclosing `std::thread::scope` joins that thread
/// before the worker can return either way.
const JOIN_GRACE: Duration = Duration::from_secs(8);

/// How long `run` holds early system chunks back waiting for the capture client's
/// FIRST device-latency sample before latching 0.0 — see [`latch_decision`]. Must
/// stay comfortably under [`RENDER_LAG_SECS`]: chunks released after the latch are
/// placed at media positions this much in the past, and the render horizon (now −
/// LAG) must not have passed them yet. The capture client's first sample lands
/// ~300ms after ITS start, which precedes the pump (pre-flight order), so in
/// practice the latch resolves on the first drain; the budget only matters for a
/// server that never reports (suspended/virtual sinks).
const SYS_LATENCY_LATCH_BUDGET: Duration = Duration::from_millis(350);

/// How long [`run`]'s startup catch-up drain may run before the render loop starts
/// regardless. Generous next to what it actually waits for (a few passes over
/// whatever the source channels buffered while the video side came up, plus the
/// [`SYS_LATENCY_LATCH_BUDGET`]), and bounded because nothing in the record path
/// waits unboundedly (DRAGON-118).
const STARTUP_CATCHUP_BUDGET: Duration = Duration::from_secs(2);

/// One [`run`] drain pass's outcome — what the startup catch-up loop needs to decide
/// it is level with the live capture.
struct DrainOutcome {
    /// Mic taps + system chunks taken off the source channels in this pass.
    drained: usize,
    /// Whether the system track's device-latency latch has resolved. Until it has,
    /// early system chunks sit in `sys_pending` and the render horizon must not pass
    /// them (see [`latch_decision`]).
    latch_decided: bool,
}

/// Whether a drain pass says the pump is level with the live capture rate: a backlog
/// hands over many items at once, a caught-up pump sees at most the block or two a
/// couple of milliseconds can have produced. Pure, so the decision is unit-tested.
fn caught_up(o: &DrainOutcome) -> bool {
    o.drained <= 2 && o.latch_decided
}

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

/// The dBFS a track with no samples, or with nothing but digital silence, reports.
/// A real `-inf` is correct and unreadable; this is the floor the log prints instead,
/// far below anything a capture chain produces.
const SILENT_DBFS: f64 = -120.0;

/// Sum of squares + sample count → RMS in dBFS (0 dBFS = full scale). Pure, unit-tested.
fn dbfs_rms(sum_sq: f64, samples: u64) -> f64 {
    if samples == 0 || sum_sq <= 0.0 {
        return SILENT_DBFS;
    }
    let rms = (sum_sq / samples as f64).sqrt();
    (20.0 * rms.log10()).max(SILENT_DBFS)
}

/// A track's aggregate loudness over the whole session: one running sum of squares and
/// one sample count, reported once at the end as a dBFS RMS.
///
/// PRIVACY: this is BEHAVIOUR, not content. It says how loud a track was on average,
/// which is what separates "we attenuated it" from "the source was quiet"; it cannot
/// reconstruct, transcribe or identify anything that was captured. The debug log's rule
/// is "what the app DID, never what the user captured", and a single number per track
/// per session is on the DID side of it.
#[derive(Debug, Clone, Copy, Default)]
struct Loudness {
    sum_sq: f64,
    samples: u64,
}

impl Loudness {
    /// Accumulate one block. Squaring at 48 kHz is a few nanoseconds per sample and
    /// does no I/O, so it is safe on the pump's control thread (which must never block).
    fn add(&mut self, samples: &[f32]) {
        let mut sum = 0.0f64;
        for s in samples {
            sum += (*s as f64) * (*s as f64);
        }
        self.sum_sq += sum;
        self.samples = self.samples.saturating_add(samples.len() as u64);
    }

    fn dbfs(&self) -> f64 {
        dbfs_rms(self.sum_sq, self.samples)
    }
}

/// The session's audio CONFIGURATION line, written once at pump start. Pure, unit-tested,
/// because the combination it exposes is easy to misread. A ducker is CONSTRUCTED whenever
/// the setting is on, but it only actually pulls the system track down while the mic is
/// LIVE: [`Ducker::feed_sidechain`] gates the sidechain on `mic_live` (seeded from `mic_on0`,
/// tracked through every toggle), so a mic that reaches no file cannot duck anything. The
/// note spells that out so `ducking=on` with `mic=off` does not read as a bug. Reading this
/// line must not require also reading `config.toml`.
fn audio_config_line(mic: bool, sys: bool, duck: bool) -> String {
    let on = |b: bool| if b { "on" } else { "off" };
    let note = if duck && !mic {
        " NOTE: ducking is enabled but INACTIVE, the mic is off so the sidechain never opens \
         (the system track is not attenuated)"
    } else {
        ""
    };
    format!(
        "media-clock pump audio config: mic={} system={} ducking={}{note}",
        on(mic),
        on(sys),
        on(duck)
    )
}

/// The session's audio LEVELS line, written once at pump finish, right after the health
/// summary. Pure, unit-tested, because it is the whole answer to "was the system track
/// attenuated by us, or was it quiet at the source": `sys_in` vs `sys_out` differing means
/// WE pulled it down (by the printed amount), and matching-but-low means the source was
/// quiet. Anything else would need the delivered file to be measured.
fn audio_levels_line(mic: &Loudness, sys_in: &Loudness, sys_out: &Loudness, duck: Option<DuckStats>) -> String {
    let (a, b) = (sys_in.dbfs(), sys_out.dbfs());
    let attenuation = (a - b).max(0.0);
    let verdict = if attenuation >= 0.5 {
        format!("we attenuated the system track by {attenuation:.1}dB")
    } else {
        "we did not attenuate the system track (its level is the source's own)".to_string()
    };
    let duck_part = match duck {
        Some(d) => format!(
            "duck(on sys_frames={} ducked_frames={} min_gain={:.3} sidechain_active={})",
            d.sys_frames, d.ducked_frames, d.min_gain, d.sidechain_active
        ),
        None => "duck(off)".to_string(),
    };
    format!(
        "media-clock pump audio levels: mic={:.1}dBFS sys_in={:.1}dBFS sys_out={:.1}dBFS \
         {duck_part}: {verdict}",
        mic.dbfs(),
        a,
        b
    )
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
#[cfg(unix)]
fn open_fifo_write_end(
    fifo: &std::path::Path,
    budget: Duration,
    stop: &AtomicBool,
) -> Option<std::fs::File> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(f) = try_open_fifo_write_end(fifo) {
            return Some(f);
        }
        if Instant::now() >= deadline || stop.load(Ordering::Relaxed) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// ONE non-blocking attempt at a FIFO's write end: `None` while no reader has it
/// open (ENXIO), the ready-to-write `File` (blocking mode, `O_CLOEXEC`) once one
/// does. The single step [`open_fifo_write_end`] loops over, split out so the
/// writer thread's PENDING sys rendezvous (`SysSink::Pending`, the `lab/flatpak`
/// ffmpeg 7.x deadlock fix) can retry once per render step instead of parking a
/// whole thread in the bounded sleep loop.
#[cfg(unix)]
fn try_open_fifo_write_end(fifo: &std::path::Path) -> Option<std::fs::File> {
    use rustix::fs::{fcntl_getfl, fcntl_setfl, Mode, OFlags};
    let fd = rustix::fs::open(
        fifo,
        OFlags::WRONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    if let Ok(flags) = fcntl_getfl(&fd) {
        let _ = fcntl_setfl(&fd, flags & !OFlags::NONBLOCK);
    }
    Some(std::fs::File::from(fd))
}

// Windows (DRAGON-229 M3): the pump's FIFO write-end rendezvous is the named-pipe
// `ConnectNamedPipe` (bounded, on the pump thread AFTER video bytes flow — the exact
// analog of the POSIX `open_fifo_write_end`), implemented in
// `crate::platform::windows::named_pipe::connect_write_end`. [`spawn`]'s
// `#[cfg(windows)]` arm calls it with the paired `PipeServer` handle instead of a path.

/// The warning for a FIFO/named-pipe write end that never opened, naming the reason it
/// actually failed rather than guessing at one (DRAGON-422).
///
/// PURE, and pinned by tests, because the wrong half of this sent a live investigation
/// after a wedged ffmpeg that did not exist. [`open_fifo_write_end`] gives up for two
/// completely different reasons: the budget expired with the session still running (a
/// muxer that really is stuck — the DRAGON-118/123 wedge this message was written for),
/// or `stop` was observed (the session is being torn down and the muxer is never going
/// to open anything). The PipeWire zero-copy worker reaches the second one routinely: it
/// spawns its muxer lazily, on the first dmabuf frame, so when no frame ever arrives
/// there was never an ffmpeg here to wedge — and the log said "wedged ffmpeg?" anyway.
fn rendezvous_failure(channel: &str, stopped: bool) -> String {
    if stopped {
        format!(
            "media-clock pump: session stopped before the muxer opened its {channel} audio \
             input; nothing was recorded"
        )
    } else {
        format!(
            "media-clock pump: {channel} FIFO write end never opened within {}s (wedged ffmpeg?)",
            FIFO_OPEN_BUDGET.as_secs()
        )
    }
}

/// Tear down the captures a pump session never got to use — its ONE abandon path
/// (DRAGON-422).
///
/// The receivers go FIRST, and that ordering is the whole point. The tap reader thread
/// hands blocks to `mic_rx` over a bounded channel and BLOCKS when it fills (~2.5s of
/// audio); a rendezvous that spends its budget leaves far more than that undrained, so
/// the reader is parked in `send` by the time we get here. Killing its ffmpeg does not
/// wake a blocked send, so `MicTapHandle::drain` used to spend its whole join budget and
/// then log "tap reader still stuck; detaching from it" — a leaked thread, and two
/// wasted seconds, on every failed start. Dropping the receivers first makes that send
/// fail immediately and the reader exit on its own, which is what the tap handle's drain
/// is written to expect.
fn abandon_captures(
    mic_tap: MicTapHandle,
    mic_rx: Receiver<StreamTap>,
    monitor: MonitorCapture,
    sys_rx: Receiver<CaptureChunk>,
) {
    drop(mic_rx);
    drop(sys_rx);
    drop(mic_tap); // bounded; its own Drop impl
    let _ = monitor.stop(); // bounded ≤2s (DRAGON-118); no Drop of its own
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

/// The writer's SYSTEM-track sink: already-open (Windows, whose named-pipe connects
/// both channels on the control thread exactly as before), or still pending its FIFO
/// rendezvous (POSIX, `lab/flatpak`).
///
/// WHY the POSIX sys rendezvous moved onto the writer: ffmpeg opens its inputs
/// strictly in order, and ffmpeg 7.x will not OPEN the sys FIFO until the MIC
/// input's stream analysis has read real audio data (ffmpeg 8 returns without any;
/// see `encode::command::fifo_input_args` for the measurements). The old shape,
/// where the control thread blocked opening the sys write end BEFORE any render
/// could run, therefore deadlocked on 7.x: no mic data until the sys open, no sys
/// open until mic data. Holding the sys sink PENDING lets renders start and mic
/// audio flow immediately after the mic rendezvous; ffmpeg eats the packet its
/// probe wants, opens the sys FIFO, and the writer's bounded retry completes.
enum SysSink {
    // POSIX release builds never CONSTRUCT this variant (their sys sink starts
    // Pending and the writer transitions to a plain boxed writer, not back into
    // the enum); Windows' spawn arm and the writer unit test do. Honestly dead
    // there, kept because it IS the type's meaning on the other platform.
    #[cfg_attr(unix, allow(dead_code))]
    Open(Box<dyn Write + Send>),
    #[cfg(unix)]
    Pending {
        path: std::path::PathBuf,
        /// The same [`FIFO_OPEN_BUDGET`] bound the old blocking open carried,
        /// stamped when the writer was handed the pending sink.
        deadline: Instant,
        /// System samples rendered while the open is pending, flushed in order the
        /// moment it completes. Bounded by construction: renders arrive in real
        /// time, so this holds at most `FIFO_OPEN_BUDGET` seconds of stereo f32
        /// (about 6 MB) before the deadline ends the session.
        buffered: Vec<f32>,
    },
}

/// The writer's MIC sink: the write end, plus (POSIX) the raw fd behind it.
///
/// The fd exists for ONE reason (DRAGON-629): while the sys sink is still
/// [`SysSink::Pending`], [`writer_loop`] drives this write end NON-BLOCKING, because on
/// that path a blocking mic write can deadlock the whole session. See
/// [`MicNonblocking`]. `fd` is `None` for the unit tests' in-memory fakes, which model a
/// full pipe themselves by returning [`std::io::ErrorKind::WouldBlock`].
pub(super) struct MicSink {
    w: Box<dyn Write + Send>,
    #[cfg(unix)]
    fd: Option<std::os::fd::RawFd>,
}

impl MicSink {
    /// The real FIFO write end (POSIX): its fd is kept so the pending phase can drive it
    /// non-blocking.
    #[cfg(unix)]
    fn real(w: std::fs::File) -> Self {
        use std::os::fd::AsRawFd;
        let fd = w.as_raw_fd();
        Self { w: Box::new(w), fd: Some(fd) }
    }

    /// The real named-pipe write end (Windows). No fd and no pending phase: that arm
    /// connects BOTH pipes on the control thread before the writer starts, so the
    /// DRAGON-629 ring cannot form there (see [`writer_loop`]).
    #[cfg(windows)]
    fn real<W: Write + Send + 'static>(w: W) -> Self {
        Self { w: Box::new(w) }
    }

    /// A test fake (or any sink with no fd of its own).
    #[cfg_attr(not(test), allow(dead_code))]
    fn boxed(w: Box<dyn Write + Send>) -> Self {
        #[cfg(unix)]
        {
            Self { w, fd: None }
        }
        #[cfg(not(unix))]
        Self { w }
    }
}

/// Scoped `O_NONBLOCK` on the mic write end, restored on drop. The same shape as
/// `record::owned`'s `NonblockingStdin`, for the same reason: a write that cannot park
/// is the only kind this thread may perform while it still owes somebody else progress.
///
/// A `None` fd (a unit-test fake) makes this a no-op guard; the fake supplies the
/// would-block behaviour directly.
#[cfg(unix)]
struct MicNonblocking {
    raw: Option<std::os::fd::RawFd>,
    restore: Option<rustix::fs::OFlags>,
}

#[cfg(unix)]
impl MicNonblocking {
    fn new(fd: Option<std::os::fd::RawFd>) -> Self {
        use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
        let Some(raw) = fd else {
            return Self { raw: None, restore: None };
        };
        // SAFETY: the fd is owned by the writer's mic sink, which outlives this guard
        // (the guard is scoped strictly inside the pending phase).
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) };
        let restore = fcntl_getfl(borrowed).ok();
        if let Some(flags) = restore {
            let _ = fcntl_setfl(borrowed, flags | OFlags::NONBLOCK);
        }
        Self { raw: Some(raw), restore }
    }
}

#[cfg(unix)]
impl Drop for MicNonblocking {
    fn drop(&mut self) {
        if let (Some(raw), Some(flags)) = (self.raw, self.restore) {
            // SAFETY: same validity reasoning as `new`: the owning `MicSink` outlives
            // this guard.
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) };
            let _ = rustix::fs::fcntl_setfl(borrowed, flags);
        }
    }
}

/// Push as much of `backlog` into `w` as it will take RIGHT NOW, dropping what was
/// accepted off the front. Never parks: the caller has set `O_NONBLOCK`, so a full pipe
/// reports [`std::io::ErrorKind::WouldBlock`] (or a zero-length write) and the remainder
/// stays queued, in order. `false` only on a REAL write error (ffmpeg gone).
#[cfg(unix)]
fn push_nonblocking(w: &mut dyn Write, backlog: &mut Vec<u8>) -> bool {
    while !backlog.is_empty() {
        match w.write(backlog) {
            Ok(0) => return true, // the pipe is full right now; keep the rest queued
            Ok(n) => {
                backlog.drain(..n);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return true,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    true
}

/// Interleaved `samples` → little-endian bytes appended to `out`. The byte-level form of
/// [`write_pcm`], for the pending phase's backlog (a partial write can land mid-sample,
/// so the queue has to be bytes, not floats).
#[cfg(unix)]
fn append_pcm(out: &mut Vec<u8>, samples: &[f32]) {
    out.reserve(samples.len() * 4);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
}

/// The writer half of the pump (see the module doc's threading section): owns both
/// audio sinks and performs EVERY blocking write, so the control thread can
/// never be stalled by ffmpeg's draining pace. On a failed write (ffmpeg gone) it
/// warns, trips the shared `stop` (a dead muxer ends the session regardless of what
/// the app-level flag says), and keeps DRAINING the channel — dropping the data —
/// so the control thread's sends stay cheap until it notices and finishes. Exit —
/// the channel hanging up after [`MediaClockPump::finish`]'s final render — is what
/// closes the FIFOs and delivers ffmpeg's audio EOF.
///
/// A [`SysSink::Pending`] sys sink resolves in a dedicated rendezvous phase FIRST:
/// retry the non-blocking open on the same 20ms cadence the blocking helper used,
/// draining any steps that arrive meanwhile (mic writes keep flowing, which is what
/// un-hungers ffmpeg 7.x's mic probe; sys samples buffer in order and flush the
/// moment the open lands). The retry cadence must NOT wait on steps: the first step
/// only arrives with the first render, and an ffmpeg that is ALREADY at its sys
/// open (ffmpeg 8, which probes without data) would sit blocked there for that
/// whole first-render latency, not consuming video stdin, and the session's opening
/// frames would land as late duplicates (caught by the E2E flash content checks).
/// The phase is bounded by its deadline and by `stop`, and a spent budget reports
/// through the same [`rendezvous_failure`] wording the old control-thread open used.
///
/// ## The mic write in that phase MUST NOT be able to park (DRAGON-629)
///
/// It used to be a plain blocking `write_all`, and that closed a deadlock ring with
/// ffmpeg. ffmpeg opens its inputs strictly IN ORDER and reads NOTHING while an open is
/// outstanding, so once it has the mic FIFO open and is sitting in `open()` on the sys
/// FIFO it is not draining mic either. Our writer then fills the mic pipe (64KB, ~341ms
/// of mono f32 at 48kHz), parks in `write_all`, and never returns to the sys-open retry
/// that ffmpeg is waiting on. Neither side can move: ffmpeg waits for a sys writer, we
/// wait for a mic reader. Nothing timed out at the right layer either, because both
/// budgets that could have caught it are LONGER than the muxer watchdog, so what the
/// user got was the recording failing outright with "ffmpeg accepted frames but never
/// wrote output" 12s in. Observed live with `sample`: ffmpeg in `avpriv_open`→`open()`
/// with only the mic fd open, `cck-pump-writer` in `write_pcm`.
///
/// So for the duration of the phase the mic end is `O_NONBLOCK` ([`MicNonblocking`]) and
/// anything the pipe will not take right now queues in `mic_backlog`, in order, exactly
/// as `buffered` already does for the sys side. Nothing is dropped and nothing is
/// reordered, so the mic stream's sample count IS still media time; the bytes are only
/// deferred, and the phase's own [`FIFO_OPEN_BUDGET`] deadline bounds how far (15s of
/// mono f32 is ~2.9MB, the same order as the sys buffer this phase already tolerates).
/// The moment the open lands the guard drops, blocking mode returns, and the backlog is
/// flushed AHEAD of anything later.
///
/// The alternative considered and rejected: moving the sys-open retry to the control
/// thread (it is non-blocking, so it would not violate that thread's no-blocking-I/O
/// rule) and leaving the mic write blocking. That does break the ring, but it puts a
/// rendezvous back on the most timing-sensitive loop in the recorder and needs a new
/// hand-off for the resolved sink, to fix a hazard that lives entirely inside this one
/// phase. Keeping it here, and making the one write that must not park unable to, is the
/// smaller and more local guarantee.
// Windows compiles `SysSink` with only its `Open` variant, making the match below
// single-armed there; the allow keeps that honest single arm instead of a cfg'd
// second match shape.
#[cfg_attr(windows, allow(clippy::infallible_destructuring_match))]
fn writer_loop(
    rx: Receiver<PcmStep>,
    mic_sink: MicSink,
    sys: SysSink,
    stop: &AtomicBool,
) {
    // Taken BEFORE the sink is destructured; the fd stays valid because `mic_fifo` (the
    // same open file) lives for the whole loop.
    #[cfg(unix)]
    let mic_raw = mic_sink.fd;
    let MicSink { w: mut mic_fifo, .. } = mic_sink;
    let mut failed = false;
    let mut sys_fifo: Box<dyn Write + Send> = match sys {
        SysSink::Open(w) => w,
        #[cfg(unix)]
        SysSink::Pending { path, deadline, mut buffered } => {
            // Whatever the mic pipe will not take right now, in order. See the fn doc:
            // this is what makes the phase's mic write unable to park, and with it the
            // DRAGON-629 deadlock ring impossible to close.
            let mut mic_backlog: Vec<u8> = Vec::new();
            let opened = {
                let _nb = MicNonblocking::new(mic_raw);
                loop {
                    // Try FIRST, bail-check second: a reader that shows up right at the
                    // deadline still rendezvouses instead of being refused on a tie.
                    if let Some(f) = try_open_fifo_write_end(&path) {
                        break Some(f);
                    }
                    if failed || Instant::now() >= deadline || stop.load(Ordering::Relaxed) {
                        break None;
                    }
                    // The 20ms wait doubles as the retry cadence; a step arriving inside
                    // it is drained immediately so mic audio never queues behind the
                    // rendezvous.
                    match rx.recv_timeout(Duration::from_millis(20)) {
                        Ok(step) => {
                            append_pcm(&mut mic_backlog, &step.mic);
                            buffered.extend_from_slice(&step.sys);
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
                    }
                    // Feed ffmpeg's mic probe whatever the pipe has room for, and NEVER
                    // wait on it: a full pipe here means ffmpeg is busy in its sys open,
                    // which is the very thing the next iteration retries.
                    if !push_nonblocking(&mut *mic_fifo, &mut mic_backlog) {
                        failed = true;
                        log::warn!(
                            "media-clock pump: FIFO write failed (ffmpeg gone); \
                             stopping the session"
                        );
                        stop.store(true, Ordering::Relaxed);
                    }
                }
            };
            // Blocking mode is back (the guard dropped with the scope above), so the
            // deferred mic bytes go out FIRST, ahead of anything the main loop writes.
            if opened.is_some() && !failed && !mic_backlog.is_empty() {
                if mic_fifo.write_all(&mic_backlog).is_err() {
                    failed = true;
                    log::warn!(
                        "media-clock pump: FIFO write failed (ffmpeg gone); \
                         stopping the session"
                    );
                    stop.store(true, Ordering::Relaxed);
                } else {
                    log::debug!(
                        "media-clock pump: flushed {} deferred mic bytes after the system \
                         FIFO rendezvous (DRAGON-629)",
                        mic_backlog.len()
                    );
                }
            }
            match opened {
                Some(mut f) => {
                    // Flush everything rendered while the open was pending, in
                    // order, so the stream's sample count stays the media time.
                    if !failed && !write_pcm(&mut f, &buffered) {
                        failed = true;
                        log::warn!(
                            "media-clock pump: FIFO write failed (ffmpeg gone); \
                             stopping the session"
                        );
                        stop.store(true, Ordering::Relaxed);
                    }
                    Box::new(f)
                }
                None => {
                    if !failed {
                        // Read `stop` BEFORE tripping it, so the report says
                        // "stopped" only when the session really was being torn
                        // down (DRAGON-422).
                        log::warn!(
                            "{}",
                            rendezvous_failure("system", stop.load(Ordering::Relaxed))
                        );
                        stop.store(true, Ordering::Relaxed);
                    }
                    // Same shape as a failed write: drain the channel (dropping the
                    // data) until it hangs up, then exit; the mic FIFO drops here
                    // and delivers its EOF.
                    while rx.recv().is_ok() {}
                    return;
                }
            }
        }
    };
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
    // Sinks drop here: EOF for ffmpeg's audio inputs.
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
    /// Aggregate loudness of the mic tap as it arrives (post-cleanup), of the system
    /// track BEFORE the ducker, and of the same system track AFTER it. Reported once
    /// at [`finish`](Self::finish); see [`Loudness`] for why this is behaviour, not
    /// captured content.
    mic_loud: Loudness,
    sys_loud_in: Loudness,
    sys_loud_out: Loudness,
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
            TrackSpec { channels: 1, initial_gain: 1.0, label: "mic" }, // TRACK_MIC
            TrackSpec { channels: 2, initial_gain: 1.0, label: "system" }, // TRACK_SYS
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
            mic_loud: Loudness::default(),
            sys_loud_in: Loudness::default(),
            sys_loud_out: Loudness::default(),
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
        self.mic_loud.add(&tap.samples);
        let audible = offset_instant(tap.audible_time, self.audio_offset_ms as f64);
        self.mixer.push_tap(TRACK_MIC, StreamTap::new(tap.samples, tap.capture_time, audible));
    }

    /// Place one system-monitor chunk onto the system track: audible time =
    /// capture time + `device_latency_ms` (0 when auto device compensation is off —
    /// the caller decides, see `run`'s `drain_external`) + the base A/V-sync offset.
    fn push_sys_chunk(&mut self, samples: Vec<f32>, capture_wall: Instant, device_latency_ms: f64) {
        let mut samples = samples;
        // Measured either side of the ducker, which is the ONE thing between the
        // capture client and the mixer that can change this track's level.
        self.sys_loud_in.add(&samples);
        if let Some(d) = self.ducker.as_mut() {
            d.process(&mut samples, 2);
        }
        self.sys_loud_out.add(&samples);
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
        let froze_at = self.clock.lock().ok().map(|mut c| {
            if paused {
                c.pause(at);
            } else {
                c.resume(at);
            }
            c.media_at(at)
        });
        // Pause = the media clock freezes, and nothing captured past the freeze is
        // recorded (DRAGON-411). The sources' contiguous stamps run AHEAD of wall time,
        // so taps already placed may sit past this position — captured after the user
        // pressed pause, and squatting on the media ground the resume is about to fill.
        // Take them back now, while the render horizon is still `RENDER_LAG_SECS` behind
        // and nothing here has reached ffmpeg.
        if let (true, Some(media)) = (paused, froze_at) {
            self.mixer.truncate_to(media);
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
        // THE recording health line — promoted out of `info` by DRAGON-419, classified
        // by the facts in [`log_audio_health`] (DRAGON-411). One line, one channel:
        // `crate::diag` wraps the `log` facade, so this reaches the debug file with no
        // per-call-site opt-in, and so does the discard `error` at the drop site.
        log_audio_health(final_media, &mic_stats, &sys_stats);
        // The LEVELS line, always `info` and always separate from the health line above:
        // it answers a different question (how loud was each track, and did we change
        // it) and must not inherit that line's severity.
        log::info!(
            "{}",
            audio_levels_line(
                &self.mic_loud,
                &self.sys_loud_in,
                &self.sys_loud_out,
                self.ducker.as_ref().map(|d| d.stats()),
            )
        );
        PumpOut { mic_off, sys_off, mic_stats, sys_stats, final_media }
    }
}

/// How a finished session's audio placement reads (DRAGON-411/419). Pure, so the level
/// choice is unit-tested instead of being inferred from a log line — which is how the
/// original defect stayed invisible: the summary below was `log::info!` while the
/// default filter is `warn` (`main.rs`), so no user has ever seen it. It is THE
/// recording health line: the one read that would have named the DRAGON-411 crackling
/// (16% of a customer's track replaced by digital silence) on the spot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioHealth {
    /// Everything captured was placed; nothing meaningful was padded.
    Clean,
    /// Nothing was destroyed, but silence was written MID-STREAM — a source genuinely
    /// stopped delivering for a while (a device stall, a re-anchor's one-time honest
    /// fill). A source's lead-in never counts: nothing is missing, it had not spoken
    /// yet.
    Padded,
    /// Captured audio reached the mixer behind the render horizon and was DISCARDED.
    /// Never acceptable: see [`RENDER_LAG_SECS`].
    Lost,
}

/// `gap_samples`/`late_frames` are FRAME counts (see [`MixerStats`]) — seconds at the
/// mixer's fixed rate.
fn frames_to_secs(frames: u64) -> f64 {
    frames as f64 / crate::mixer::SAMPLE_RATE as f64
}

/// Classify a finished session from both tracks' placement stats.
///
/// **Read the counters, not their names.** The obvious predicate — `late_chunks +
/// gap_samples > 0`, which DRAGON-419 first promoted this line with — reports EVERY
/// healthy recording as a loss, for two independent reasons, and a warning that fires
/// every time is one everybody learns to scroll past:
///
/// - `late_chunks` is non-zero on every session by DESIGN since DRAGON-417. The sources
///   are already delivering when the recorder anchors its clock, so the opening taps sit
///   before media 0; signed placement keeps whatever tail of them belongs in the file and
///   drops the rest. Nothing is missing from the recording. Only [`MixerStats::late_frames`]
///   — the part of a drop at media >= 0 — is audio the user actually lost.
/// - `gap_samples` USED to be non-zero on every session too — 54-136ms on both tracks
///   across all seven E2E sessions — because it lumped the source's LEAD-IN (silence
///   before its first sample, which is not a defect: nothing is missing, the source had
///   simply not spoken yet) together with mid-stream holes. Those are now separate
///   counters ([`MixerStats::leading_gap_samples`]), so `gap_samples` means what the
///   report needs it to mean: silence substituted for audio that should have been there.
fn audio_health(mic: &MixerStats, sys: &MixerStats) -> AudioHealth {
    if mic.late_frames > 0 || sys.late_frames > 0 {
        return AudioHealth::Lost;
    }
    let worst_gap = frames_to_secs(mic.gap_samples.max(sys.gap_samples));
    if worst_gap > GAP_NOTICE_SECS {
        return AudioHealth::Padded;
    }
    AudioHealth::Clean
}

/// The session's one audio-placement summary, at a level that MATCHES what happened
/// (DRAGON-411/419): discarded audio is an `error` — it is unrecoverable data loss from
/// the user's recording — written silence is a `warn`, and a clean session stays the
/// historical `info`. `paused_drop` never raises the level on its own (a paused pipeline
/// captures nothing, by design).
///
/// Raw counts stay in the line even when they do not raise the level: `late=2 lost=0.000s
/// pre_start=2` reads as "two chunks dropped, none of it yours", which is exactly what a
/// reader needs to not chase it.
fn log_audio_health(final_media: f64, mic: &MixerStats, sys: &MixerStats) {
    let line = format!(
        "media-clock pump finished: media={final_media:.3}s \
         mic(late={} lost={:.3}s paused_drop={} pre_start={} gap={:.3}s lead_in={:.3}s) \
         sys(late={} lost={:.3}s paused_drop={} pre_start={} gap={:.3}s lead_in={:.3}s)",
        mic.late_chunks,
        frames_to_secs(mic.late_frames),
        mic.discarded_paused_chunks,
        mic.pre_start_chunks,
        frames_to_secs(mic.gap_samples),
        frames_to_secs(mic.leading_gap_samples),
        sys.late_chunks,
        frames_to_secs(sys.late_frames),
        sys.discarded_paused_chunks,
        sys.pre_start_chunks,
        frames_to_secs(sys.gap_samples),
        frames_to_secs(sys.leading_gap_samples),
    );
    match audio_health(mic, sys) {
        AudioHealth::Lost => log::error!(
            "{line} — captured audio was DISCARDED (it reached the mixer behind the render \
             horizon); that much of the recording is silence"
        ),
        AudioHealth::Padded => {
            log::warn!("{line} — silence was written where a source stopped delivering")
        }
        AudioHealth::Clean => log::info!("{line}"),
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
        let mut drained_items = 0usize;
        pump.set_paused(paused.load(Ordering::Relaxed), at);
        let drained = events.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default();
        for (t, chan, on) in drained {
            pump.push_toggle(t, chan, on);
        }
        while let Ok(tap) = mic_rx.try_recv() {
            drained_items += 1;
            pump.push_mic_tap(tap);
        }
        while let Ok(chunk) = sys_rx.try_recv() {
            drained_items += 1;
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
        DrainOutcome { drained: drained_items, latch_decided: sys_latch.is_some() }
    };
    // Startup catch-up, BEFORE the first render (DRAGON-417). Media 0 is the instant
    // audio capture began, which is earlier than this thread's first cycle by the
    // whole video-side startup plus the FIFO rendezvous. All of that audio is already
    // queued in the source channels (and the mic reader may be BLOCKED on a full one,
    // so a single pass can't see it all), while the very first `render_cycle` would
    // jump the render horizon straight to `media_at(now) − RENDER_LAG_SECS` — past
    // material still in the queue, which then reads as late and is dropped. Drain
    // until a pass comes back level with the live capture rate (and the system-latency
    // latch has resolved, so no chunk is being held back either), bounded so a source
    // that never settles can't stall the session start.
    let catchup_deadline = Instant::now() + STARTUP_CATCHUP_BUDGET;
    loop {
        let outcome = drain_external(&mut pump, Instant::now());
        if caught_up(&outcome) || stop.load(Ordering::Relaxed) {
            break;
        }
        if Instant::now() >= catchup_deadline {
            log::warn!(
                "media-clock pump: startup catch-up still behind after {STARTUP_CATCHUP_BUDGET:?} \
                 ({} item(s) in the last pass); starting the render loop anyway",
                outcome.drained
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    // DRAGON-554: render the opening slice NOW, not at wall RENDER_LAG_SECS. ffmpeg
    // 7.x will not finish OPENING an audio input until it has read
    // FFMPEG7_INPUT_OPEN_BYTES from it, and while an input hangs in open ffmpeg reads
    // no video from stdin either — the worker's frame loop parks in a blocking write
    // and the session opens on ~RENDER_LAG_SECS of frozen frames (the DRAGON-554
    // evidence file). The catch-up drain above already placed everything captured so
    // far, so this emits the same opening bytes the horizon would have emitted at
    // wall 1.5s. See OPENING_PRIME_FRAMES' doc for the bounded late-arrival trade.
    pump.render_to(OPENING_PRIME_FRAMES as f64 / crate::mixer::SAMPLE_RATE as f64);
    log::debug!(
        "media-clock pump: opening prime rendered ({OPENING_PRIME_FRAMES} frames per track) \
         so the muxer's input-open never waits on the render horizon (DRAGON-554)"
    );
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
        let grace = Instant::now() + JOIN_GRACE;
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
/// Returns IMMEDIATELY (the ticker in hand): the MIC write-end rendezvous happens
/// ON the spawned thread, not here — load-bearing ordering, learned from a live
/// wedge (see `open_fifo_write_end`'s doc): ffmpeg only reaches its FIFO read-side
/// opens after PROBING input 0, which reads real video bytes from stdin — and those
/// bytes come from the caller's video loop, which can only start once this returns.
/// Blocking here would deadlock the session start for the FIFO budget, every time.
/// The SYS rendezvous goes one step further on POSIX (`lab/flatpak`, the ffmpeg 7.x
/// deadlock): it rides the WRITER thread as a pending sink (see [`SysSink`]),
/// because ffmpeg 7.x will not even open the sys FIFO until the mic input's probe
/// has read real audio, which only flows once `run` is rendering. If a rendezvous
/// never completes (all bounded), the owning thread trips `stop` and the session
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
    // Windows (DRAGON-229 M3): the paired named-pipe SERVER handles (from
    // `OwnedAudioStart`) the pump `ConnectNamedPipe`s on its own thread, in place of the
    // POSIX `open_fifo_write_end(path)`. Only the Windows worker passes these; the
    // Linux/mac call sites stay byte-identical (the params don't exist off Windows).
    #[cfg(windows)] mic_pipe: crate::platform::windows::named_pipe::PipeServer,
    #[cfg(windows)] sys_pipe: crate::platform::windows::named_pipe::PipeServer,
) -> Result<(PumpHandle<'scope>, VideoTicker), String> {
    let clock = Arc::new(Mutex::new(MediaClock::new(start)));
    // The opening span (DRAGON-417): how much media time already elapsed — i.e. how
    // much audio was already captured — before the caller's video side was ready to
    // feed its first frame. The caller covers it with copies of that frame. It used
    // to be invisible AND lossy: audio captured in this window was discarded with no
    // error and nothing in any log, so a truncated recording could only be found by
    // listening back. Logged unconditionally so the next one is one `journalctl` away.
    let opening = Instant::now().saturating_duration_since(start);
    log::info!(
        "media-clock session: anchored at audio-capture start, {}ms of it before the \
         video side was ready (that span opens on the first captured frame)",
        opening.as_millis()
    );
    // What the audio side was actually asked to do, in one line, so a finished
    // recording's log answers "was the mic even in this file, and could the ducker
    // have touched the system track" without anyone reading the settings file.
    log::info!("{}", audio_config_line(cfg.mic_on0, cfg.sys_on0, cfg.duck_system));
    let ticker = VideoTicker { clock: clock.clone(), fps: cfg.fps.max(1), next_tick: 0 };
    let thread_stop = stop;
    #[cfg(not(windows))]
    let (mic_path, sys_path) = (mic_fifo_path.clone(), sys_fifo_path.clone());
    let spawned = std::thread::Builder::new().name("cck-media-clock-pump".to_string()).spawn_scoped(
        scope,
        move || {
            // The FIFO/named-pipe rendezvous, on THIS thread (see the fn doc for why). On
            // failure: trip `stop` so the video loop winds down, and tear the
            // captures down explicitly — `MonitorCapture` has no `Drop` of its own
            // (see `crate::audio::capture`), so an early return that merely
            // dropped it would leak its background thread; `mic_tap`'s `Drop`
            // handles itself.
            #[cfg(not(windows))]
            let mic_fifo = open_fifo_write_end(&mic_path, FIFO_OPEN_BUDGET, thread_stop);
            #[cfg(windows)]
            let mic_fifo = crate::platform::windows::named_pipe::connect_write_end(
                mic_pipe, FIFO_OPEN_BUDGET, thread_stop,
            );
            let Some(mic_fifo) = mic_fifo else {
                log::warn!("{}", rendezvous_failure("mic", thread_stop.load(Ordering::Relaxed)));
                thread_stop.store(true, Ordering::Relaxed);
                abandon_captures(mic_tap, mic_rx, monitor, sys_rx);
                return PumpOut::empty();
            };
            // POSIX: the SYS rendezvous is handed to the WRITER as a pending sink
            // instead of being blocked on here (`lab/flatpak`, the ffmpeg 7.x
            // deadlock; see `SysSink`'s doc): renders must start and mic audio must
            // flow BEFORE ffmpeg will open the sys FIFO at all, so blocking this
            // thread on it, ahead of `run`, could never complete on 7.x. The budget
            // is the same [`FIFO_OPEN_BUDGET`], now stamped here; a rendezvous that
            // spends it trips `stop` from the writer and the session winds down
            // through the same wedge machinery as before.
            #[cfg(not(windows))]
            let sys_sink = SysSink::Pending {
                path: sys_path,
                deadline: Instant::now() + FIFO_OPEN_BUDGET,
                buffered: Vec::new(),
            };
            // Windows: named pipes have no open-order dependency (the client's
            // CreateFile connects regardless of ffmpeg's probe order), so the
            // control-thread connect stays byte-identical.
            #[cfg(windows)]
            let sys_sink = {
                let sys_fifo = crate::platform::windows::named_pipe::connect_write_end(
                    sys_pipe, FIFO_OPEN_BUDGET, thread_stop,
                );
                let Some(sys_fifo) = sys_fifo else {
                    log::warn!(
                        "{}",
                        rendezvous_failure("system", thread_stop.load(Ordering::Relaxed))
                    );
                    thread_stop.store(true, Ordering::Relaxed);
                    abandon_captures(mic_tap, mic_rx, monitor, sys_rx);
                    return PumpOut::empty();
                };
                SysSink::Open(Box::new(sys_fifo))
            };
            // The writer thread (module doc): owns the audio sinks and
            // every blocking write, spawned on the same scope (nested scoped
            // spawns are joined at scope exit like any other; see
            // `PumpHandle::join`'s doc for why its exit is guaranteed bounded).
            let (writer_tx, writer_rx) = std::sync::mpsc::channel::<PcmStep>();
            let writer_stop = thread_stop;
            let writer = std::thread::Builder::new().name("cck-pump-writer".to_string()).spawn_scoped(
                scope,
                move || writer_loop(writer_rx, MicSink::real(mic_fifo), sys_sink, writer_stop),
            );
            if let Err(e) = writer {
                log::warn!("media-clock pump: could not spawn its writer thread: {e}");
                thread_stop.store(true, Ordering::Relaxed);
                abandon_captures(mic_tap, mic_rx, monitor, sys_rx);
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

    // ---- The DRAGON-411 invariant: the horizon must clear the recovery trigger ----

    /// THE regression this ticket exists for. `RENDER_LAG_SECS` and
    /// `StreamAnchor::REANCHOR_THRESHOLD_SECS` were both 0.5 — the same number — so a
    /// source that fell behind could only ever re-anchor (recover) AFTER the render
    /// horizon had already discarded its audio. Recovery has to be able to happen
    /// FIRST, with room to spare; a future edit that lets these converge again
    /// reintroduces the field failure (16.3% of a customer's mic track written as
    /// bit-exact zeros, with the splice clicks heard as "static"). The same condition
    /// is a compile-time `assert!` next to the constants — this test is the one that
    /// explains itself when it breaks.
    // ── The rendezvous diagnosis (DRAGON-422) ──────────────────────────────

    #[test]
    fn a_rendezvous_cut_short_by_a_stop_does_not_blame_ffmpeg() {
        // The PipeWire zero-copy worker spawns its muxer on the first dmabuf frame, so
        // when no frame arrives there is no ffmpeg here at all — and this line was the
        // first symptom in the log of a session that failed for a completely different
        // reason. It sent a live investigation after a wedge that never existed.
        let m = rendezvous_failure("mic", true);
        assert!(!m.contains("ffmpeg"), "nothing here says ffmpeg was involved: {m}");
        assert!(m.contains("stopped"), "says the session was torn down: {m}");
        assert!(m.contains("mic"), "names the channel: {m}");
    }

    #[test]
    fn a_rendezvous_that_spends_its_budget_still_reports_the_wedge() {
        // The original diagnosis is still the right one when the session is live and
        // the budget simply ran out: that IS a muxer that never opened its inputs.
        let m = rendezvous_failure("system", false);
        assert!(m.contains("wedged ffmpeg"), "keeps the wedge diagnosis: {m}");
        assert!(
            m.contains(&FIFO_OPEN_BUDGET.as_secs().to_string()),
            "names the budget that was actually spent: {m}"
        );
        assert!(m.contains("system"), "names the channel: {m}");
    }

    #[test]
    fn the_join_grace_outlasts_the_teardown_it_waits_on() {
        // `abandon_captures` runs on the control thread, and each step carries its own
        // DRAGON-118 bound. A grace shorter than their sum reports "control thread never
        // finished" every time a session fails to start, while the thread is doing
        // exactly what it should — a false alarm in the one log a failed recording leaves.
        let teardown = Duration::from_secs(2) // mic child: term_then_wait
            + Duration::from_secs(2) // tap reader join
            + Duration::from_secs(2); // monitor capture join
        assert!(
            JOIN_GRACE >= teardown,
            "the join grace ({JOIN_GRACE:?}) must cover the abandon path's own bounded \
             teardown ({teardown:?})"
        );
    }

    #[test]
    fn render_horizon_clears_the_reanchor_threshold() {
        let threshold = crate::audio::capture::StreamAnchor::REANCHOR_THRESHOLD_SECS;
        assert!(
            RENDER_LAG_SECS > threshold,
            "the render horizon ({RENDER_LAG_SECS}s) must sit BEHIND the source re-anchor \
             threshold ({threshold}s), never at or in front of it: equal means a lagging \
             source's audio is discarded before it can ever recover (DRAGON-411)"
        );
        assert!(
            RENDER_LAG_SECS >= threshold * REANCHOR_HEADROOM,
            "the horizon ({RENDER_LAG_SECS}s) must clear the re-anchor threshold \
             ({threshold}s) by {REANCHOR_HEADROOM}x, so a source that trips the recovery \
             trigger still has {}s of further slippage before anything it delivers can be \
             dropped (DRAGON-411)",
            threshold * (REANCHOR_HEADROOM - 1.0)
        );
        // And the recovery must be reachable at all: a source is stamped in whole
        // chunks, so the threshold plus one generous chunk still has to fit.
        assert!(
            threshold + 0.1 < RENDER_LAG_SECS,
            "a chunk stamped right at the re-anchor threshold must still land safely \
             ahead of the horizon"
        );
    }

    #[test]
    fn audio_health_reads_loss_padding_and_a_clean_session_apart() {
        let clean = MixerStats::default();
        assert_eq!(audio_health(&clean, &clean), AudioHealth::Clean);
        // A trivial pad (under the notice threshold) is still clean.
        let small_gap = MixerStats { gap_samples: 4_800, ..MixerStats::default() }; // 0.1s
        assert_eq!(audio_health(&small_gap, &clean), AudioHealth::Clean);
        // Silence written into the recording, on EITHER track, is worth a warn.
        let big_gap = MixerStats { gap_samples: 48_000, ..MixerStats::default() }; // 1.0s
        assert_eq!(audio_health(&big_gap, &clean), AudioHealth::Padded);
        assert_eq!(audio_health(&clean, &big_gap), AudioHealth::Padded);
        // Chunks dropped because they PREDATE the recording are not loss, and every
        // session has some (the sources deliver before the clock is anchored, and
        // DRAGON-417's signed placement drops whatever has no post-0 tail). The chunk
        // count alone must therefore never escalate the report — only `late_frames`,
        // the in-recording audio, may.
        let pre_start =
            MixerStats { late_chunks: 3, pre_start_chunks: 3, ..MixerStats::default() };
        assert_eq!(audio_health(&pre_start, &clean), AudioHealth::Clean);
        // Same trap on the other counter: a source's LEAD-IN silence is present on
        // every healthy session (measured at 55-81ms on both tracks in every E2E
        // session), so it must never raise the level however large it is. Between them,
        // these two cases are why `late_chunks + gap_samples > 0` reported 7 of 7
        // healthy sessions as a loss.
        let lead_in = MixerStats { leading_gap_samples: 96_000, ..MixerStats::default() };
        assert_eq!(audio_health(&lead_in, &clean), AudioHealth::Clean, "2s of lead-in");
        // Any discarded audio outranks everything else: that is data loss.
        let lost = MixerStats { late_chunks: 1, late_frames: 480, ..MixerStats::default() };
        assert_eq!(audio_health(&lost, &clean), AudioHealth::Lost);
        assert_eq!(audio_health(&big_gap, &lost), AudioHealth::Lost);
    }

    #[test]
    fn frames_to_secs_reports_at_the_mixer_rate() {
        assert_eq!(frames_to_secs(48_000), 1.0);
        assert_eq!(frames_to_secs(0), 0.0);
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

    // ---- caught_up (the startup catch-up drain, DRAGON-417) -------------------

    #[test]
    fn caught_up_needs_both_a_quiet_pass_and_a_decided_latch() {
        // A backlog hands over many items at once — keep draining.
        assert!(!caught_up(&DrainOutcome { drained: 40, latch_decided: true }));
        // Level with the live rate AND nothing held back: start rendering.
        assert!(caught_up(&DrainOutcome { drained: 2, latch_decided: true }));
        assert!(caught_up(&DrainOutcome { drained: 0, latch_decided: true }));
        // System chunks still held for the latch must not be rendered past, however
        // quiet the pass looks.
        assert!(!caught_up(&DrainOutcome { drained: 0, latch_decided: false }));
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
    fn a_mid_session_mic_toggle_off_releases_the_duck() {
        // DRAGON-297 defect 1: the ducker keys on the mic's CURRENT toggle state, not only
        // its state at t=0. A mic armed at start (so the ducker exists and ducked on speech)
        // but toggled OFF mid-session must RELEASE — its now-muted speech is cut from the
        // file at finalize, so continuing to duck to it would carve holes in the system track.
        let t0 = base();
        let mut pump = ducked_pump_after_speech(t0, true); // armed; hold engaged by speech
        // First prove it IS ducking while the mic is live: a 0.3s system chunk (14_400
        // stereo frames) drops to the ducked gain once the 0.1s attack completes.
        pump.push_sys_chunk(vec![1.0; 28_800], t0, 0.0); // media 0..0.3
        // Now toggle the mic OFF and keep "speaking" into the muted channel: feed_sidechain
        // with live=false must never re-arm the hold, so it drains over the hold + release.
        pump.push_toggle(t0 + secs(0.6), AudioChannel::Mic, false);
        for i in 0..150 {
            let at = t0 + Duration::from_millis(600 + i * 10);
            pump.push_mic_tap(StreamTap::new(vec![0.5; 480], at, at));
        }
        // A later 1.0s system chunk (48_000 stereo frames) at media 3..4 must have fully
        // RELEASED back to unity by its end.
        pump.push_sys_chunk(vec![1.0; 96_000], t0 + secs(3.0), 0.0);
        let out = pump.mixer.render(4.0);
        let sys = &out.tracks[TRACK_SYS];
        // The live-mic chunk: frame 12_000 (media 0.25, well past the 0.1s attack) sits at
        // the ducked gain (~-12 dB).
        assert!(sys[24_000] < 0.5, "the live-mic chunk must duck (got {})", sys[24_000]);
        // The post-toggle chunk's final frame is back at unity.
        let last = sys[sys.len() - 1];
        assert!(
            (last - 1.0).abs() < 1e-6,
            "a mic toggled OFF mid-session must release the duck, not keep ducking on its \
             muted speech (got {last}, DRAGON-297)"
        );
    }

    #[test]
    fn gated_silence_mic_taps_never_duck_the_system_track() {
        // DRAGON-297 defect 3: the sidechain IS the cleaned, post-gate mic tap, so noise the
        // cleanup chain removes reaches the ducker as digital silence — a closed voice gate
        // can never duck the system track no matter how loud the raw room noise was.
        let t0 = base();
        let clock = Arc::new(Mutex::new(MediaClock::new(t0)));
        let (writer_tx, _writer_rx) = std::sync::mpsc::channel::<PcmStep>();
        let mut c = cfg(30);
        c.duck_system = true;
        c.mic_on0 = true;
        let mut pump = MediaClockPump::new(clock, &c, writer_tx);
        for i in 0..50 {
            let at = t0 + Duration::from_millis(i * 10);
            pump.push_mic_tap(StreamTap::new(vec![0.0; 480], at, at)); // gated silence
        }
        pump.push_sys_chunk(vec![1.0; 28_800], t0, 0.0);
        let out = pump.mixer.render(1.0);
        assert!(
            out.tracks[TRACK_SYS][..28_800].iter().all(|&s| s == 1.0),
            "gated-silence mic taps (cleaned-away noise) must never duck the system track"
        );
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
    fn pausing_takes_back_audio_already_placed_past_the_freeze() {
        // DRAGON-411: sources stamp CONTIGUOUSLY, so their audible times run ahead of
        // wall time and taps land at media positions the pause has not reached yet.
        // Those samples were captured after the user pressed pause — they must not be
        // in the file, and while they sat there the RESUME's own audio arrived for
        // ground already taken and was discarded as late (an `error`-level loss report
        // on every paused recording, which is how this was found).
        let t0 = base();
        let PumpRig { mut pump, .. } = pump_with_fakes(t0, 30);
        // One full second of mic audio placed at media 0.0..1.0, then a pause at 0.5.
        pump.push_mic_tap(StreamTap::new(vec![1.0; 48_000], t0, t0));
        pump.set_paused(true, t0 + secs(0.5));
        let out = pump.mixer.render(1.0);
        let mic = &out.tracks[TRACK_MIC];
        assert_eq!(mic.len(), 48_000);
        assert!(mic[..24_000].iter().all(|&s| s == 1.0), "audio before the freeze is kept");
        assert!(
            mic[24_000..].iter().all(|&s| s == 0.0),
            "audio captured past the freeze must not be recorded — pause means the media \
             clock stops, and leaving it also robs the resume of those positions"
        );
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
        // `SysSink::Open` is the Windows-and-rendezvoused shape; the pending-open
        // variant is exercised by ffmpeg itself in the media-clock E2E sessions.
        let stop = AtomicBool::new(false);
        writer_loop(
            writer_rx,
            MicSink::boxed(Box::new(mic_fifo.clone())),
            SysSink::Open(Box::new(sys_fifo.clone())),
            &stop,
        );
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

#[cfg(all(test, unix))]
mod pending_rendezvous_tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::mpsc::{channel, RecvTimeoutError};

    /// THE DRAGON-629 regression, with real FIFOs and no ffmpeg.
    ///
    /// Reproduces the live deadlock exactly: ffmpeg has our mic FIFO open but is not
    /// reading it, because it is sitting in `open()` on the SYS FIFO waiting for our
    /// write end, and our writer is in the pending-sys phase filling the mic pipe. Once
    /// that pipe is full, the OLD code parked in a blocking `write_all` and never
    /// returned to the sys-open retry the other side was waiting on. Neither could move
    /// and the recording died 12s later on the muxer watchdog.
    ///
    /// The observation point is the sys reader's own BLOCKING open: on POSIX it returns
    /// exactly when a writer opens the other end, so it IS the rendezvous, seen from
    /// ffmpeg's side. Nothing here drains the mic FIFO before that check, which is the
    /// whole point: draining it would let the parked write finish and would let the
    /// buggy code pass. Against the old code this times out; against the fix it returns
    /// in a couple of retry cycles.
    #[test]
    fn a_full_mic_pipe_cannot_stall_the_pending_system_rendezvous() {
        let dir = std::env::temp_dir();
        let tag = format!("cck-pump-629-{}", std::process::id());
        let mic_path = dir.join(format!("{tag}.mic"));
        let sys_path = dir.join(format!("{tag}.sys"));
        assert!(super::super::owned::mkfifo(&mic_path), "mkfifo mic");
        assert!(super::super::owned::mkfifo(&sys_path), "mkfifo sys");

        // A mic READER that never reads: the write end can open, and the pipe then fills
        // and stays full. This is ffmpeg holding the input open while blocked elsewhere.
        let mic_reader = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&mic_path)
            .expect("open the mic read end");

        let mic_w = try_open_fifo_write_end(&mic_path).expect("mic write end");
        let stop = AtomicBool::new(false);
        let (tx, rx) = channel::<PcmStep>();

        // 256KB of mono f32, comfortably past any FIFO buffer (64KB on Linux and macOS).
        let per_step = 4_800usize; // 100ms at 48kHz
        let steps = 14;
        for i in 0..steps {
            tx.send(PcmStep {
                mic: vec![i as f32; per_step],
                sys: vec![0.25; per_step * 2],
            })
            .expect("queue a step");
        }

        let (opened_tx, opened_rx) = channel::<()>();
        let (done_tx, done_rx) = channel::<()>();
        let sys_for_reader = sys_path.clone();

        std::thread::scope(|scope| {
            scope.spawn(|| {
                writer_loop(
                    rx,
                    MicSink::real(mic_w),
                    SysSink::Pending {
                        path: sys_path.clone(),
                        deadline: Instant::now() + Duration::from_secs(10),
                        buffered: Vec::new(),
                    },
                    &stop,
                );
                let _ = done_tx.send(());
            });

            // The "ffmpeg" side: after a beat, block in open() on the sys FIFO. This
            // returns only once the writer's retry opens the write end.
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                let mut sys_reader = std::fs::File::open(&sys_for_reader).expect("sys read end");
                let _ = opened_tx.send(());
                // Then behave like ffmpeg and actually consume it, so the writer's
                // post-rendezvous flush of `buffered` is not the thing that blocks.
                let mut buf = [0u8; 64 * 1024];
                let deadline = Instant::now() + Duration::from_secs(6);
                while Instant::now() < deadline {
                    match sys_reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            });

            let rendezvoused = opened_rx.recv_timeout(Duration::from_secs(5));

            // Unstick the writer whatever happened, so neither arm can hang the suite:
            // drain the mic FIFO to EOF on a helper, and hang the step channel up.
            let drain = std::thread::spawn(move || {
                let mut sink = Vec::new();
                let mut r = mic_reader;
                let mut buf = [0u8; 64 * 1024];
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    match r.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => sink.extend_from_slice(&buf[..n]),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
                sink
            });
            drop(tx);
            let finished = done_rx.recv_timeout(Duration::from_secs(8));
            let written = drain.join().expect("mic drain thread");

            assert!(
                rendezvoused.is_ok(),
                "the system FIFO rendezvous never completed while the mic pipe was full \
                 (DRAGON-629: the writer parked in a blocking mic write and stopped \
                 retrying the sys open)"
            );
            assert!(
                !matches!(finished, Err(RecvTimeoutError::Timeout)),
                "the writer never finished after the channel hung up"
            );
            assert!(
                !stop.load(Ordering::Relaxed),
                "a healthy rendezvous must not trip the session stop"
            );
            // Nothing may be dropped or reordered by the deferral: the mic stream's
            // sample count IS media time.
            assert_eq!(
                written.len(),
                steps * per_step * 4,
                "every deferred mic byte must reach the FIFO once the rendezvous lands"
            );
            let first = f32::from_le_bytes(written[0..4].try_into().expect("4 bytes"));
            let last_off = written.len() - 4;
            let last = f32::from_le_bytes(written[last_off..].try_into().expect("4 bytes"));
            assert_eq!(first, 0.0, "the backlog must flush in order, oldest first");
            assert_eq!(last, (steps - 1) as f32, "and end on the newest step");
        });

        let _ = std::fs::remove_file(&mic_path);
        let _ = std::fs::remove_file(&sys_path);
    }

    // ── The session's audio observability lines ────────────────────────────

    /// The dBFS mapping the levels line is read through. Wrong here and every "was it
    /// attenuated" reading off the log is wrong with it.
    #[test]
    fn dbfs_rms_maps_full_scale_half_scale_and_silence() {
        // A constant-magnitude signal's RMS is that magnitude: full scale is 0 dBFS.
        assert!((dbfs_rms(4.0, 4) - 0.0).abs() < 1e-9, "full scale is 0 dBFS");
        // Half amplitude is one bit down, ≈ −6.02 dB.
        assert!((dbfs_rms(0.25 * 4.0, 4) + 6.0206).abs() < 1e-3);
        // Digital silence and an empty track both report the printable floor, never −inf.
        assert_eq!(dbfs_rms(0.0, 480), SILENT_DBFS);
        assert_eq!(dbfs_rms(0.0, 0), SILENT_DBFS);
        assert!(dbfs_rms(1e-30, 480) >= SILENT_DBFS, "the floor also catches near-zero energy");
    }

    #[test]
    fn loudness_accumulates_to_the_same_answer_as_one_block() {
        let mut split = Loudness::default();
        split.add(&[0.5; 240]);
        split.add(&[0.5; 240]);
        let mut whole = Loudness::default();
        whole.add(&[0.5; 480]);
        assert!((split.dbfs() - whole.dbfs()).abs() < 1e-9);
        assert_eq!(split.samples, 480);
    }

    /// The combination the config line exists for: ducking enabled with the mic NOT
    /// recorded. The ducker is inactive then (its sidechain is gated on the mic being live),
    /// and the note has to say so on its own so the reader does not mistake it for a bug.
    #[test]
    fn the_config_line_calls_out_ducking_without_a_recorded_mic() {
        let line = audio_config_line(false, true, true);
        assert!(line.contains("mic=off"), "{line}");
        assert!(line.contains("system=on"), "{line}");
        assert!(line.contains("ducking=on"), "{line}");
        assert!(line.contains("NOTE:"), "the odd combination must be named outright: {line}");
        assert!(line.contains("INACTIVE"), "the note must say the ducker does NOT fire: {line}");
        // Every ordinary combination stays a plain statement of fact, with no note to
        // scroll past.
        assert!(!audio_config_line(true, true, true).contains("NOTE:"));
        assert!(!audio_config_line(false, true, false).contains("NOTE:"));
    }

    /// The levels line has to answer "did WE pull the system track down" at a glance,
    /// which is the whole reason it exists.
    #[test]
    fn the_levels_line_separates_our_attenuation_from_a_quiet_source() {
        let mut mic = Loudness::default();
        mic.add(&[0.1; 480]);
        let mut sys_in = Loudness::default();
        sys_in.add(&[0.4; 480]);
        // Ducked to a quarter: −12 dB, exactly the ducker's configured gain.
        let mut sys_out = Loudness::default();
        sys_out.add(&[0.1; 480]);
        let stats = DuckStats {
            sys_frames: 240,
            ducked_frames: 200,
            min_gain: 0.25,
            sidechain_active: 30,
        };
        let ducked = audio_levels_line(&mic, &sys_in, &sys_out, Some(stats));
        assert!(ducked.contains("we attenuated the system track by 12.0dB"), "{ducked}");
        assert!(ducked.contains("ducked_frames=200"), "{ducked}");
        assert!(ducked.contains("min_gain=0.250"), "{ducked}");
        assert!(ducked.contains("sidechain_active=30"), "{ducked}");

        // Same quiet source, nothing applied: the two ends match, so the level is the
        // source's own.
        let mut quiet = Loudness::default();
        quiet.add(&[0.001; 480]);
        let untouched = audio_levels_line(&mic, &quiet, &quiet, None);
        assert!(untouched.contains("did not attenuate"), "{untouched}");
        assert!(untouched.contains("duck(off)"), "{untouched}");
        assert!(untouched.contains("sys_in=-60.0dBFS"), "{untouched}");
        assert!(untouched.contains("sys_out=-60.0dBFS"), "{untouched}");
    }
}
