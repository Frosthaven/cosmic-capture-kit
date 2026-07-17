//! The mixer core (DRAGON-122 phase 3, chunk A): a pure, deterministic composition of
//! [`clock::MediaClock`] (wall→media time), [`track::Track`] (per-track sample
//! placement), and [`control::ControlLane`] (timestamped pause/resume/gain commands)
//! behind one `render` call. This is a RECORDER's mixer, not a live-monitoring one:
//! deep buffering is fine and low latency is a NON-goal (see each submodule's doc for
//! what that buys — `Track`'s deque has no cap, gain changes ramp over a full 10ms
//! frame, nothing here ever reads the wall clock itself).
//!
//! Live since DRAGON-125 chunk B1: [`crate::record::pump`] drives a `Final`-mode mixer
//! for the PipeWire media-clock owned recording path (chunk A's module-wide
//! `#![allow(dead_code)]` — "nothing is live yet" — went with it). The one still
//! forward-looking piece is `Live` mode's in-render gain application ([`MixMode::Live`]
//! plus [`control::GainRamp`], for the epic's later live-MONITORING consumer;
//! recording uses `Final`, which reports automation instead of applying it) — that
//! keeps a targeted per-item allow, same as `audio::filters::StreamTap` did while it
//! was ahead of its consumer.

pub(crate) mod clock;
pub(crate) mod control;
pub(crate) mod track;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::audio::filters::StreamTap;

use clock::MediaClock;
use control::{AppliedEvent, ControlEvent, ControlKind, ControlLane};
use track::{MixerStats, Track};

/// Fixed mixing rate (DRAGON-122's design: 48kHz throughout, no per-track override).
pub(crate) const SAMPLE_RATE: u32 = 48_000;

/// One track's construction-time shape: its channel count (1 = mono, 2 = stereo) and
/// starting gain before any `TrackGain` event retargets it.
pub(crate) struct TrackSpec {
    pub(crate) channels: u8,
    pub(crate) initial_gain: f32,
}

/// Whether `Mixer::render` applies `TrackGain` automation into the samples it returns
/// (`Live`) or leaves them raw and only reports the automation for a later pass to apply
/// (`Final`) — the DRAGON-122 live/final invariant. `Pause` behaves identically either
/// way: the clock stops, so a paused wall stretch simply contributes no media time to
/// render in either mode — there is no separate "silence for the pause" case to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MixMode {
    /// Forward-looking (see the module doc): no production consumer constructs
    /// `Live` yet — recording (`record::pump`) is `Final`-mode — but `render`'s
    /// `Live` arm and its `GainRamp` machinery are the epic's planned
    /// live-monitoring path, exercised by this module's tests meanwhile.
    #[allow(dead_code)]
    Live,
    Final,
}

/// One `render` call's output: every track's samples for this step, in LOCKSTEP frame
/// count (`len() / channels` equal across tracks, even when their pushed data doesn't
/// reach equally far — the shorter ones are silence-padded; see `Track::drain_to`), plus
/// the control events this step consumed, resolved to media-time positions.
pub(crate) struct RenderOut {
    pub(crate) tracks: Vec<Vec<f32>>,
    pub(crate) automation: Vec<AppliedEvent>,
}

/// The mixer: owns (or shares — see [`Mixer::with_clock`]) the [`MediaClock`], every
/// [`Track`], the [`ControlLane`], and (used by `Live` mode only) each track's
/// [`control::GainRamp`]. `render` is the only thing that advances media time —
/// nothing here ever calls `Instant::now()`.
pub(crate) struct Mixer {
    mode: MixMode,
    /// Shared with the caller when built via [`with_clock`](Self::with_clock)
    /// (DRAGON-125: `record::pump` hands the SAME clock to its `VideoTicker`, making
    /// audio-vs-video timeline divergence structurally impossible — there is no second
    /// clock instance to disagree). `new` builds a private one, preserving chunk A's
    /// self-contained construction for tests and any future standalone consumer.
    clock: Arc<Mutex<MediaClock>>,
    tracks: Vec<Track>,
    gains: Vec<control::GainRamp>,
    control: ControlLane,
    /// Media position already rendered up to — the start of the NEXT `render` call.
    horizon: f64,
}

