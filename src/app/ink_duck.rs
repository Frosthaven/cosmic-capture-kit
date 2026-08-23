//! Pause other media players while a live-annotation tool is armed (DRAGON-694).
//!
//! The SECOND trigger for the guard in [`crate::audio::ducking`]. The first one is the
//! preview editor: a video document with a soundtrack holds the guard for as long as it is
//! open. This one holds it for as long as a DRAWING tool is selected during a live
//! recording, and lets go the moment the toolbar returns to Pointer.
//!
//! The guard itself is untouched, and so is its discipline: the babysitter child pauses only
//! what was PLAYING, remembers exactly which players it paused, resumes only those, and never
//! re-pauses. So a user who resumes their music mid-annotation keeps it, because nothing here
//! ever pauses twice; see [`crate::audio::ducking`]'s own doc for the pipe-EOF contract.
//!
//! WHY the setting is separate, and default OFF. This trigger changes the recording's
//! CONTENT: the pause is audible in the recorded system track, and someone annotating over a
//! video they meant to be playing wants that video to keep playing. The preview-editor
//! trigger has no such cost (nothing is being recorded), which is why that one is default on
//! and this one is not. One toggle covering both would force the user to give up the trigger
//! they want to be rid of the one they do not.
//!
//! WHY the resume cannot be forgotten. Four independent nets, outermost last, and each one
//! alone is enough:
//!
//! 1. Returning to Pointer releases it ([`App::ink_select`] is the ONE arm/disarm dispatch,
//!    and Escape routes there too, so there is no second way to disarm).
//! 2. Pressing stop releases it (`App::stop_recording`), so the music comes back when the
//!    user stops rather than when finalize finishes, which on a long take is seconds later.
//! 3. The recording ending releases it ([`App::clear_recording_state`], the one place that
//!    says what "no longer recording" means), which also covers the endings nobody pressed
//!    stop for: a wedged session abandoned, a cancelled take.
//! 4. The SESSION ending releases it (`App::finish_session`), and below that, the process
//!    exiting at all closes the babysitter's stdin, which is what it is blocked on. That last
//!    one covers a crash, so no failure mode of ours can leave the owner's music paused.
//!
//! The decision is pure and lives here; the D-Bus / MediaRemote / GSMTC calls stay in
//! [`crate::audio::ducking`] and its per-platform backends.

use super::App;

/// What a media-guard transition should do. Three states rather than a `bool`, because
/// "keep holding it" and "keep not holding it" must both be reachable without touching the
/// guard: re-engaging one already held would spawn a second babysitter, and a spurious
/// release would resume the player mid-annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DuckStep {
    /// Spawn the guard: pause whatever is playing.
    Engage,
    /// Drop the guard: resume exactly what it paused.
    Release,
    /// Leave the guard alone.
    Unchanged,
}

/// Should the guard be HELD right now?
///
/// All three facts are required, and each one alone is a reason to let go: the user turned
/// the setting off, the recording is over, or the toolbar went back to Pointer.
///
/// `recording_live` is "a recording is running AND the user has not pressed stop", not
/// merely "a worker exists". The two differ by the whole finalize wait, which can be seconds
/// on a long take, and nothing is being annotated during it.
///
/// Pure, and unit-tested below.
pub(super) fn ink_duck_wanted(enabled: bool, recording_live: bool, drawing: bool) -> bool {
    enabled && recording_live && drawing
}

/// The transition from what the guard is doing (`held`) to what it should be doing
/// (`wanted`).
///
/// Pure, and unit-tested below.
pub(super) fn ink_duck_step(held: bool, wanted: bool) -> DuckStep {
    match (held, wanted) {
        (false, true) => DuckStep::Engage,
        (true, false) => DuckStep::Release,
        _ => DuckStep::Unchanged,
    }
}

impl App {
    /// Bring the live-annotation media guard in line with the app's state.
    ///
    /// Called at every edge that can change the answer: a tool selection, the stop press, the
    /// recording ending, the session ending, and the setting itself being toggled. Idempotent,
    /// so an extra call costs a comparison.
    ///
    /// `ink_drawing_armed` already answers `false` when the tools are not offered, so the
    /// recording fact is asked separately and explicitly rather than leaned on: a guard held
    /// past the end of a recording is the failure this feature must not have.
    pub(super) fn sync_ink_duck(&mut self) {
        let live = self.recording.is_some() && !self.recording_stopping;
        let wanted =
            ink_duck_wanted(self.pause_others_during_ink, live, self.ink_drawing_armed());
        match ink_duck_step(self.ink_duck.is_some(), wanted) {
            DuckStep::Engage => {
                log::debug!("live annotation: pausing other media players");
                self.ink_duck = Some(crate::audio::ducking::OtherAudioDuck::engage());
            }
            DuckStep::Release => {
                log::debug!("live annotation: resuming the media players we paused");
                self.ink_duck = None;
            }
            DuckStep::Unchanged => {}
        }
    }
}

#[cfg(test)]
mod ink_duck_wanted_tests {
    use super::*;

    #[test]
    fn every_fact_is_required() {
        assert!(ink_duck_wanted(true, true, true));
        // The setting is the user's opt-in, and it is default OFF.
        assert!(!ink_duck_wanted(false, true, true));
        // Nothing live to keep quiet for: no recording, or one the user has stopped.
        assert!(!ink_duck_wanted(true, false, true));
        // Back to Pointer.
        assert!(!ink_duck_wanted(true, true, false));
    }

    #[test]
    fn the_default_configuration_never_pauses_anything() {
        // The shipped default: whatever the recording and the toolbar are doing, a user who
        // has not opted in keeps their music.
        for live in [false, true] {
            for drawing in [false, true] {
                assert!(!ink_duck_wanted(false, live, drawing));
            }
        }
    }
}

#[cfg(test)]
mod ink_duck_step_tests {
    use super::*;

    #[test]
    fn the_two_edges_act() {
        assert_eq!(ink_duck_step(false, true), DuckStep::Engage);
        assert_eq!(ink_duck_step(true, false), DuckStep::Release);
    }

    #[test]
    fn the_two_steady_states_do_nothing() {
        // Re-engaging a guard already held would spawn a second babysitter, and the second
        // one would find nothing playing (the first already paused it) and hold nothing.
        assert_eq!(ink_duck_step(true, true), DuckStep::Unchanged);
        assert_eq!(ink_duck_step(false, false), DuckStep::Unchanged);
    }

    #[test]
    fn a_disarm_after_a_release_is_not_a_second_release() {
        // The recording ends with a tool armed (release), then the tool selection is reset
        // (nothing left to release). Both orderings happen, and only the first one may act.
        let held = false; // already released by `clear_recording_state`
        assert_eq!(ink_duck_step(held, ink_duck_wanted(true, false, false)), DuckStep::Unchanged);
    }
}
