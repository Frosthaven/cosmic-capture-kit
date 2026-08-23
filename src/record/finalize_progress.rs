//! What the finalize pass is doing right now, for the spinner the user is watching
//! (DRAGON-728).
//!
//! # Why this exists
//!
//! "Finalizing your recording" is an indeterminate spinner, and a finalize that takes a
//! while is therefore indistinguishable from a hang. That ambiguity is what opened
//! DRAGON-716 in the first place, and it came back from the field on 2026-08-21: an inked
//! 40s 5120x1394 at 60fps take sat on a blank spinner for 55 seconds
//! (`ink bake: handing finalize` at 13:18:10, `ink bake: fed 2433 of 2433 overlay frames to
//! finalize` at 13:19:05), and the owner reported it as "man that took a long time to get
//! ready". Nothing was wrong with that recording. The user simply had no way to tell.
//!
//! DRAGON-716 already removed the DANGER (the reap is progress-based, so a long pass is no
//! longer killed for being long). This says out loud that the wait is work.
//!
//! # Where the number comes from, and where it does NOT
//!
//! **The ink FEED is the measurable phase, and it is the phase that is slow.**
//! `run_ink_pass` writes one RGBA overlay frame per video frame into ffmpeg's stdin, and
//! those writes BLOCK on the encoder: the pipe holds 64KB, which is a fraction of one frame
//! at any size worth measuring, so the feed advances at exactly the speed ffmpeg consumes it.
//! The field log above says the same thing from the outside: the feed loop's own start and
//! end bracket the whole 55 second wait. So "frames handed over" is a faithful account of
//! "frames encoded", to within ffmpeg's own queue, and it needs nothing from ffmpeg at all.
//!
//! **ffmpeg's own `frame= ... time=...` progress line is NOT available here, and adding it
//! would mean changing the argv.** Every finalize invocation carries `-loglevel error`
//! (`finalize::build_finalize_cmd`), and ffmpeg logs its stats line at `AV_LOG_INFO`, so the
//! log level suppresses it: MEASURED 2026-08-21 on ffmpeg 9.0, running the shipped finalize
//! argv over a 20s two-audio-stream fixture, stderr was **zero bytes**. The same command with
//! `-stats` added printed the familiar `frame= 424 fps=412 ... time=00:00:14.06` lines, which
//! is the whole mechanism: `-stats` bypasses the log level with a direct `fprintf`. So the
//! ring `ffmpeg_log` already drains has nothing in it to parse, and there is deliberately no
//! `time=` parser in this module. `ink_sw_video_args` is pinned byte-identical by DRAGON-716
//! and the finalize invocations are historical, so neither `-stats` nor `-progress` is ours
//! to add for a cosmetic readout. If that pin is ever lifted for another reason, this is the
//! module the parser belongs in.
//!
//! **Nothing here invents a number.** The plain mux (video stream-copied, audio gated and
//! re-encoded once) has no honest denominator, so it publishes a phase and no fraction, and
//! the UI keeps the spinner it has always had. Same for the span of the ink pass AFTER the
//! last overlay frame, where ffmpeg encodes the rest of the video with nothing drawn on it
//! and no signal in this process can count it. A bar frozen on one value reads as broken
//! rather than as "there is nothing to show yet", which is the lesson
//! `preview::chrome::upload_progress_track` already carries, so a phase that cannot count
//! says so and hands the spinner back.
//!
//! # The shape
//!
//! One process-wide [`Slot`], published by `finalize` and read by the app's existing 100ms
//! recording poll. It is a shared static rather than an `Arc` on `RecordHandle` for the same
//! reason `ffmpeg_log`'s ring table is: threading a handle down would mean editing every
//! platform's worker, two of which cannot be compiled on the machine a change is written on,
//! and `finalize_with_ink` is already the ONE seam every worker's tail call goes through. One
//! recording runs at a time in this one-shot process, and the pass clears the slot however it
//! ends.
//!
//! The decisions are pure and unit-tested here; the syscall-free plumbing is three lines of
//! the house drain idiom. The user-facing COPY is not here: it belongs to the app
//! (`app::preview::video::finalize_readout`), which is where the dialog and label rules are
//! already enforced.

use std::sync::Mutex;

/// Which part of the finalize pass is running.
///
/// The `attempt` on the ink phases is which try this is: DRAGON-716 runs the recording's own
/// hardware tier first and falls back to software, and the fallback feeds the SAME frames
/// again from zero. Carrying it is what lets [`advance`] tell a fresh feed apart from a
/// reading that went backwards, so the second attempt restarts the bar instead of being
/// clamped to the dead attempt's high-water mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The plain finalize: video stream-copied, audio gated and re-encoded once. No honest
    /// number is available (see the module doc), so this is a wait with a name and nothing
    /// else.
    Muxing,
    /// Feeding the ink overlay's frames into the re-encode. The one phase that can count.
    BakingInk { attempt: u32 },
    /// The ink pass after its last overlay frame: ffmpeg is encoding the rest of the video
    /// (and rewriting the moov atom for `+faststart`) with nothing left to feed it.
    FinishingInk { attempt: u32 },
}