impl Mixer {
    /// `start` seeds a private [`MediaClock`]. (The design this chunk implements
    /// describes `Mixer::new` as `(mode, tracks)` alone, but a clock cannot exist
    /// without an anchor instant, and nothing in this module may call `Instant::now()`
    /// — see the crate's DRAGON-125 notes for this gap and why construction takes it
    /// explicitly instead.) The production recorder builds via
    /// [`with_clock`](Self::with_clock) since the one-clock fix; this stays as chunk
    /// A's self-contained constructor (tests, future standalone consumers) — same
    /// targeted-allow idiom as [`MixMode::Live`].
    #[allow(dead_code)]
    pub(crate) fn new(mode: MixMode, start: Instant, tracks: &[TrackSpec]) -> Self {
        Self::with_clock(mode, Arc::new(Mutex::new(MediaClock::new(start))), tracks)
    }

    /// Build against a CALLER-OWNED clock (DRAGON-125 chunk B1 fix): pause/resume the
    /// caller applies to `clock` are visible here the instant they land — chunk
    /// placement (`Track::push`'s paused-discard and media mapping) reads the same
    /// authoritative history the caller's other consumers (the video ticker) read,
    /// rather than a lag-consumed private copy. `ControlLane` events still resolve
    /// their media positions through this same clock at consume time; a `Pause`/
    /// `Resume` the caller already applied re-applies as a no-op (`MediaClock`'s
    /// idempotence), yielding identical `AppliedEvent` values either way.
    pub(crate) fn with_clock(
        mode: MixMode,
        clock: Arc<Mutex<MediaClock>>,
        tracks: &[TrackSpec],
    ) -> Self {
        Self {
            mode,
            clock,
            tracks: tracks.iter().map(|t| Track::new(t.channels)).collect(),
            gains: tracks.iter().map(|t| control::GainRamp::new(t.initial_gain)).collect(),
            control: ControlLane::new(),
            horizon: 0.0,
        }
    }

    /// Place a captured chunk on `track`'s timeline (see `Track::push`). `track` is a
    /// caller-controlled index fixed by construction, not runtime data — out of range is
    /// a bug worth panicking on immediately, not swallowing (same reasoning covers
    /// `stats` and a `TrackGain` event's `track` field in `render`, below).
    /// Lock idiom (here and in `render`): a poisoned clock mutex still yields the
    /// clock (`MediaClock` keeps no invariant a panicking peer could break mid-update
    /// — its anchor pushes are single appends), so mixing never silently stops on an
    /// unrelated thread's panic.
    pub(crate) fn push_tap(&mut self, track: usize, tap: StreamTap) {
        let clock = self.clock.lock().unwrap_or_else(|e| e.into_inner());
        self.tracks[track].push(tap, &clock);
    }

    /// Queue a command (see `ControlLane::push`) — applied only once `render`'s horizon
    /// reaches its media position, never on arrival.
    pub(crate) fn push_event(&mut self, ev: ControlEvent) {
        self.control.push(ev);
    }

    pub(crate) fn stats(&self, track: usize) -> MixerStats {
        self.tracks[track].stats()
    }

