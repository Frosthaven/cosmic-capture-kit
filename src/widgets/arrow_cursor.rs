//! A wrapper widget that suppresses the hand ("pointer") cursor for its content,
//! leaving the plain arrow instead.
//!
//! Cosmic's interactive widgets (buttons, togglers, dropdowns, segmented controls,
//! …) report [`mouse::Interaction::Pointer`] on hover, which draws the hand cursor.
//! The house style for the toolbar / settings / preview surfaces is that ONLY real
//! URL links get the hand; every other control uses the arrow. Buttons expose
//! `.interaction(Idle)` to opt out directly, but togglers/dropdowns/segmented do
//! not — wrap those in [`arrow_cursor`].
//!
//! It is a pure pass-through: every `Widget` method delegates to the content
//! unchanged EXCEPT [`mouse_interaction`](Widget::mouse_interaction), which maps a
//! `Pointer` result to `None` (arrow) and passes every other interaction (text
//! I-beam, resize, grab, …) through untouched.

use cosmic::iced::core::widget::{Operation, Tree};
use cosmic::iced::core::{
    Clipboard, Event, Layout, Length, Rectangle, Shell, Size, layout, mouse, overlay, renderer,
};
use cosmic::widget::Widget;

pub struct ArrowCursor<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
}

/// Wrap `content` so it never shows the hand cursor (see the module docs).
pub fn arrow_cursor<'a, Msg: 'a>(
    content: impl Into<cosmic::Element<'a, Msg>>,
) -> cosmic::Element<'a, Msg> {
    cosmic::Element::new(ArrowCursor {
        content: content.into(),
    })
}

impl<'a, Msg> Widget<Msg, cosmic::Theme, cosmic::Renderer> for ArrowCursor<'a, Msg> {
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
        // The whole point: the hand becomes the arrow; everything else is untouched.
        if inner == mouse::Interaction::Pointer {
            mouse::Interaction::None
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
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
            // Wrap the popup overlay (e.g. a dropdown's open menu) so ITS items show the
            // arrow too — the widget's own `mouse_interaction` above doesn't cover the overlay.
            .map(|inner| overlay::Element::new(Box::new(ArrowOverlay { inner })))
    }
}

/// Overlay twin of [`ArrowCursor`] for popup layers (a dropdown's open menu, etc.):
/// remaps a `Pointer` interaction to the plain arrow, passes everything else through, and
/// recurses into any nested overlay.
struct ArrowOverlay<'a, Msg> {
    inner: overlay::Element<'a, Msg, cosmic::Theme, cosmic::Renderer>,
}

impl<Msg> cosmic::iced::core::Overlay<Msg, cosmic::Theme, cosmic::Renderer>
    for ArrowOverlay<'_, Msg>
{
    fn layout(&mut self, renderer: &cosmic::Renderer, bounds: Size) -> layout::Node {
        self.inner.as_overlay_mut().layout(renderer, bounds)
    }

    fn draw(
        &self,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.inner
            .as_overlay()
            .draw(renderer, theme, style, layout, cursor);
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn Operation<()>,
    ) {
        self.inner
            .as_overlay_mut()
            .operate(layout, renderer, operation);
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
    ) {
        self.inner
            .as_overlay_mut()
            .update(event, layout, cursor, renderer, clipboard, shell);
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        let inner = self
            .inner
            .as_overlay()
            .mouse_interaction(layout, cursor, renderer);
        if inner == mouse::Interaction::Pointer {
            mouse::Interaction::None
        } else {
            inner
        }
    }

    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &cosmic::Renderer,
    ) -> Option<overlay::Element<'c, Msg, cosmic::Theme, cosmic::Renderer>> {
        self.inner
            .as_overlay_mut()
            .overlay(layout, renderer)
            .map(|inner| overlay::Element::new(Box::new(ArrowOverlay { inner })))
    }

    fn index(&self) -> f32 {
        self.inner.as_overlay().index()
    }
}

impl<'a, Msg: 'a> From<ArrowCursor<'a, Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: ArrowCursor<'a, Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}