impl Phase {
    /// **Pure**, unit-tested: whether this phase can honestly say how far along it is.
    ///
    /// Only the feed can. The other two are named waits, and saying so is the whole point:
    /// the alternative is a number nobody measured.
    pub(crate) fn counts(self) -> bool {
        matches!(self, Phase::BakingInk { .. })
    }
}

/// How far the finalize pass has got, in frames of the video it is producing.
///
/// `total` is the recording's OWN frame count (`InkJob::video_frames`, what the worker
/// actually emitted), never the number of overlay frames the bake will feed. The pass encodes
/// the whole video whether or not the ink covers it, so a drawing that ends a minute into a
/// ten minute take must not report 100% with nine minutes still to encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Progress {
    pub phase: Phase,
    /// Frames handed to the encoder so far.
    pub done: u64,
    /// Frames the whole pass will produce, or 0 when the worker could not say.
    pub total: u64,
}

impl Progress {
    /// **Pure**, unit-tested: the fraction to draw, or `None` when nothing honest is known.
    ///
    /// `None` for a phase that cannot count, for a missing denominator, and never a value
    /// outside `0.0..=1.0`: `done` is a count of writes and `total` is a count the worker
    /// took at stop, so a slow last frame or an off-by-one must not be able to draw a bar
    /// past its own track.
    pub(crate) fn fraction(self) -> Option<f32> {
        if !self.phase.counts() || self.total == 0 {
            return None;
        }
        Some((self.done.min(self.total) as f32 / self.total as f32).clamp(0.0, 1.0))
    }
}

/// **Pure**, unit-tested: what the slot should hold, given what it already held.
///
/// Within one phase the reading only ever goes FORWARD. A fraction that slips backwards
/// reads as work being undone, and the pass never undoes any: `done` counts completed
/// writes.
///
/// A DIFFERENT phase replaces outright, which is what makes the DRAGON-716 software fallback
/// honest. That attempt starts its feed again at frame zero, and clamping it to the failed
/// attempt's mark would freeze the bar full for the whole of the slow pass, which is exactly
/// the "is it stuck" question this module exists to answer.
pub(crate) fn advance(prev: Option<Progress>, next: Progress) -> Progress {
    match prev {
        Some(p) if p.phase == next.phase && p.total == next.total => {
            Progress { done: p.done.max(next.done), ..next }
        }
        _ => next,
    }
}

/// The published state of one finalize pass.
///
/// A named type rather than a bare static so the publish rule can be tested on an INSTANCE.
/// The process-wide one is written by the real finalize, and the ffmpeg-backed e2e tests run
/// that finalize for real, in this same process, alongside everything else: a test that
/// asserted on the shared static would be racing them.
pub(crate) struct Slot(Mutex<Option<Progress>>);

impl Slot {
    const fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// Record `next`, subject to [`advance`]. Poison-tolerant: a progress readout is never
    /// worth taking a recording down for.
    fn publish(&self, next: Progress) {
        if let Ok(mut g) = self.0.lock() {
            *g = Some(advance(*g, next));
        }
    }

    /// The current state, if a pass is running. Non-blocking and poison-tolerant, and it does
    /// NOT drain: the app polls this ten times a second and wants the state each time.
    fn read(&self) -> Option<Progress> {
        self.0.lock().ok().and_then(|g| *g)
    }

    /// Forget the pass. Called however it ended, so nothing can go on claiming a phase is
    /// running after the finalize returned.
    fn clear(&self) {
        if let Ok(mut g) = self.0.lock() {
            *g = None;
        }
    }
}

/// The one slot this process's finalize publishes into.
static CURRENT: Slot = Slot::new();

/// Record where the running finalize pass has got to.
pub(crate) fn publish(next: Progress) {
    CURRENT.publish(next);
}

/// What the running finalize pass is doing, or `None` when none is.
pub(crate) fn read() -> Option<Progress> {
    CURRENT.read()
}

/// The pass is over, whatever it delivered.
pub(crate) fn clear() {
    CURRENT.clear();
}

#[cfg(test)]
mod fraction_tests {
    use super::*;

    fn baking(done: u64, total: u64) -> Progress {
        Progress { phase: Phase::BakingInk { attempt: 1 }, done, total }
    }

    /// The feed is the one phase with a number, and it is `done / total`.
    #[test]
    fn the_feed_reports_its_share_of_the_whole_video() {
        assert_eq!(baking(0, 2433).fraction(), Some(0.0));
        assert_eq!(baking(2433, 2433).fraction(), Some(1.0));
        let half = baking(1200, 2400).fraction().expect("a fraction");
        assert!((half - 0.5).abs() < 0.001, "{half}");
    }

