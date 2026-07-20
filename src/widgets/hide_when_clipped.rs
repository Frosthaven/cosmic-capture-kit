//! A wrapper widget that hides its whole child while the child straddles the
//! current draw `viewport` (the clipped scroll region).
//!
//! Why: iced's `text_input` clips its value glyphs to its own box bounds, not to
//! the scroll viewport, and iced's clip stack REPLACES rather than intersects.
//! So when an input scrolls partway under a pinned tab strip, its background
//! clips fine but its glyph layer leaks OVER the tabs. We cannot fix that inside
//! `text_input` without forking libcosmic. Instead we wrap the input: `draw` is
//! gated so the child is drawn only when its layout bounds are FULLY inside the
//! propagated `viewport` (which the scrollable sets to the visible/clipped region
//! — see `iced/widget/src/scrollable.rs` `with_layer(visible_bounds, ..)` then
//! `content.draw(.., &Rectangle { .. visible_bounds })`). As the input crosses
//! the clip edge it disappears cleanly instead of leaking text.
//!
//! Every other `Widget` method delegates to the child unchanged, so focus,
//! typing, cursor and interaction are completely unaffected — only `draw` is
//! gated, making this zero-overhead when the child is fully visible.

use cosmic::iced::core::widget::{Operation, Tree, tree};
use cosmic::iced::core::{
    Clipboard, Event, Layout, Length, Rectangle, Shell, Size, layout, mouse, overlay, renderer,
};
use cosmic::widget::Widget;

/// Slack (logical px) so a box whose edge sits exactly on the viewport edge — or
/// a sub-pixel rounding hair past it — still counts as fully visible and draws.
const EPSILON: f32 = 0.5;

/// True when `bounds` is fully contained within `viewport` (with a small
/// tolerance). False when `bounds` extends past any viewport edge — i.e. the
/// child is straddling the scroll clip boundary and should be hidden.
fn fully_visible(bounds: Rectangle, viewport: Rectangle) -> bool {
    bounds.x >= viewport.x - EPSILON
        && bounds.y >= viewport.y - EPSILON
        && bounds.x + bounds.width <= viewport.x + viewport.width + EPSILON
        && bounds.y + bounds.height <= viewport.y + viewport.height + EPSILON
}

pub struct HideWhenClipped<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
}

impl<'a, Msg> HideWhenClipped<'a, Msg> {
    pub fn new(content: impl Into<cosmic::Element<'a, Msg>>) -> Self {
        Self {
            content: content.into(),
        }
    }
}

/// Wrap `content` so it hides itself whenever it straddles the scroll viewport.
pub fn hide_when_clipped<'a, Msg: 'a>(
    content: impl Into<cosmic::Element<'a, Msg>>,
) -> cosmic::Element<'a, Msg> {
    cosmic::Element::new(HideWhenClipped::new(content))
}

impl<'a, Msg> Widget<Msg, cosmic::Theme, cosmic::Renderer> for HideWhenClipped<'a, Msg> {
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
        self.content.as_widget_mut().layout(tree, renderer, limits)
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
            .operate(tree, layout, renderer, operation);
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
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
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
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
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
        // The one gate: only draw the child when it is fully inside the clipped
        // viewport. Straddling the scroll edge → draw nothing (no leaked glyphs).
        if fully_visible(layout.bounds(), *viewport) {
            self.content
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
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
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

impl<'a, Msg: 'a> From<HideWhenClipped<'a, Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: HideWhenClipped<'a, Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rectangle {
        Rectangle {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn contained_is_visible() {
        let viewport = rect(0.0, 0.0, 100.0, 100.0);
        assert!(fully_visible(rect(10.0, 10.0, 20.0, 20.0), viewport));
    }

    #[test]
    fn exact_fit_is_visible() {
        let viewport = rect(0.0, 0.0, 100.0, 100.0);
        assert!(fully_visible(rect(0.0, 0.0, 100.0, 100.0), viewport));
    }

    #[test]
    fn straddling_top_edge_is_hidden() {
        // Box scrolled partway up under the pinned tabs: top escapes the viewport.
        let viewport = rect(0.0, 50.0, 100.0, 100.0);
        assert!(!fully_visible(rect(10.0, 40.0, 20.0, 20.0), viewport));
    }

    #[test]
    fn straddling_bottom_edge_is_hidden() {
        let viewport = rect(0.0, 0.0, 100.0, 100.0);
        assert!(!fully_visible(rect(10.0, 90.0, 20.0, 20.0), viewport));
    }

    #[test]
    fn fully_outside_is_hidden() {
        let viewport = rect(0.0, 0.0, 100.0, 100.0);
        assert!(!fully_visible(rect(10.0, 200.0, 20.0, 20.0), viewport));
    }

    #[test]
    fn subpixel_overhang_within_epsilon_still_visible() {
        let viewport = rect(0.0, 0.0, 100.0, 100.0);
        // Extends 0.3px past the right edge — inside EPSILON, so still drawn.
        assert!(fully_visible(rect(0.0, 0.0, 100.3, 100.0), viewport));
    }
}
