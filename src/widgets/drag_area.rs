//! A wrapper widget that makes its content draggable from anywhere on it —
//! including directly over buttons.
//!
//! It fully intercepts pointer input over its bounds: a press that moves past a
//! small threshold becomes a drag (emitting `on_pan(dx, dy)` deltas in surface
//! logical px); a press+release without movement is a tap, which it replays to
//! the content as a clean press+release so inner buttons still fire (and never
//! get left in a half-pressed state). Capturing the press also keeps a drag that
//! starts on the toolbar from drawing a region on the selector beneath it.

use cosmic::iced::core::widget::{Operation, Tree, tree};
use cosmic::iced::core::{
    Clipboard, Event, Layout, Length, Point, Rectangle, Shell, Size, layout, mouse, overlay,
    renderer,
};
use cosmic::widget::Widget;

const DRAG_THRESHOLD: f32 = 4.0; // px of movement before a press becomes a drag

#[derive(Default)]
struct State {
    /// Surface-local point where the press began (None when not pressed).
    press: Option<Point>,
    dragging: bool,
    /// Last cursor position seen while dragging.
    last: Point,
}

pub struct DragArea<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
    on_pan: Box<dyn Fn(f32, f32) -> Msg + 'a>,
    on_drag_end: Option<Msg>,
}

impl<'a, Msg> DragArea<'a, Msg> {
    pub fn new(
        content: impl Into<cosmic::Element<'a, Msg>>,
        on_pan: impl Fn(f32, f32) -> Msg + 'a,
    ) -> Self {
        Self {
            content: content.into(),
            on_pan: Box::new(on_pan),
            on_drag_end: None,
        }
    }

    /// Message emitted when a drag finishes (the pointer is released after
    /// actually moving) — e.g. to re-sync a click-through input region to the
    /// content's new position.
    pub fn on_drag_end(mut self, msg: Msg) -> Self {
        self.on_drag_end = Some(msg);
        self
    }
}

impl<'a, Msg: Clone + 'a> Widget<Msg, cosmic::Theme, cosmic::Renderer> for DragArea<'a, Msg> {
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
        let bounds = layout.bounds();
        let st = tree.state.downcast_mut::<State>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = cursor.position_over(bounds) {
                    st.press = Some(p);
                    st.dragging = false;
                    st.last = p;
                    // Hold the press: decide on release whether it was a tap or a
                    // drag (and keep the selector beneath from reacting).
                    shell.capture_event();
                    return;
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if st.press.is_some() => {
                if let Some(p) = cursor.position() {
                    let press = st.press.unwrap();
                    if !st.dragging
                        && (p.x - press.x).hypot(p.y - press.y) > DRAG_THRESHOLD
                    {
                        st.dragging = true;
                    }
                    if st.dragging {
                        let (dx, dy) = (p.x - st.last.x, p.y - st.last.y);
                        st.last = p;
                        shell.publish((self.on_pan)(dx, dy));
                        shell.capture_event();
                        return;
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if st.press.is_some() =>
            {
                let was_drag = st.dragging;
                st.press = None;
                st.dragging = false;
                if was_drag {
                    if let Some(m) = &self.on_drag_end {
                        shell.publish(m.clone());
                    }
                    shell.capture_event();
                    return;
                }
                // Tap: replay press+release to the content so a button fires once,
                // cleanly (it never saw the held-back press). The synthetic press
                // is fed through a throwaway shell — a pressed button captures the
                // event, and that capture flag would otherwise make the button skip
                // the following release (it bails on `is_event_captured`).
                let press_ev =
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
                let mut press_msgs: Vec<Msg> = Vec::new();
                {
                    let mut press_shell = Shell::new(&mut press_msgs);
                    self.content.as_widget_mut().update(
                        &mut tree.children[0],
                        &press_ev,
                        layout,
                        cursor,
                        renderer,
                        clipboard,
                        &mut press_shell,
                        viewport,
                    );
                }
                for m in press_msgs {
                    shell.publish(m);
                }
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
                return;
            }
            _ => {}
        }
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
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        let inner = self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        );
        // Over the toolbar but not over an interactive child → hint it's grabbable.
        if inner == mouse::Interaction::None && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Grab
        } else {
            inner
        }
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
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Msg: Clone + 'a> From<DragArea<'a, Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: DragArea<'a, Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}
