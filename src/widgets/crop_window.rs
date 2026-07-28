//! A fixed-size VIEWPORT onto a larger child, offset and clipped (DRAGON-385).
//!
//! The preview editor's crop tool is NON-destructive: the committed crop is a rectangle over the
//! full source image, applied only at bake. Once accepted, the editor must nonetheless FRAME the
//! cropped region — "a crop to the bottom right shows only the bottom right" — while every pixel
//! layer (the base image, the real-time effect shader passes, the covermark) keeps rendering the
//! WHOLE frame exactly as it always has. This widget is the seam that reconciles the two: it draws
//! its child (the full-frame media stack, sized to the whole picture at the crop's on-screen
//! scale) TRANSLATED so the crop's top-left lands at this widget's origin, and CLIPPED to its own
//! (crop-sized) bounds. Out-of-source area (a crop dragged past the image edge) shows opaque BLACK,
//! matching the bake's black fill.
//!
//! It is DRAW-ONLY in spirit: the child is `widget::image` + shader stacks that never handle a
//! pointer event, so this only has to place, clip and forward. The annotation interaction (which
//! DOES map pointer coordinates) lives one layer up in [`crate::widgets::annotation_canvas`], whose
//! [`CanvasMap`](crate::widgets::annotation_canvas::CanvasMap) carries the SAME crop offset — the
//! two agree by construction, since both frame the identical crop region into the identical box.
//!
//! Inserted ONLY when a crop is applied; an un-cropped preview never wraps its media in one, so the
//! default render path stays byte-identical.

use cosmic::iced::core::widget::{tree, Operation, Tree};
use cosmic::iced::core::{
    layout, mouse, overlay, renderer, Clipboard, Color, Event, Layout, Length, Point, Rectangle,
    Shell, Size, Vector,
};
use cosmic::iced::advanced::Renderer as _;
use cosmic::widget::Widget;

/// A viewport onto `content`, showing only a `window`-sized region of it offset by `offset`.
pub struct CropWindow<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
    /// This widget's own on-screen size (screen px) = the crop's fitted display box.
    window: (f32, f32),
    /// The child's full on-screen size (screen px) = the whole frame at the crop's scale.
    content_size: (f32, f32),
    /// The translation (screen px) applied to the child before clipping — the NEGATED crop
    /// origin in screen px, so the crop's top-left aligns with this widget's origin.
    offset: (f32, f32),
    /// Paint opaque black behind the child, so a crop that extends past the source shows black
    /// (matching the bake) rather than the surface behind it.
    backfill: bool,
}

impl<'a, Msg> CropWindow<'a, Msg> {
    /// `window` is this widget's size; `content_size` the child's full size; `offset` the
    /// translation applied to the child (the negated crop origin, in screen px).
    pub fn new(
        content: impl Into<cosmic::Element<'a, Msg>>,
        window: (f32, f32),
        content_size: (f32, f32),
        offset: (f32, f32),
    ) -> Self {
        Self { content: content.into(), window, content_size, offset, backfill: true }
    }

    /// The child's layout (offset within this widget's node). Static so it never borrows `self`
    /// alongside `self.content.as_widget_mut()`.
    fn child_layout(layout: Layout<'_>) -> Layout<'_> {
        layout.children().next().unwrap_or(layout)
    }
}

impl<'a, Msg: Clone + 'a> Widget<Msg, cosmic::Theme, cosmic::Renderer> for CropWindow<'a, Msg> {
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&mut self, tree: &mut Tree) {
        self.content.as_widget_mut().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.window.0), Length::Fixed(self.window.1))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &cosmic::Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        // Lay the child out at its FULL size, then position it (negatively) so the crop region
        // sits at this widget's origin. This widget's own node is the (smaller) crop window.
        let child_limits =
            layout::Limits::new(Size::ZERO, Size::new(self.content_size.0, self.content_size.1));
        let child = self
            .content
            .as_widget_mut()
            .layout(tree, renderer, &child_limits)
            .move_to(Point::new(self.offset.0, self.offset.1));
        layout::Node::with_children(Size::new(self.window.0, self.window.1), vec![child])
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
            .operate(tree, Self::child_layout(layout), renderer, operation);
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
        let child = Self::child_layout(layout);
        self.content
            .as_widget_mut()
            .update(tree, event, child, cursor, renderer, clipboard, shell, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, Self::child_layout(layout), cursor, viewport, renderer)
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
        let bounds = layout.bounds();
        // Clip to the crop window, but never wider than the viewport this widget was handed (the
        // ZoomPan's content rect): iced's nested `push_clip` does NOT intersect with the parent
        // clip, so when the crop is zoomed IN — its on-screen box exceeding the content rect — a
        // bare `with_layer(bounds)` would let the picture bleed over the scrollbars. Intersecting
        // keeps it inside both. The child's own `with_layer(viewport)` passes (the shader effect /
        // covermark passes) scissor to the viewport we hand them, so they clip here too.
        let clip = bounds.intersection(viewport).unwrap_or(bounds);
        renderer.with_layer(clip, |renderer| {
            if self.backfill {
                renderer.fill_quad(
                    renderer::Quad { bounds, ..Default::default() },
                    Color::BLACK,
                );
            }
            self.content.as_widget().draw(
                tree,
                renderer,
                theme,
                style,
                Self::child_layout(layout),
                cursor,
                &clip,
            );
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Msg, cosmic::Theme, cosmic::Renderer>> {
        let child = Self::child_layout(layout);
        self.content
            .as_widget_mut()
            .overlay(tree, child, renderer, viewport, translation)
    }
}

impl<'a, Msg: Clone + 'a> From<CropWindow<'a, Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: CropWindow<'a, Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}