    /// **The no-lies rule.** A phase nobody can count says nothing, so the UI keeps the
    /// spinner it has always had rather than being handed a number that was never measured.
    #[test]
    fn a_phase_that_cannot_count_offers_no_fraction() {
        assert_eq!(Progress { phase: Phase::Muxing, done: 0, total: 0 }.fraction(), None);
        // Even handed a full-looking pair, since the plain mux's frames are not counted.
        assert_eq!(Progress { phase: Phase::Muxing, done: 100, total: 100 }.fraction(), None);
        assert_eq!(
            Progress { phase: Phase::FinishingInk { attempt: 1 }, done: 90, total: 100 }
                .fraction(),
            None
        );
    }

    /// A worker that could not say how many frames it emitted leaves the pass indeterminate
    /// rather than dividing by zero or inventing a denominator.
    #[test]
    fn a_missing_denominator_is_not_a_number() {
        assert_eq!(baking(500, 0).fraction(), None);
    }

    /// The bar can never be drawn past its own track. The two counts come from different
    /// places (writes here, the worker's emitted count there), so they are allowed to
    /// disagree at the edge and must not produce a fraction above 1.
    #[test]
    fn an_overrun_reading_still_lands_inside_the_track() {
        assert_eq!(baking(9_000, 2_433).fraction(), Some(1.0));
    }
}

#[cfg(test)]
mod advance_tests {
    use super::*;

    fn baking(attempt: u32, done: u64) -> Progress {
        Progress { phase: Phase::BakingInk { attempt }, done, total: 1_000 }
    }

    /// The first reading of a pass is simply taken.
    #[test]
    fn an_empty_slot_takes_whatever_arrives() {
        assert_eq!(advance(None, baking(1, 7)), baking(1, 7));
    }

    /// **Monotonic within one phase**: a later, smaller reading never moves the bar back.
    #[test]
    fn a_reading_that_slipped_backwards_never_moves_the_bar_back() {
        let prev = Some(baking(1, 900));
        assert_eq!(advance(prev, baking(1, 400)), baking(1, 900));
        assert_eq!(advance(prev, baking(1, 900)), baking(1, 900), "and equal holds");
        assert_eq!(advance(prev, baking(1, 901)), baking(1, 901), "but growth passes");
    }

    /// **The DRAGON-716 software fallback restarts the bar.** It feeds the same frames again
    /// from zero, so clamping it to the failed hardware attempt's mark would leave the bar
    /// full for the whole of the slower pass.
    #[test]
    fn a_second_attempt_starts_its_own_count() {
        let prev = Some(baking(1, 1_000));
        assert_eq!(advance(prev, baking(2, 0)), baking(2, 0));
    }

    /// Moving on to a phase that cannot count replaces the reading rather than holding the
    /// feed's last one, so nothing downstream can show a stale full bar for the tail.
    #[test]
    fn moving_to_an_uncountable_phase_replaces_the_reading() {
        let prev = Some(baking(1, 1_000));
        let tail = Progress { phase: Phase::FinishingInk { attempt: 1 }, done: 1_000, total: 1_000 };
        assert_eq!(advance(prev, tail), tail);
        assert_eq!(advance(Some(tail), tail).fraction(), None);
    }

    /// A changed denominator is a different pass's reading, not growth on this one, so it
    /// replaces rather than merging. Two passes cannot share a high-water mark measured in
    /// different units.
    #[test]
    fn a_changed_total_replaces_rather_than_merging() {
        let prev = Some(baking(1, 900));
        let other = Progress { phase: Phase::BakingInk { attempt: 1 }, done: 10, total: 50 };
        assert_eq!(advance(prev, other), other);
    }
}

#[cfg(test)]
mod slot_tests {
    use super::*;

    /// The slot's whole contract, on an INSTANCE of its own: publish applies [`advance`],
    /// read does not drain, and clear forgets the pass. Deliberately not asserted against the
    /// process-wide `CURRENT`, which the ffmpeg-backed finalize tests write to for real.
    #[test]
    fn a_slot_advances_holds_and_clears() {
        let slot = Slot::new();
        assert_eq!(slot.read(), None);
        let a = Progress { phase: Phase::BakingInk { attempt: 1 }, done: 10, total: 100 };
        slot.publish(a);
        assert_eq!(slot.read(), Some(a));
        assert_eq!(slot.read(), Some(a), "reading must not drain: the app polls it");
        slot.publish(Progress { done: 4, ..a });
        assert_eq!(slot.read(), Some(a), "and the publish rule still applies");
        slot.clear();
        assert_eq!(slot.read(), None);
    }
}
