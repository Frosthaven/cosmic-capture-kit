//! Custom libcosmic `Widget` implementations.

pub mod annotation_canvas;
pub mod annotation_fx;
pub mod arrow_cursor;
/// The colour picker's transparent input surface (DRAGON-582): pointer moves + the pick
/// click. It draws nothing; see its module doc for why the visuals are stacked elements.
pub mod color_pick;
pub mod copy_button;
pub mod crop_canvas;
pub mod crop_window;
pub mod drag_area;
/// What the GPU will accept for a shader primitive's viewport (DRAGON-401) — observed from
/// the shader `prepare`s that are handed a `wgpu::Device`, read by the preview's zoom ceiling.
pub mod gpu;
pub mod hide_when_clipped;
pub mod icons;
pub mod notched_slider;
pub mod output_selection;
pub mod region_selection;
pub mod upload_stripes;
pub mod zoom_pan;

pub use drag_area::DragArea;
pub use hide_when_clipped::hide_when_clipped;
pub use notched_slider::notched_slider;
pub use output_selection::OutputSelection;
pub use region_selection::RegionSelection;
pub use crop_window::CropWindow;
pub use zoom_pan::ZoomPan;

/// Post-pointer-enter cursor RE-ASSERT (DRAGON-331). cosmic-comp DROPS a `wl_pointer.set_cursor`
/// issued too soon after a fresh `wl_pointer.enter` (proven by an `update_mouse` trace: the
/// identical `set_cursor(Crosshair)` is dropped a few frames post-enter but sticks later). iced
/// applies the cursor only on a redraw, and only when the resolved `mouse::Interaction` CHANGES,
/// so a widget whose cursor is unchanged across a leave→re-enter (e.g. a draw crosshair over its
/// whole surface) never re-issues it, leaving the compositor's default showing.
///
/// The remedy a widget opts into: assert the real cursor IMMEDIATELY on enter (no flash when the
/// compositor honors it), then RE-ASSERT once at [`SETTLE`] past the enter — with a brief default
/// "dip" of [`DIP`] just before the deadline so that re-assert is a genuine change iced re-issues,
/// now late enough that the compositor keeps it. A widget: (1) holds an `Option<Instant>` entry
/// stamp in its `State`, (2) calls [`arm`] from `update` to maintain it + schedule the boundary
/// redraws, and (3) calls [`in_dip`] in `mouse_interaction`, returning its DEFAULT cursor while true.
pub mod cursor_reassert {
    use cosmic::iced::core::time::{Duration, Instant};
    use cosmic::iced::core::{mouse, Event, Shell};

    /// When we re-assert the cursor after a fresh enter — past cosmic-comp's post-enter drop window.
    pub const SETTLE: Duration = Duration::from_millis(200);
    /// Length of the pre-deadline default "dip" that makes the re-assert a change iced re-issues.
    pub const DIP: Duration = Duration::from_millis(24);

    /// Whether we are in the pre-deadline dip and should return the DEFAULT cursor (call from
    /// `mouse_interaction`). `entered_at` is the widget's stored entry stamp.
    pub fn in_dip(entered_at: Option<Instant>) -> bool {
        entered_at.is_some_and(|t| (SETTLE.saturating_sub(DIP)..SETTLE).contains(&t.elapsed()))
    }

    /// Start the dance from the surface's own APPEARANCE instead of from a pointer enter
    /// (DRAGON-601, second pass). Call from `update` on any event the widget is guaranteed to
    /// see, then let [`arm`] carry the schedule forward as usual. A no-op once armed, so a real
    /// `CursorEntered` later still re-arms through [`arm`] and nothing double-schedules.
    ///
    /// **Why the enter is not a good enough trigger, measured.** An overlay that maps UNDER a
    /// resting pointer asks for its cursor on its very first frame, and on cosmic-comp that
    /// frame lands about 55 ms BEFORE the compositor sends `wl_pointer.enter`. A cursor
    /// request with no enter serial cannot be made: sctk drops it, and the seat then records
    /// the icon as applied anyway (`active_icon`, and our fork's `hidden`), so the enter's own
    /// re-assert compares equal and skips. The request is lost in a way nothing reports, and
    /// the compositor's default sprite stays on screen until the user moves the mouse and
    /// something finally issues a request while a serial exists.
    ///
    /// Arming here puts the re-assert at [`SETTLE`] after the surface appeared, which on that
    /// timeline is about 150 ms after the enter: late enough that a serial exists, and late
    /// enough to clear cosmic-comp's own post-enter drop window, which is the reason [`SETTLE`]
    /// is 200 ms in the first place.
    ///
    /// The old trigger could not have worked on a layer surface for a second, independent
    /// reason: the widget never receives `CursorEntered` there at all. The compositor delivers
    /// `wl_pointer.enter` with a position, and the toolkit passes neither the event nor the
    /// position on until the pointer MOVES. That is a toolkit-side gap, not something a caller
    /// can close, which is exactly why this trigger does not depend on it.
    pub fn arm_on_appear<Msg>(entered_at: &mut Option<Instant>, shell: &mut Shell<'_, Msg>) {
        if entered_at.is_some() {
            return;
        }
        let now = Instant::now();
        *entered_at = Some(now);
        shell.request_redraw_at(now + SETTLE.saturating_sub(DIP));
    }

