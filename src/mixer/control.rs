//! `ControlLane` + `GainRamp`: timestamped commands (pause/resume/per-track gain) queued
//! as they arrive and consumed only once the render horizon actually crosses their media
//! position (DRAGON-122 phase 3, chunk A) — never on arrival, since arrival order across
//! threads/channels is unguaranteed but the mixer's output must apply commands in MEDIA
//! order. `Pause`/`Resume` feed the [`MediaClock`] directly as they're consumed (there is
//! no other consumer for them); `TrackGain` is reported back to [`super::Mixer`] to apply
//! to the relevant track's [`GainRamp`] — which is what turns the click-free-ramp
//! requirement into per-sample gain values (`Live` mode only; see `Mixer::render`'s mode
//! doc for why `Final` skips this).

use std::time::Instant;

use super::clock::MediaClock;

/// One queued command, stamped at the wall instant it actually happened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ControlEvent {
    pub(crate) at: Instant,
    pub(crate) kind: ControlKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ControlKind {
    Pause,
    Resume,
    TrackGain { track: usize, gain: f32 },
}

/// One consumed event, resolved to its media-time position — what `Mixer::render`
/// reports in `RenderOut::automation` (the future finalize pass's consumption point for
/// `Final`-mode automation).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AppliedEvent {
    pub(crate) media: f64,
    pub(crate) kind: ControlKind,
}

/// The pending-event queue. Push order carries no meaning; `consume_through` always
/// applies in wall-clock (equivalently media, since `MediaClock`'s mapping is monotonic)
/// order.
pub(crate) struct ControlLane {
    pending: Vec<ControlEvent>,
}

impl ControlLane {
    pub(crate) fn new() -> Self {
        Self { pending: Vec::new() }
    }

    pub(crate) fn push(&mut self, ev: ControlEvent) {
        self.pending.push(ev);
    }

    /// Consume every pending event whose media position (mapped through `clock`, which
    /// `Pause`/`Resume` mutate as each is applied) is strictly before `until_media`, in
    /// wall-clock order, and return them as media-time-stamped [`AppliedEvent`]s.
    /// Events at or past `until_media` are left pending for a future call.
    pub(crate) fn consume_through(
        &mut self,
        clock: &mut MediaClock,
        until_media: f64,
    ) -> Vec<AppliedEvent> {
        self.pending.sort_by_key(|ev| ev.at);
        let mut applied = Vec::new();
        let mut consumed = 0;
        for ev in &self.pending {
            let media = clock.media_at(ev.at);
            if media >= until_media {
                break;
            }
            match ev.kind {
                ControlKind::Pause => clock.pause(ev.at),
                ControlKind::Resume => clock.resume(ev.at),
                ControlKind::TrackGain { .. } => {} // reported only; Mixer applies it
            }
            applied.push(AppliedEvent { media, kind: ev.kind });
            consumed += 1;
        }
        self.pending.drain(0..consumed);
        applied
    }
}

/// The click-free ramp window: 480 samples (10ms at 48kHz) — the same frame length as
/// `audio::filters::gate::Gate`'s ramp. Unlike the gate's ramp (which divides by the
/// frame length and so only ASYMPTOTICALLY approaches its target within one call,
/// reaching it exactly only once a following frame starts from an already-equal gain), a
/// `TrackGain` event must land on an EXACT sample position — `ControlLane`'s contract —
/// so this divides by `RAMP_FRAMES - 1` instead: the ramp's last sample is bit-exact at
/// the target, not merely close.
const RAMP_FRAMES: u64 = 480;

/// An in-flight linear gain ramp: `from` at `start`, reaching `to` exactly at
/// `start + RAMP_FRAMES - 1`.
struct ActiveRamp {
    start: u64,
    from: f32,
    to: f32,
}

/// Per-track gain state: a settled `current` value plus (optionally) one ramp in
/// flight. A new target always ramps from whatever gain is ACTUALLY in effect at its
/// start position — even mid-ramp — so back-to-back events stay click-free.
pub(crate) struct GainRamp {
    current: f32,
    active: Option<ActiveRamp>,
}

impl GainRamp {
    pub(crate) fn new(initial: f32) -> Self {
        Self { current: initial, active: None }
    }

