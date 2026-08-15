//! A wrapper widget that FORCES one mouse cursor for its content (DRAGON-682 item 40).
//!
//! The twin of [`crate::widgets::arrow_cursor`], and the opposite move. That one MAPS a
//! result (the hand becomes the arrow) and passes everything else through, which is right
//! for a house style. This one OVERRIDES: while the pointer is over its bounds, its
//! interaction is the answer, whatever the content underneath would have said.
//!
//! It exists because both halves of the colour picker's drag needed a cursor the content
//! would not give:
//!
//! * a drag SOURCE shows the open GRAB hand on hover, and two of the three sources are (or
//!   wrap) a cosmic button, which reports `Pointer`, which `arrow_cursor` then turns into
//!   `Idle`. `mouse_area`'s own `.interaction()` cannot help: it only applies when the
//!   content reports `Interaction::None`, so anything with a real answer wins over it;
//! * a LIVE drag shows the closed GRABBING hand EVERYWHERE, including over the value row's
//!   text inputs (an I-beam) and every button in the window. Only an override at the root
//!   can do that, and only an override that ignores its content.
//!
//! Overlays are passed through untouched, unlike `arrow_cursor`'s: a menu is not a drag
//! source, and no menu can be open while a drag is live.

use cosmic::iced::core::widget::{Operation, Tree};
use cosmic::iced::core::{
    Clipboard, Event, Layout, Length, Rectangle, Shell, Size, layout, mouse, overlay, renderer,
};
use cosmic::widget::Widget;

pub struct ForceCursor<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
    /// `None` is a pure pass-through: the content's own interaction stands. It exists so
    /// a caller whose override is CONDITIONAL (the picker root forces the grabbing hand
    /// only while a drag is live) can keep this wrapper in the tree permanently and vary
    /// only the value: swapping the wrapper in and out re-shapes the widget tree, and
    /// iced's positional diff then mis-aligns every stateful descendant, which is
    /// exactly the scroll-reset bug DRAGON-687's drag-jump round traced (see
    /// `color_picker_window_view`'s layer block).
    interaction: Option<mouse::Interaction>,
}

/// Wrap `content` so the pointer over it always shows `interaction`.
pub fn force_cursor<'a, Msg: 'a>(
    content: impl Into<cosmic::Element<'a, Msg>>,
    interaction: mouse::Interaction,
) -> cosmic::Element<'a, Msg> {
    cosmic::Element::new(ForceCursor {
        content: content.into(),
        interaction: Some(interaction),
    })
}

/// [`force_cursor`] with the override OPTIONAL: `None` changes nothing about the
/// cursor, and everything about tree stability (the wrapper stays put; see the field
/// doc). For a caller whose condition flips at runtime, this is the only correct form.
pub fn force_cursor_maybe<'a, Msg: 'a>(
    content: impl Into<cosmic::Element<'a, Msg>>,
    interaction: Option<mouse::Interaction>,
) -> cosmic::Element<'a, Msg> {
    cosmic::Element::new(ForceCursor {
        content: content.into(),
        interaction,
    })
}

impl<'a, Msg> Widget<Msg, cosmic::Theme, cosmic::Renderer> for ForceCursor<'a, Msg> {
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
        // A pure pass-through for EVENTS: this widget changes what the pointer looks like
        // and nothing about what it does.
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
        if let Some(forced) = self.interaction
            && cursor.is_over(layout.bounds())
        {
            return forced;
        }
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
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}
