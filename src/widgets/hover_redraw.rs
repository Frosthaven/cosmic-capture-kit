//! A wrapper widget that requests a redraw when a pointer move could have changed
//! its content's hover highlight (DRAGON-681: settings tabs did not light up when
//! the pointer went straight from one tab to the next).
//!
//! THE DEFECT. libcosmic's `segmented_button` (the widget behind both
//! `widget::tab_bar::horizontal` and `widget::nav_bar`) keeps its own hover field
//! and updates it from the cursor position: `LocalState.hovered` is reassigned in
//! `update` whenever the cursor is over a different button's bounds (libcosmic
//! `src/widget/segmented_button/widget.rs`, the `for (key, bounds)` loop). What it
//! never does is ask for the frame that would SHOW the change: there is not one
//! `shell.request_redraw()` in that whole file. Its sibling `widget::button` gets
//! this right, and the two sit next to each other in the same crate: button's
//! `CursorMoved | FingerMoved` arm flips `state.is_hovered` and calls
//! `shell.request_redraw()` in the same breath (libcosmic `src/widget/button/widget.rs`).
//!
//! iced 0.14 renders on demand. After an event batch the winit loop schedules a
//! frame only for (a) a published message, (b) a changed top-level
//! `mouse::Interaction`, or (c) a widget-requested redraw (`iced_winit`
//! `src/lib.rs`, the `needs_redraw` / `mouse_changed` block that matches
//! `user_interface::State::Updated`). The `unconditional-rendering` feature that
//! would paint every frame regardless is not enabled anywhere in our graph.
//!
//! WHY TAB-TO-TAB STARVES AND OUTSIDE-IN DOES NOT. Path (b) is what has been
//! carrying the tab strip all along, by accident. `SegmentedButton::mouse_interaction`
//! answers `Pointer` over ANY enabled button and `Interaction::None` anywhere else,
//! so it is a two-valued function of "am I over a tab at all":
//!
//! * off the strip, then onto a tab: `None` to `Pointer`. That IS a change, path
//!   (b) fires, the frame lands, the highlight paints. This is the case the owner
//!   reported as working.
//! * one tab straight to the next: `Pointer` to `Pointer`. No change, no message,
//!   no request, so nothing schedules a frame. `hovered` is already correct in the
//!   widget tree and the pixels are simply stale until something unrelated
//!   repaints the window.
//!
//! There is no gap to save it, either. `tab_bar::horizontal` never calls
//! `.spacing()` and the `SegmentedButton` default is 0, so adjacent tab bounds are
//! exactly contiguous (`horizontal.rs`: `bounds.x += layout_bounds.width + spacing`)
//! and the pointer never passes through a "no tab here" pixel that would flip the
//! interaction back to `None`. The nav rail is the same widget with the same
//! missing request, but it DOES set `.spacing(space_xxs)`, a gap of 4 to 12 px
//! depending on the theme's density, so a slow move usually lands in it and buys a
//! frame by luck; a quick flick that jumps the gap in one motion event starves
//! exactly like the tab strip. That is the difference between "never" and
//! "sometimes" in the report.
//!
//! Our `arrow_cursor` wrapper is NOT the cause, though it is in the path: it maps
//! `Pointer` to `Idle` uniformly, which keeps the function two-valued (`None` off
//! the strip, `Idle` over any tab) and so preserves both bullets above unchanged.
//! Stock libcosmic has the same hole.
//!
//! WHY IT WAS REPORTED ON LINUX. Nothing above is platform-specific: the missing
//! request is in portable widget code. What Wayland adds is that a surface repaints
//! ONLY when the client asks for a frame, so a missing request is the whole story
//! there, with nothing else to cover for it. DRAGON-648 recorded the same asymmetry
//! for the dropdown one widget over, where AppKit's own view invalidation masked an
//! identically missing request on macOS. This wrapper is portable and carries no
//! `cfg`, so if the symptom ever shows on macOS or Windows it is already fixed.
//!
//! WHY THE FIX LIVES HERE. The real fix is two lines in libcosmic's
//! `segmented_button`, copying what its own `button` already does, and it is worth
//! offering upstream. But we do not fork libcosmic (only winit and iced, see
//! FORKED_CHANGES.md), and taking on that fork for two lines buys a rebase
//! treadmill. Patching our iced fork cannot help either: iced never learns that
//! `LocalState.hovered` moved, because the field is private to the widget, so only
//! the widget's side of the `Shell` seam can say "this move changed what the next
//! frame shows". Hence an app-side wrapper at the same seam, which is exactly the
//! shape DRAGON-648 settled on for the dropdown.
//!
//! WHAT IT COSTS. A wrapper can only see its own bounds, not the individual tab
//! bounds, so it cannot detect the flip the way `button` does; it fires on every
//! pointer move while the cursor is over the wrapped content. Wrap the hovering
//! widget itself and those bounds stay small, a 44px-tall band for a tab strip,
//! the rail's own column for the nav. The frames are not one per motion event
//! either: iced batches a whole event cycle and `RedrawRequest::NextFrame`
//! collapses the batch into a single frame, so the ceiling is the display refresh
//! rate for as long as the pointer is on the widget, which is what any hover
//! animation would already cost.
//!
//! Considered and rejected:
//! - Folding this into `arrow_cursor`, which already wraps both call sites. Same
//!   answer as DRAGON-648: its contract is "pure pass-through except the cursor",
//!   and it also wraps buttons, sliders and whole toolbars that need no help.
//! - Folding it into `press_redraw`. Different trigger, different contract, and
//!   `press_redraw` brackets dropdowns specifically so its capture measurement
//!   stays meaningful.
//! - Turning on iced's `unconditional-rendering` feature. It repaints forever, on
//!   every platform, to fix a strip of tabs.
//! - Widening the trigger to any event over the bounds. Presses and wheel steps on
//!   a segmented button already publish a message, which is path (a).