    /// Maintain `entered_at` from pointer lifecycle events and schedule the dip-start + deadline
    /// redraws so the re-assert fires (call from `update`, before consuming the event).
    pub fn arm<Msg>(entered_at: &mut Option<Instant>, event: &Event, shell: &mut Shell<'_, Msg>) {
        let dip_start = SETTLE.saturating_sub(DIP);
        match event {
            Event::Mouse(mouse::Event::CursorEntered) => {
                let now = Instant::now();
                *entered_at = Some(now);
                shell.request_redraw_at(now + dip_start);
            }
            Event::Mouse(mouse::Event::CursorLeft) => *entered_at = None,
            // A REDRAW carries the schedule forward exactly as a move does (DRAGON-601), and
            // that is what makes the dance complete under a pointer nobody is touching.
            //
            // The enter above books ONE redraw, at the start of the dip. Reaching it used to
            // book nothing further, so the deadline redraw only ever arrived if the user
            // happened to move the mouse in that 24ms window; with a stationary pointer the
            // widget dipped to the default cursor and stayed there, which is the opposite of
            // the bug this module exists to fix. It went unnoticed because the two widgets
            // using it are a region selector and a drawing canvas, where a hand is on the
            // mouse by definition. The colour picker's overlay appears UNDER a resting pointer
            // and asks for a hidden cursor, so it is the first caller that has to survive
            // enter-then-nothing.
            Event::Mouse(mouse::Event::CursorMoved { .. })
            | Event::Window(cosmic::iced::core::window::Event::RedrawRequested(_)) => {
                if let Some(t) = *entered_at {
                    let e = t.elapsed();
                    if e < dip_start {
                        shell.request_redraw_at(t + dip_start);
                    } else if e < SETTLE {
                        shell.request_redraw_at(t + SETTLE);
                    }
                }
            }
            _ => {}
        }
    }

    /// DRAGON-331 / DRAGON-601: the dip window. Small, but a widget returning its default
    /// cursor is a VISIBLE state, so both edges matter: too early and the cursor flickers on
    /// every enter, too late and the re-assert lands inside cosmic-comp's drop window and the
    /// whole dance achieves nothing.
    #[cfg(test)]
    mod dip_window_tests {
        use super::*;

        /// The dip sits at the END of the settle window, immediately before the deadline, so
        /// the re-assert that follows it is both a change AND late enough to stick.
        #[test]
        fn the_dip_is_the_last_moments_before_the_deadline() {
            assert!(DIP < SETTLE, "a dip at least as long as the settle never re-asserts");
            let start = SETTLE.saturating_sub(DIP);
            let now = Instant::now();
            // Just inside the dip, and just past its far edge.
            assert!(in_dip(Some(now - start)), "the dip must be open at its start");
            assert!(!in_dip(Some(now - SETTLE)), "the dip must be closed at the deadline");
        }

        /// Before the dip the widget asserts its REAL cursor immediately, so a compositor that
        /// honours the first request shows no flicker at all.
        #[test]
        fn nothing_dips_before_the_window_opens() {
            let now = Instant::now();
            assert!(!in_dip(Some(now)), "a fresh enter asserts the real cursor at once");
            assert!(!in_dip(Some(now - Duration::from_millis(1))));
        }

        /// No entry stamp means no dance: a widget whose pointer has left, or which never saw
        /// an enter, keeps its ordinary cursor resolution.
        #[test]
        fn an_unarmed_widget_never_dips() {
            assert!(!in_dip(None));
        }
    }
}
