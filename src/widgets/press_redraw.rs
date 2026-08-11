//! A wrapper widget that requests a redraw when its content captures a press
//! (DRAGON-648: dropdown menus on Wayland did not appear until the mouse moved).
//!
//! THE DEFECT. libcosmic's `widget::dropdown` opens its menu as an iced overlay
//! driven by internal widget state: the left-press handler flips `state.is_open`
//! and calls `shell.capture_event()`, and that is ALL it does on the non-popup
//! path (libcosmic `src/widget/dropdown/widget.rs`, the `ButtonPressed` arm:
//! `open(...)` + `capture_event()`, no message, no `request_redraw`). iced 0.14
//! renders on demand: after an event batch, the winit loop schedules a frame only
//! for (a) a published message, (b) a changed `mouse::Interaction`, or (c) a
//! widget-requested redraw (`iced_winit` `src/lib.rs`, the block that matches
//! `user_interface::State::Updated { redraw_request, .. }`). The opening press
//! produces none of the three: no message, the cursor is still over the control
//! so the interaction stays `Pointer`, and the widget never asks. On Wayland a
//! surface repaints ONLY when the client requests a frame, so the flipped state
//! sat invisible until the next mouse move happened to redraw. macOS masked the
//! same missing request because AppKit invalidates the view on click-driven
//! window activity; the toolkit path is identical there. The CLOSE half shares
//! the defect: a press on the control (or outside the open menu) flips
//! `is_open` off through the same silent arm, so the painted menu would linger
//! one event too long.
//!
//! Upstream iced's own `PickList` guards exactly this case: it tracks a
//! `last_status` and calls `shell.request_redraw()` when the press changes it
//! (`iced_widget` `src/pick_list.rs`, the status block at the end of `update`).
//! libcosmic's `Dropdown` forked an older pick_list from before that guard and
//! never regained it.
//!
//! WHY THE FIX LIVES HERE. The two-line fix belongs in libcosmic's dropdown
//! press arms, but we do not fork libcosmic (only winit and iced, see
//! FORKED_CHANGES.md), and adopting a whole fork for two lines buys a rebase
//! treadmill. Patching our iced fork cannot help: iced never learns the
//! dropdown's private `is_open` flipped; only the widget knows, so only the
//! widget's side of the `Shell` seam can say "this press changed what the next
//! frame shows". Hence an app-side wrapper at the same seam: it delegates
//! `update` and, when the wrapped subtree turned a press from not-captured to
//! captured, requests one frame. A captured press on a dropdown is exactly its
//! open/close transition, so this is precise, and one extra frame is the whole
//! cost when some future widget under it captures a press for another reason.
//!
//! Considered and rejected:
//! - Folding this into `arrow_cursor`, which already wraps every dropdown: its
//!   contract is "pure pass-through except the cursor", and it also wraps
//!   buttons, sliders and whole toolbars that publish messages on press and need
//!   no help. Two behaviors in one wrapper is how contracts rot.
//! - A subscription-driven redraw tick: a timer to paper over a missing frame
//!   request burns power and still races the press.
//! - The colour picker's hand-built mode chip does NOT need this: its chip is an
//!   ordinary button whose press PUBLISHES `ModeMenuToggled`, and a published
//!   message already schedules the frame (path (a) above).

use cosmic::iced::core::widget::{Operation, Tree};
use cosmic::iced::core::{
    Clipboard, Event, Layout, Length, Rectangle, Shell, Size, layout, mouse, overlay, renderer,
    touch,
};
use cosmic::widget::Widget;

pub struct PressRedraw<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
}

/// Wrap `content` so a press it captures also schedules the frame that shows the
/// result (see the module docs). Wrap the `widget::dropdown` itself, inside
/// `arrow_cursor`, so the capture measurement brackets just the dropdown.
pub fn press_redraw<'a, Msg: 'a>(
    content: impl Into<cosmic::Element<'a, Msg>>,
) -> cosmic::Element<'a, Msg> {
    cosmic::Element::new(PressRedraw {
        content: content.into(),
    })
}

/// Pure, unit-tested: does this event pass warrant a frame request?
///
/// True only when the wrapped subtree turned a press from not-captured to
/// captured. That transition is the dropdown acting on the press (opening its
/// menu, or closing it on a press outside the open menu), which changes what the
/// next frame shows without publishing a message or invalidating layout, so
/// nothing else will schedule that frame. A pass that was captured BEFORE the
/// content ran belongs to some earlier widget; a non-press capture (wheel,
/// keyboard) repaints through its own message or hover machinery.
fn should_request_redraw(was_captured: bool, is_captured: bool, event: &Event) -> bool {
    if was_captured || !is_captured {
        return false;
    }
    matches!(
        event,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. })
    )
}

impl<'a, Msg> Widget<Msg, cosmic::Theme, cosmic::Renderer> for PressRedraw<'a, Msg> {
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
        let was_captured = shell.is_event_captured();
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
        // The whole point: the press that flipped hidden widget state also
        // schedules the frame that shows it.
        if should_request_redraw(was_captured, shell.is_event_captured(), event) {
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
        // Plain forward. The open menu overlay needs no help from us: item
        // selection publishes a message and hover changes already call
        // `request_redraw` in the menu's own List widget.
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, Msg: 'a> From<PressRedraw<'a, Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: PressRedraw<'a, Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}

/// DRAGON-648: the frame request must fire on exactly the capture TRANSITION of
/// a press, nothing wider. Wider (any captured event) turns every keystroke a
/// text input swallows into a frame; narrower (never) is the shipped bug.
#[cfg(test)]
mod press_capture_tests {
    use super::*;
    use cosmic::iced::core::Point;

    fn left_press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }

    fn finger_press() -> Event {
        Event::Touch(touch::Event::FingerPressed {
            id: touch::Finger(0),
            position: Point::ORIGIN,
        })
    }

    /// The shipped bug: the open/close press captures and nothing painted. The
    /// not-captured to captured transition is the signal.
    #[test]
    fn a_press_the_content_captures_requests_a_frame() {
        assert!(should_request_redraw(false, true, &left_press()));
        assert!(should_request_redraw(false, true, &finger_press()));
    }

    /// A pass already captured before the content ran belongs to an earlier
    /// widget; repainting for it is not this wrapper's business.
    #[test]
    fn a_press_captured_by_someone_earlier_is_ignored() {
        assert!(!should_request_redraw(true, true, &left_press()));
    }

    /// A press the content let bubble changed nothing it owns.
    #[test]
    fn an_uncaptured_press_requests_nothing() {
        assert!(!should_request_redraw(false, false, &left_press()));
    }

    /// Non-press captures (wheel steps, swallowed keys) repaint through their
    /// own messages; firing on them would make every such event cost a frame.
    #[test]
    fn non_press_events_never_request() {
        let moved = Event::Mouse(mouse::Event::CursorMoved {
            position: Point::ORIGIN,
        });
        assert!(!should_request_redraw(false, true, &moved));
        let right = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right));
        assert!(!should_request_redraw(false, true, &right));
    }
}
