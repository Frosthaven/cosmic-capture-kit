//! Custom libcosmic `Widget` implementations.

pub mod annotation_canvas;
pub mod annotation_fx;
pub mod arrow_cursor;
pub mod drag_area;
/// DRAGON-366 — TEMPORARY per-frame preview-editor diagnostics. Remove with its call sites
/// (all tagged `DRAGON-366`) once the large-capture lag report is settled.
pub mod dragon366;
pub mod hide_when_clipped;
pub mod icons;
pub mod notched_slider;
pub mod output_selection;
pub mod region_selection;
pub mod zoom_pan;

pub use drag_area::DragArea;
pub use hide_when_clipped::hide_when_clipped;
pub use notched_slider::notched_slider;
pub use output_selection::OutputSelection;
pub use region_selection::RegionSelection;
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
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
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
}
