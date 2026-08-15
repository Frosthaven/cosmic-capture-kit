//! A wrapper widget that places its content at an ABSOLUTE point inside the surface,
//! including points outside it (DRAGON-682 item 41).
//!
//! # Why this exists
//!
//! The house trick for absolute placement is a `Fill` container with leading PADDING (the
//! colour picker's own `view::absolute`, the preview's letterboxed backdrop, the capture
//! overlay's cursor indicator). It is two lines and it is right for everything that lives
//! inside the window.
//!
//! It cannot express a NEGATIVE origin, because padding is a size and sizes do not go below
//! zero. The colour picker's drag ghost has to: it follows the pointer, the pointer keeps
//! reporting past the frame while a button is held (the platform's implicit grab), and the
//! owner's report was exactly this, "we can't drag a swatch above the window or beyond the
//! left edge, the swatch clamps". Clamping at two edges and clipping at the other two is
//! also inconsistent for no reason a user could infer.
//!
//! So this positions by LAYOUT instead: the child is laid out at its own size and moved to
//! the requested point, which the renderer is free to place partly or wholly off the
//! surface. Clipping is then the surface's job, at every edge, in the same way.
//!
//! It takes the whole space it is given, so it belongs in a `stack` layer of its own where it
//! covers the window without displacing anything.

use cosmic::iced::core::widget::{Operation, Tree};
use cosmic::iced::core::{
    Clipboard, Event, Layout, Length, Point, Rectangle, Shell, Size, layout, mouse, overlay,
    renderer,
};
use cosmic::widget::Widget;

pub struct Positioned<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
    at: (f32, f32),
}

/// Place `content` with its top-left corner at `at`, in the coordinates of whatever layer
/// this sits in. Negative coordinates are honoured, not clamped.
pub fn positioned<'a, Msg: 'a>(
    content: impl Into<cosmic::Element<'a, Msg>>,
    at: (f32, f32),
) -> cosmic::Element<'a, Msg> {
    cosmic::Element::new(Positioned {
        content: content.into(),
        at,
    })
}

impl<'a, Msg> Widget<Msg, cosmic::Theme, cosmic::Renderer> for Positioned<'a, Msg> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // The child is measured against a FRESH limit rather than the remaining space, so
        // where it is placed cannot change how big it is. Then it is moved, and a move to a
        // negative point is as legal as any other.
        let child = self.content.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::NONE,
        );
        let child = child.move_to(Point::new(self.at.0, self.at.1));
        layout::Node::with_children(limits.max(), vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn Operation<()>,
    ) {
        if let Some(child) = layout.children().next() {
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], child, renderer, operation);
        }
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
        if let Some(child) = layout.children().next() {
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                child,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
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
        if let Some(child) = layout.children().next() {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                child,
                cursor,
                viewport,
            );
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: cosmic::iced::core::Vector,
    ) -> Option<overlay::Element<'b, Msg, cosmic::Theme, cosmic::Renderer>> {
        let child = layout.children().next()?;
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], child, renderer, viewport, translation)
    }
}