use cosmic::iced::core::widget::{Operation, Tree, tree};
use cosmic::iced::core::{
    Clipboard, Event, Layout, Length, Rectangle, Shell, Size, layout, mouse, overlay, renderer,
    touch,
};
use cosmic::widget::Widget;

pub struct HoverRedraw<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
}

/// Was the cursor over the wrapped content when we last saw a pointer event? Kept
/// so the move that carries the pointer OFF the content also earns a frame, which
/// is what clears the highlight the content is still painting.
#[derive(Default)]
struct State {
    over: bool,
}

/// Wrap `content` so a pointer move that could change its hover highlight also
/// schedules the frame that shows it (see the module docs). Wrap the hovering
/// widget itself, inside `arrow_cursor`, so the bounds we watch are the strip's
/// and not a whole page's.
pub fn hover_redraw<'a, Msg: 'a>(
    content: impl Into<cosmic::Element<'a, Msg>>,
) -> cosmic::Element<'a, Msg> {
    cosmic::Element::new(HoverRedraw {
        content: content.into(),
    })
}

/// Pure, unit-tested: does this event pass warrant a frame request?
///
/// The arms mirror the ones libcosmic's own `widget::button` uses for exactly this
/// job. A move is the only thing that can change which segment is hovered, and it
/// matters whether the cursor is over the content NOW (it may have just arrived on
/// a new segment) or was over it LAST time (it may have just left, and the stale
/// highlight has to be cleared). A cursor leaving the window, or a touch the
/// compositor takes away, is the same clearing case with no "now" to speak of.
/// Everything else, presses included, repaints through the message the segmented
/// button publishes.
fn should_request_redraw(event: &Event, over_now: bool, was_over: bool) -> bool {
    match event {
        Event::Mouse(mouse::Event::CursorMoved { .. })
        | Event::Touch(touch::Event::FingerMoved { .. }) => over_now || was_over,
        Event::Mouse(mouse::Event::CursorLeft) | Event::Touch(touch::Event::FingerLost { .. }) => {
            was_over
        }
        _ => false,
    }
}

/// Whether this event carries pointer position news, i.e. whether it should
/// refresh the remembered `over` flag. Kept separate from the decision above so a
/// press that happens to land off the content cannot rewrite what the last MOVE
/// established.
fn tracks_position(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse::Event::CursorMoved { .. } | mouse::Event::CursorLeft)
            | Event::Touch(touch::Event::FingerMoved { .. } | touch::Event::FingerLost { .. })
    )
}