    /// The gain in effect at absolute sample position `pos` (pure query).
    pub(crate) fn gain_at(&self, pos: u64) -> f32 {
        match &self.active {
            Some(r) if pos < r.start => r.from,
            Some(r) if pos >= r.start + RAMP_FRAMES - 1 => r.to,
            Some(r) => {
                let j = (pos - r.start) as f32;
                r.from + (r.to - r.from) * (j / (RAMP_FRAMES - 1) as f32)
            }
            None => self.current,
        }
    }

    /// Install a new ramp target starting at `pos`, from whatever `gain_at(pos)`
    /// resolves to right now (superseding any ramp already in flight).
    pub(crate) fn set_target(&mut self, pos: u64, target: f32) {
        let from = self.gain_at(pos);
        self.current = target;
        self.active = Some(ActiveRamp { start: pos, from, to: target });
    }

    /// Multiply `raw` (interleaved, `channels` per frame, starting at absolute frame
    /// `from_frame`) by the gain envelope, installing each of `events`
    /// (`(start_pos, target)`, ascending by `start_pos`, all `>= from_frame`) exactly
    /// when the scan reaches it. This interleaving (rather than installing every event
    /// up front) is what makes MULTIPLE events landing in one call apply correctly:
    /// each captures its own `from` at the moment it starts, not retroactively.
    pub(crate) fn apply(
        &mut self,
        raw: &[f32],
        channels: u8,
        from_frame: u64,
        events: &[(u64, f32)],
    ) -> Vec<f32> {
        let ch = channels as usize;
        let mut out = Vec::with_capacity(raw.len());
        let mut next = 0;
        for (i, frame) in raw.chunks(ch).enumerate() {
            let pos = from_frame + i as u64;
            while next < events.len() && events[next].0 == pos {
                self.set_target(pos, events[next].1);
                next += 1;
            }
            let g = self.gain_at(pos);
            out.extend(frame.iter().map(|&s| s * g));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn base() -> Instant {
        Instant::now()
    }
    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    // --- GainRamp: the click-free ramp shape ---

    #[test]
    fn ramp_boundary_samples_are_bit_exact_unscaled_then_fully_scaled() {
        // The gate-ramp equality style (filters/gate.rs): gain 1 -> 0 starting at
        // sample P; P-1 is untouched (still 1.0), P+479 is exactly the target (0.0).
        let mut ramp = GainRamp::new(1.0);
        let p = 10_000u64;
        ramp.set_target(p, 0.0);
        assert_eq!(ramp.gain_at(p - 1), 1.0, "before the event: unscaled");
        assert_eq!(ramp.gain_at(p + 479), 0.0, "480 samples in: fully scaled");
        assert_eq!(ramp.gain_at(p + 480), 0.0, "after the ramp: holds the target");
    }

    #[test]
    fn ramp_is_monotonic_between_its_endpoints() {
        let mut ramp = GainRamp::new(1.0);
        ramp.set_target(0, 0.0);
        let vals: Vec<f32> = (0..480).map(|j| ramp.gain_at(j)).collect();
        for w in vals.windows(2) {
            assert!(w[1] <= w[0], "ramp must not rise: {vals:?}");
        }
        assert_eq!(vals[0], 1.0);
        assert_eq!(*vals.last().unwrap(), 0.0);
    }

    #[test]
    fn back_to_back_events_ramp_from_the_actual_current_value_mid_ramp() {
        // A second event landing before the first ramp finishes must start from
        // wherever the first ramp actually is at that instant, not from the first
        // ramp's (never-reached) target.
        let mut ramp = GainRamp::new(1.0);
        ramp.set_target(0, 0.0);
        let mid = ramp.gain_at(100); // partway through the first ramp
        ramp.set_target(100, 1.0);
        assert_eq!(ramp.gain_at(100), mid, "the new ramp starts exactly where the old one was");
        assert_eq!(ramp.gain_at(100 + 479), 1.0);
    }

    // --- ControlLane: consume ordering / media resolution ---

    #[test]
    fn events_pushed_out_of_order_are_consumed_in_media_order() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        let mut lane = ControlLane::new();
        // Push the LATER event first.
        lane.push(ControlEvent {
            at: t0 + secs(2.0),
            kind: ControlKind::TrackGain { track: 0, gain: 0.5 },
        });
        lane.push(ControlEvent {
            at: t0 + secs(1.0),
            kind: ControlKind::TrackGain { track: 0, gain: 0.2 },
        });
        let applied = lane.consume_through(&mut clock, 10.0);
        assert_eq!(applied.len(), 2);
        assert_eq!(applied[0].media, 1.0);
        assert_eq!(applied[1].media, 2.0);
    }

    #[test]
    fn consume_through_stops_before_the_horizon_and_leaves_the_rest_pending() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        let mut lane = ControlLane::new();
        lane.push(ControlEvent {
            at: t0 + secs(1.0),
            kind: ControlKind::TrackGain { track: 0, gain: 0.5 },
        });
        lane.push(ControlEvent {
            at: t0 + secs(5.0),
            kind: ControlKind::TrackGain { track: 0, gain: 0.8 },
        });
        let applied = lane.consume_through(&mut clock, 2.0);
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].media, 1.0);
        // The second event (media 5.0) is still pending -- consuming further reveals it.
        let more = lane.consume_through(&mut clock, 100.0);
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].media, 5.0);
    }

    // Mirrors `toggle_during_pause_mutes_from_the_seam`: an event stamped DURING an
    // already-open pause resolves to the pause's frozen seam position, not a naive
    // wall-time mapping.
    #[test]
    fn event_during_a_pause_resolves_to_the_seam() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.pause(t0 + secs(10.0)); // frozen at media 10.0
        let mut lane = ControlLane::new();
        lane.push(ControlEvent {
            at: t0 + secs(17.0), // inside the pause
            kind: ControlKind::TrackGain { track: 0, gain: 0.0 },
        });
        let applied = lane.consume_through(&mut clock, 100.0);
        assert_eq!(applied[0].media, 10.0);
    }

    // Mirrors `pause_spanning_mute_produces_one_interval_across_the_seam`: an event
    // before the pause and one after resume land on either side of the collapsed gap,
    // with no contribution from the pause itself.
    #[test]
    fn events_before_and_after_a_pause_skip_the_gap_entirely() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        let mut lane = ControlLane::new();
        lane.push(ControlEvent {
            at: t0 + secs(8.0),
            kind: ControlKind::TrackGain { track: 0, gain: 0.0 },
        });
        lane.push(ControlEvent {
            at: t0 + secs(27.0), // 2s into the post-resume run
            kind: ControlKind::TrackGain { track: 0, gain: 1.0 },
        });
        let applied = lane.consume_through(&mut clock, 1.0); // before anything: nothing yet
        assert!(applied.is_empty());
        clock.pause(t0 + secs(10.0));
        clock.resume(t0 + secs(25.0));
        let applied = lane.consume_through(&mut clock, 100.0);
        assert_eq!(applied[0].media, 8.0);
        assert_eq!(applied[1].media, 12.0); // 10 (seam) + 2
    }

    #[test]
    fn pause_and_resume_control_events_feed_the_clock_as_they_are_consumed() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        let mut lane = ControlLane::new();
        lane.push(ControlEvent { at: t0 + secs(10.0), kind: ControlKind::Pause });
        lane.push(ControlEvent { at: t0 + secs(25.0), kind: ControlKind::Resume });
        lane.push(ControlEvent {
            at: t0 + secs(17.0), // during the pause
            kind: ControlKind::TrackGain { track: 0, gain: 0.0 },
        });
        let applied = lane.consume_through(&mut clock, 1_000.0);
        assert_eq!(applied.len(), 3);
        assert!(matches!(applied[0].kind, ControlKind::Pause));
        assert_eq!(applied[0].media, 10.0);
        assert!(matches!(applied[1].kind, ControlKind::TrackGain { .. }));
        assert_eq!(applied[1].media, 10.0, "resolved to the seam, same as the pause itself");
        assert!(matches!(applied[2].kind, ControlKind::Resume));
        assert_eq!(applied[2].media, 10.0);
        assert!(!clock.is_paused_at(t0 + secs(30.0)));
    }
}
