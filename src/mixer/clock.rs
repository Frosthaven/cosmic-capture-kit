//! `MediaClock`: the wall-clock → media-time mapping a recording session's pause/resume
//! cycle drives (DRAGON-122 phase 3, chunk A). Media time is the timeline every track's
//! samples get placed against — it advances 1:1 with wall time while running, and freezes
//! during a pause (a paused stretch contributes ZERO media seconds, by construction: it
//! wasn't recorded). This is the sample-accurate generalization of the legacy
//! (DRAGON-111, retired DRAGON-127) `record::finalize::session_media_time` — the
//! assembled-recording wall→media mapping, built from precomputed segment spans known
//! only after the fact — to a clock built INCREMENTALLY, live, from `pause`/`resume`
//! calls as they happen; the boundary semantics (a wall instant inside a pause clamps
//! to the pause's frozen media position) are the same idea, numerically verified
//! against that function's cases when it still existed.

use std::time::Instant;

/// One boundary in the clock's history: from `wall` onward (until superseded by the next
/// anchor, or indefinitely if this is the last one), the clock is `running` (media
/// advances 1:1 with wall time starting from `media`) or paused (frozen at `media`).
struct Anchor {
    wall: Instant,
    media: f64,
    running: bool,
}

/// The wall→media mapping for one recording session. Pure and deterministic: every
/// method takes its `Instant` explicitly and none reads the system clock, so behavior is
/// fully reproducible from a fixed base `Instant` plus `Duration` offsets (tests do
/// exactly this; a live caller feeds it real `Instant::now()` values from outside).
pub(crate) struct MediaClock {
    /// Non-decreasing by `wall`; seeded by `new` and never emptied.
    anchors: Vec<Anchor>,
}

impl MediaClock {
    /// A fresh clock: media 0.0 at `start`, running.
    pub(crate) fn new(start: Instant) -> Self {
        Self { anchors: vec![Anchor { wall: start, media: 0.0, running: true }] }
    }

    fn current(&self) -> &Anchor {
        self.anchors.last().expect("seeded by `new`, never emptied")
    }

    /// Pause at `at`. A no-op if already paused (idempotent, mirroring the app's own
    /// pause/resume toggle, where pausing an already-paused recording changes nothing).
    pub(crate) fn pause(&mut self, at: Instant) {
        if !self.current().running {
            return;
        }
        // Freeze at whatever media_at(at) resolves to under the CURRENT (still-running)
        // history — this is what makes a later query at/after `at` read the same value.
        let media = self.media_at(at);
        self.anchors.push(Anchor { wall: at, media, running: false });
    }

    /// Resume at `at`. A no-op if already running (idempotent).
    pub(crate) fn resume(&mut self, at: Instant) {
        if self.current().running {
            return;
        }
        let media = self.current().media; // the frozen value carries across the pause
        self.anchors.push(Anchor { wall: at, media, running: true });
    }

    /// The anchor governing `wall`: the LAST one at or before it, clamped to the first
    /// anchor for a `wall` before the clock even started. Half-open convention: the
    /// exact instant of a `resume` already reads as running (it IS that anchor); the
    /// exact instant of a `pause` already reads as paused, for the same reason — each
    /// anchor governs from its own `wall` forward.
    fn anchor_at(&self, wall: Instant) -> &Anchor {
        self.anchors.iter().rev().find(|a| a.wall <= wall).unwrap_or(&self.anchors[0])
    }

    /// Media time at `wall`. Clamps into pauses: a wall instant inside a pause maps to
    /// that pause's frozen media position — exactly like `session_media_time` clamps a
    /// wall instant in a pause gap to the media position where the two neighbouring
    /// segments meet.
    pub(crate) fn media_at(&self, wall: Instant) -> f64 {
        let a = self.anchor_at(wall);
        if a.running {
            a.media + wall.saturating_duration_since(a.wall).as_secs_f64()
        } else {
            a.media
        }
    }

    /// Whether the clock is paused AT `wall` (same boundary convention as `media_at`).
    pub(crate) fn is_paused_at(&self, wall: Instant) -> bool {
        !self.anchor_at(wall).running
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

    #[test]
    fn new_clock_starts_at_media_zero_running() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        assert_eq!(clock.media_at(t0), 0.0);
        assert!(!clock.is_paused_at(t0));
    }