impl<'a, Msg> Widget<Msg, cosmic::Theme, cosmic::Renderer> for HoverRedraw<'a, Msg> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn Operation<()>,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
        viewport: &Rectangle,
    ) {
        let over_now = cursor.is_over(layout.bounds());
        let was_over = tree.state.downcast_ref::<State>().over;

        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        if tracks_position(event) {
            tree.state.downcast_mut::<State>().over = over_now;
        }
        // The whole point: the move that changed which segment the content thinks
        // is hovered also schedules the frame that paints it.
        if should_request_redraw(event, over_now, was_over) {
            shell.request_redraw();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: cosmic::iced::core::Vector,
    ) -> Option<overlay::Element<'b, Msg, cosmic::Theme, cosmic::Renderer>> {
        // Plain forward. A segmented button's overlay is its context menu, whose
        // items are ordinary buttons that already request their own hover frames.
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, Msg: 'a> From<HoverRedraw<'a, Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: HoverRedraw<'a, Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}

/// DRAGON-681: the frame request must cover every move that can change the
/// content's hover, and nothing wider. Narrower (only moves that ENTER the
/// content) is the shipped bug, since tab-to-tab never enters anything. Wider
/// (every event over the bounds) turns each click and wheel step into a frame the
/// published message was already going to buy.
#[cfg(test)]
mod hover_frame_tests {
    use super::*;
    use cosmic::iced::core::Point;

    fn moved() -> Event {
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::ORIGIN,
        })
    }

    fn finger_moved() -> Event {
        Event::Touch(touch::Event::FingerMoved {
            id: touch::Finger(0),
            position: Point::ORIGIN,
        })
    }

    /// The shipped bug. A move from one tab to the next never leaves the strip, so
    /// `was_over` and `over_now` are both true and nothing else would ask.
    #[test]
    fn a_move_within_the_content_requests_a_frame() {
        assert!(should_request_redraw(&moved(), true, true));
        assert!(should_request_redraw(&finger_moved(), true, true));
    }

    /// Arriving from outside still requests one. The interaction change would
    /// cover this case today, but the wrapper must not depend on that accident.
    #[test]
    fn a_move_arriving_on_the_content_requests_a_frame() {
        assert!(should_request_redraw(&moved(), true, false));
    }

    /// Leaving has to repaint too, or the segment the pointer just left keeps its
    /// highlight.
    #[test]
    fn a_move_leaving_the_content_clears_the_stale_highlight() {
        assert!(should_request_redraw(&moved(), false, true));
        assert!(should_request_redraw(
            &Event::Mouse(mouse::Event::CursorLeft),
            false,
            true
        ));
    }

    /// A move that was never near the content is somebody else's business.
    #[test]
    fn a_move_nowhere_near_the_content_requests_nothing() {
        assert!(!should_request_redraw(&moved(), false, false));
        assert!(!should_request_redraw(
            &Event::Mouse(mouse::Event::CursorLeft),
            false,
            false
        ));
    }

    /// Presses, releases and wheel steps on a segmented button publish a message,
    /// and a published message already schedules the frame.
    #[test]
    fn non_move_events_never_request() {
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let wheel = Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        });
        assert!(!should_request_redraw(&press, true, true));
        assert!(!should_request_redraw(&release, true, true));
        assert!(!should_request_redraw(&wheel, true, true));
    }

    /// Only position news may rewrite the remembered flag. A press that lands off
    /// the content must not erase what the last move established, or the next move
    /// would mistake a leave for "was never here".
    #[test]
    fn only_position_events_refresh_the_remembered_flag() {
        assert!(tracks_position(&moved()));
        assert!(tracks_position(&finger_moved()));
        assert!(tracks_position(&Event::Mouse(mouse::Event::CursorLeft)));
        assert!(!tracks_position(&Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left
        ))));
        assert!(!tracks_position(&Event::Mouse(
            mouse::Event::CursorEntered
        )));
    }
}