    /// Advance the render horizon to `until_media`, applying every control event whose
    /// media position falls before it. Chunk A trusts the caller's `until_media` (see
    /// the module doc; chunk B is what will clamp it to `now - lag`), but still handles
    /// it landing beyond what any given track has actually received: every track's
    /// output is exactly `(until_media - previous horizon) * SAMPLE_RATE` frames
    /// (rounded once per call, not accumulated across calls — see `Track::drain_to`),
    /// silence-padded past whatever data has arrived, so every track comes back at the
    /// same frame count regardless of which ones are behind.
    pub(crate) fn render(&mut self, until_media: f64) -> RenderOut {
        if until_media <= self.horizon {
            return RenderOut {
                tracks: self.tracks.iter().map(|_| Vec::new()).collect(),
                automation: Vec::new(),
            };
        }
        let applied = {
            let mut clock = self.clock.lock().unwrap_or_else(|e| e.into_inner());
            self.control.consume_through(&mut clock, until_media)
        };

        let from_frame = (self.horizon * SAMPLE_RATE as f64).round() as u64;
        let until_frame = (until_media * SAMPLE_RATE as f64).round() as u64;

        // Per-track TrackGain events this step, as (sample position, target gain),
        // clamped forward into this step's span — Live mode ramps these into the
        // samples below; Final mode ignores this and reports `applied` untouched.
        let mut gain_events: Vec<Vec<(u64, f32)>> = vec![Vec::new(); self.tracks.len()];
        for ev in &applied {
            if let ControlKind::TrackGain { track, gain } = ev.kind {
                let pos = ((ev.media * SAMPLE_RATE as f64).round() as u64).max(from_frame);
                gain_events[track].push((pos, gain));
            }
        }

        let mut tracks = Vec::with_capacity(self.tracks.len());
        for (i, track) in self.tracks.iter_mut().enumerate() {
            let raw = track.drain_to(until_frame);
            let samples = match self.mode {
                MixMode::Final => raw,
                MixMode::Live => {
                    self.gains[i].apply(&raw, track.channels(), from_frame, &gain_events[i])
                }
            };
            tracks.push(samples);
        }
        self.horizon = until_media;
        RenderOut { tracks, automation: applied }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use control::GainRamp;
    use std::time::Duration;

    fn base() -> Instant {
        Instant::now()
    }
    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    fn mono_spec(gain: f32) -> TrackSpec {
        TrackSpec { channels: 1, initial_gain: gain }
    }

    #[test]
    fn render_before_the_current_horizon_is_a_no_op() {
        let t0 = base();
        let mut mixer = Mixer::new(MixMode::Final, t0, &[mono_spec(1.0)]);
        let out = mixer.render(0.0); // horizon starts at 0.0 already
        assert!(out.tracks[0].is_empty());
        assert!(out.automation.is_empty());
    }

    #[test]
    fn render_advances_by_media_time_not_wall_time_across_a_pause() {
        let t0 = base();
        let mut mixer = Mixer::new(MixMode::Final, t0, &[mono_spec(1.0)]);
        mixer.push_event(ControlEvent { at: t0 + secs(1.0), kind: ControlKind::Pause });
        mixer.push_event(ControlEvent { at: t0 + secs(3.0), kind: ControlKind::Resume });
        // 3.5s of wall time have "happened" (1 real + 2 paused + 0.5 real) but only
        // 1.5s of MEDIA time -- render to exactly that media horizon.
        let out = mixer.render(1.5);
        assert_eq!(out.tracks[0].len(), 72_000); // 1.5s * 48kHz
        assert_eq!(out.automation.len(), 2);
        assert_eq!(out.automation[0].media, 1.0);
        assert_eq!(out.automation[1].media, 1.0);
    }

    fn build_and_render(mode: MixMode, t0: Instant) -> (RenderOut, Mixer) {
        let mut mixer = Mixer::new(mode, t0, &[mono_spec(1.0)]);
        mixer.push_tap(0, StreamTap::new(vec![1.0; 2000], t0, t0));
        mixer.push_event(ControlEvent {
            at: t0 + secs(0.01), // resolves to frame 480 exactly
            kind: ControlKind::TrackGain { track: 0, gain: 0.0 },
        });
        let out = mixer.render(2000.0 / SAMPLE_RATE as f64);
        (out, mixer)
    }

    #[test]
    fn live_and_final_report_identical_automation_and_stats() {
        let t0 = base();
        let (live_out, live_mixer) = build_and_render(MixMode::Live, t0);
        let (final_out, final_mixer) = build_and_render(MixMode::Final, t0);
        assert_eq!(live_out.automation, final_out.automation);
        assert_eq!(live_mixer.stats(0), final_mixer.stats(0));
    }

    #[test]
    fn live_samples_differ_from_final_exactly_by_the_gain_automation() {
        let t0 = base();
        let (live_out, _) = build_and_render(MixMode::Live, t0);
        let (final_out, _) = build_and_render(MixMode::Final, t0);
        // Final is raw (untouched by the gain event); re-deriving the same envelope
        // independently, from Final's raw samples, must reproduce Live's exactly.
        let mut ramp = GainRamp::new(1.0);
        let pos = (0.01 * SAMPLE_RATE as f64).round() as u64;
        let expected = ramp.apply(&final_out.tracks[0], 1, 0, &[(pos, 0.0)]);
        assert_eq!(live_out.tracks[0], expected);
        assert_ne!(
            live_out.tracks[0], final_out.tracks[0],
            "the gain event must have changed something"
        );
    }

    #[test]
    fn initial_gain_applies_in_live_mode_without_any_event() {
        let t0 = base();
        let mut mixer = Mixer::new(MixMode::Live, t0, &[mono_spec(0.5)]);
        mixer.push_tap(0, StreamTap::new(vec![2.0; 100], t0, t0));
        let out = mixer.render(100.0 / SAMPLE_RATE as f64);
        assert!(out.tracks[0].iter().all(|&s| s == 1.0)); // 2.0 * 0.5
    }

    #[test]
    fn render_pads_a_shorter_track_to_the_same_frame_count_and_counts_the_gap() {
        let t0 = base();
        let mut mixer = Mixer::new(
            MixMode::Final,
            t0,
            &[TrackSpec { channels: 1, initial_gain: 1.0 }, TrackSpec {
                channels: 2,
                initial_gain: 1.0,
            }],
        );
        mixer.push_tap(0, StreamTap::new(vec![1.0; 100], t0, t0)); // 100 mono frames
        mixer.push_tap(1, StreamTap::new(vec![2.0; 80], t0, t0)); // 40 stereo frames
        let out = mixer.render(100.0 / SAMPLE_RATE as f64);
        assert_eq!(out.tracks[0].len(), 100); // 100 frames * 1ch
        assert_eq!(out.tracks[1].len(), 200); // 100 frames * 2ch (60 padded)
        assert!(out.tracks[1][80..].iter().all(|&s| s == 0.0));
        assert_eq!(mixer.stats(1).gap_samples, 60);
        assert_eq!(mixer.stats(0).gap_samples, 0);
    }

    // The gate-ramp equality style (filters/gate.rs), end to end through push/render:
    // gain 1 -> 0 at media t; the sample just before is untouched, the 480th sample of
    // the ramp is exactly the target, and it's monotonic in between.
    #[test]
    fn live_render_ramps_gain_exactly_across_the_480_sample_window() {
        let t0 = base();
        let mut mixer = Mixer::new(MixMode::Live, t0, &[mono_spec(1.0)]);
        mixer.push_tap(0, StreamTap::new(vec![1.0; 60_000], t0, t0));
        mixer.push_event(ControlEvent {
            at: t0 + secs(1.0), // frame 48_000
            kind: ControlKind::TrackGain { track: 0, gain: 0.0 },
        });
        let out = mixer.render(1.1); // well past the ramp window, still within pushed data
        let s = &out.tracks[0];
        assert_eq!(s[47_999], 1.0, "before the event: unscaled");
        assert_eq!(s[48_479], 0.0, "480 samples in: fully scaled");
        assert_eq!(s[50_000], 0.0, "after the ramp: holds the target");
        let ramp_window = &s[48_000..48_480];
        for w in ramp_window.windows(2) {
            assert!(w[1] <= w[0], "ramp must not rise: {ramp_window:?}");
        }
    }

    #[test]
    fn behavior_depends_only_on_relative_offsets_not_the_absolute_instant() {
        let (out_a, _) = build_and_render(MixMode::Live, Instant::now());
        let (out_b, _) = build_and_render(MixMode::Live, Instant::now());
        assert_eq!(out_a.tracks, out_b.tracks);
        assert_eq!(out_a.automation.len(), out_b.automation.len());
        assert_eq!(out_a.automation[0].media, out_b.automation[0].media);
    }
}