    #[test]
    fn media_advances_one_to_one_with_wall_time_while_running() {
        // Mirrors `media_time_inside_first_segment_is_identity` (4.5).
        let t0 = base();
        let clock = MediaClock::new(t0);
        assert_eq!(clock.media_at(t0 + secs(4.5)), 4.5);
    }

    #[test]
    fn media_at_before_construction_clamps_to_zero() {
        let t0 = base();
        let clock = MediaClock::new(t0);
        assert_eq!(clock.media_at(t0 - secs(1.0)), 0.0);
    }

    // Mirrors finalize.rs's SPANS = [(0.0, 10.0), (25.0, INFINITY)]: run 10s, pause for
    // 15s, resume, keep running. Same wall offsets, same expected media values.
    #[test]
    fn pause_freezes_media_time_for_any_query_during_the_pause() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.pause(t0 + secs(10.0));
        // Mirrors `media_time_in_the_pause_gap_clamps_to_the_seam`'s exact assertions.
        assert_eq!(clock.media_at(t0 + secs(10.0)), 10.0);
        assert_eq!(clock.media_at(t0 + secs(17.3)), 10.0);
        assert_eq!(clock.media_at(t0 + secs(24.999)), 10.0);
        assert!(clock.is_paused_at(t0 + secs(17.3)));
    }

    #[test]
    fn resume_continues_media_from_the_frozen_value() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.pause(t0 + secs(10.0));
        clock.resume(t0 + secs(25.0));
        // Mirrors `media_time_inside_a_later_segment_offsets_by_prior_media` (13.0).
        assert_eq!(clock.media_at(t0 + secs(28.0)), 13.0);
        assert!(!clock.is_paused_at(t0 + secs(28.0)));
    }

    #[test]
    fn media_at_exact_pause_instant_is_already_paused() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.pause(t0 + secs(10.0));
        assert!(clock.is_paused_at(t0 + secs(10.0)));
        assert_eq!(clock.media_at(t0 + secs(10.0)), 10.0);
    }

    #[test]
    fn media_at_exact_resume_instant_is_already_running() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.pause(t0 + secs(10.0));
        clock.resume(t0 + secs(25.0));
        assert!(!clock.is_paused_at(t0 + secs(25.0)));
        assert_eq!(clock.media_at(t0 + secs(25.0)), 10.0);
    }

    #[test]
    fn double_pause_is_idempotent() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.pause(t0 + secs(10.0));
        clock.pause(t0 + secs(12.0)); // already paused: ignored
        clock.resume(t0 + secs(25.0));
        assert_eq!(clock.media_at(t0 + secs(20.0)), 10.0);
        assert_eq!(clock.media_at(t0 + secs(28.0)), 13.0);
    }

    #[test]
    fn resume_while_running_is_idempotent() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.resume(t0 + secs(3.0)); // never paused: ignored
        assert_eq!(clock.media_at(t0 + secs(5.0)), 5.0);
    }

    // Mirrors `media_time_past_finite_spans_clamps_to_total` (finite spans [(0,10),
    // (25,5)] -> total 15.0): pause again 5s into the second run and never resume ->
    // every later query clamps to that frozen total.
    #[test]
    fn pausing_again_with_no_further_resume_clamps_to_the_last_frozen_value() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.pause(t0 + secs(10.0));
        clock.resume(t0 + secs(25.0));
        clock.pause(t0 + secs(30.0)); // 5s into the second run: media 10 + 5 = 15
        assert_eq!(clock.media_at(t0 + secs(30.0)), 15.0);
        assert_eq!(clock.media_at(t0 + secs(31.0)), 15.0);
        assert_eq!(clock.media_at(t0 + secs(100.0)), 15.0);
    }

    // Mirrors `media_time_three_segments_accumulates_offsets` (spans accumulate to 25.0).
    #[test]
    fn multiple_pause_resume_cycles_accumulate_media() {
        let t0 = base();
        let mut clock = MediaClock::new(t0);
        clock.pause(t0 + secs(10.0));
        clock.resume(t0 + secs(25.0)); // +10 running so far
        clock.pause(t0 + secs(35.0)); // another 10s run -> media 20
        clock.resume(t0 + secs(50.0));
        assert_eq!(clock.media_at(t0 + secs(55.0)), 25.0); // +5 more
    }
}
