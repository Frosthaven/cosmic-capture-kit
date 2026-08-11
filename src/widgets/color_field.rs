//! A draggable GRADIENT FIELD (DRAGON-630): caller-supplied gradient content (usually a
//! pre-rastered image) under a position marker, reporting NORMALIZED coordinates on
//! press and on drag.
//!
//! Deliberately REUSABLE, and that is a requirement rather than a nicety (the owner's
//! instruction: these will be mounted elsewhere later). The widget knows nothing about
//! colour. The colour picker window mounts three of them — the saturation/value square,
//! the hue strip and the alpha strip — and a future tenant (an annotation colour editor
//! is the obvious one) mounts the same type over a different raster. Keep it that way:
//! the content is a finished `Element`, the marker is a `0..1` pair, and the answer is a
//! `0..1` pair; what the gradient MEANS belongs entirely to the caller.
//!
//! # Shape
//!
//! * The CONTENT draws the gradient. It is a child element, not a raster this widget
//!   builds, for the same reason `preview::layers` and the magnifier build their pixels
//!   in the update handler: minting image handles per frame churns the texture atlas,
//!   so the caller builds one handle when its inputs change and the view re-uses it.
//! * The MARKER is drawn by this widget, because it moves on every drag and must not
//!   cost a re-raster. Its LOOK is the caller's ([`MarkerStyle`], a theme closure),
//!   because "what reads well here" depends on what the gradient means: the picker's
//!   strips wear a window-background thumb with a subdued border, its square wears a
//!   hollow ring whose ink flips with the colour under it. The widget stays
//!   colour-blind either way.
//! * The marker is drawn in its OWN LAYER (`renderer.with_layer`), and that is a
//!   correctness requirement, not compositing taste: within one iced layer every QUAD
//!   is drawn before every IMAGE, whatever order the widget issues them in, so a marker
//!   quad painted "after" the gradient image still lands underneath it and is simply
//!   invisible. The first version of this widget shipped exactly that bug.
//! * Dragging CAPTURES the pointer, and the position keeps reporting while the pointer
//!   is outside the bounds, clamped, exactly like every real slider: a drag that
//!   wanders off the strip must not stop answering.

use cosmic::iced::advanced::Renderer as _;
use cosmic::iced::core::renderer::Quad;
use cosmic::iced::core::widget::{Operation, Tree, tree};
use cosmic::iced::core::{
    Background, Border, Clipboard, Color, Event, Layout, Length, Point, Rectangle, Shadow, Shell,
    Size, Vector, layout, mouse, overlay, renderer, touch,
};
use cosmic::widget::Widget;

/// Which way the marker travels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldAxis {
    /// A 2D field: the marker follows the pointer on both axes (the SV square).
    Both,
    /// A horizontal strip: only X moves, and the marker rides the vertical centre.
    X,
}

/// How the marker is painted, resolved from the live theme per draw
/// ([`ColorField::marker_style`] takes a closure) so a caller can key it off theme
/// tokens, or off the colour currently under the marker, without this widget learning
/// what either means.
#[derive(Clone, Copy, Debug)]
pub struct MarkerStyle {
    /// The circle's fill. `None` leaves the gradient visible through the ring, which is
    /// what a position marker over a 2D field wants; a slider thumb usually fills.
    pub fill: Option<Color>,
    /// The ring's ink.
    pub border: Color,
    pub border_width: f32,
    /// The quad's drop shadow; `Shadow::default()` (transparent) is none.
    pub shadow: Shadow,
}

impl Default for MarkerStyle {
    /// A hollow white ring over a soft shadow: legible on any gradient with no theme
    /// knowledge, which is all a DEFAULT can promise. Real tenants restyle it.
    fn default() -> Self {
        Self {
            fill: None,
            border: Color::WHITE,
            border_width: 2.0,
            shadow: Shadow {
                color: Color { a: 0.5, ..Color::BLACK },
                offset: Vector::new(0.0, 1.0),
                blur_radius: 3.0,
            },
        }
    }
}

/// Pure, unit-tested: the normalized position of point `p` inside a field `w` x `h`
/// points across, with `p` measured from the field's own origin. Clamped into `0..=1`
/// per axis, so a drag that leaves the field keeps answering the nearest edge, and a
/// degenerate extent answers `0.0` rather than NaN.
pub fn field_position(p: (f32, f32), size: (f32, f32)) -> (f32, f32) {
    let axis = |v: f32, extent: f32| -> f32 {
        if !v.is_finite() || !extent.is_finite() || extent <= 0.0 {
            return 0.0;
        }
        (v / extent).clamp(0.0, 1.0)
    };
    (axis(p.0, size.0), axis(p.1, size.1))
}

/// Pure, unit-tested: where the marker's CENTRE sits, in field-local points, for a
/// normalized `marker` on a field `size` across. [`FieldAxis::X`] pins the vertical
/// centre, which is what makes a strip's marker ride its middle whatever the caller
/// last reported for y.
pub fn marker_centre(marker: (f32, f32), axis: FieldAxis, size: (f32, f32)) -> (f32, f32) {
    let clamp01 = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let x = clamp01(marker.0) * size.0;
    let y = match axis {
        FieldAxis::Both => clamp01(marker.1) * size.1,
        FieldAxis::X => size.1 / 2.0,
    };
    (x, y)
}

/// Pure, unit-tested: is field-local point `p` inside the marker's own circle?
///
/// This is what makes the whole THUMB grabbable (the owner's report): the strips'
/// marker is larger than the strip, so its overhang lies OUTSIDE the widget's bounds,
/// and a press there used to fall through because the hit test was the content
/// rectangle alone. A press now starts a drag when it lands on the field OR anywhere on
/// the marker, whichever is true. The pixel of slack past the radius forgives the
/// anti-aliased rim.
pub fn within_marker(
    p: (f32, f32),
    marker: (f32, f32),
    axis: FieldAxis,
    size: (f32, f32),
    diameter: f32,
) -> bool {
    let (mx, my) = marker_centre(marker, axis, size);
    let (dx, dy) = (p.0 - mx, p.1 - my);
    (dx * dx + dy * dy).sqrt() <= diameter / 2.0 + 1.0
}

#[derive(Default)]
struct State {
    dragging: bool,
}

pub struct ColorField<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
    /// The marker's position, normalized `0..1` per axis.
    marker: (f32, f32),
    axis: FieldAxis,
    /// The ring's diameter in points. Defaults suit a fine 2D field; strips usually ask
    /// for something nearer their own height.
    marker_diameter: f32,
    marker_style: Box<dyn Fn(&cosmic::Theme) -> MarkerStyle + 'a>,
    on_change: Box<dyn Fn(f32, f32) -> Msg + 'a>,
}

impl<'a, Msg> ColorField<'a, Msg> {
    pub fn new(
        content: impl Into<cosmic::Element<'a, Msg>>,
        marker: (f32, f32),
        axis: FieldAxis,
        on_change: impl Fn(f32, f32) -> Msg + 'a,
    ) -> Self {
        Self {
            content: content.into(),
            marker,
            axis,
            marker_diameter: 14.0,
            marker_style: Box::new(|_| MarkerStyle::default()),
            on_change: Box::new(on_change),
        }
    }

    /// Override the marker ring's diameter (points).
    pub fn marker_diameter(mut self, d: f32) -> Self {
        self.marker_diameter = d.max(4.0);
        self
    }

    /// Override how the marker is painted, from the live theme (see [`MarkerStyle`]).
    pub fn marker_style(
        mut self,
        style: impl Fn(&cosmic::Theme) -> MarkerStyle + 'a,
    ) -> Self {
        self.marker_style = Box::new(style);
        self
    }
}

impl<'a, Msg: 'a> Widget<Msg, cosmic::Theme, cosmic::Renderer> for ColorField<'a, Msg> {
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
        let bounds = layout.bounds();
        let st = tree.state.downcast_mut::<State>();
        // One report for both the press and every drag step: the field-local point,
        // clamped by `field_position`, so a drag past the edge answers the edge.
        let report = |p: Point, shell: &mut Shell<'_, Msg>| {
            let (nx, ny) =
                field_position((p.x - bounds.x, p.y - bounds.y), (bounds.width, bounds.height));
            shell.publish((self.on_change)(nx, ny));
        };
        // A press starts a drag when it lands on the FIELD or anywhere on the MARKER:
        // the marker overhangs the field (a strip's thumb is taller than the strip, and
        // an edge position hangs it past the ends), and grabbing a visible thumb must
        // work wherever it is visible (the owner's report; the field-only test made the
        // overhang dead).
        let grabs = |p: Point| {
            bounds.contains(p)
                || within_marker(
                    (p.x - bounds.x, p.y - bounds.y),
                    self.marker,
                    self.axis,
                    (bounds.width, bounds.height),
                    self.marker_diameter,
                )
        };
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = cursor.position().filter(|p| grabs(*p)) {
                    st.dragging = true;
                    report(p, shell);
                    shell.capture_event();
                }
            }
            Event::Touch(touch::Event::FingerPressed { position, .. })
                if grabs(*position) =>
            {
                st.dragging = true;
                report(*position, shell);
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if st.dragging => {
                if let Some(p) = cursor.position() {
                    report(p, shell);
                    shell.capture_event();
                }
            }
            Event::Touch(touch::Event::FingerMoved { position, .. }) if st.dragging => {
                report(*position, shell);
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. })
            | Event::Touch(touch::Event::FingerLost { .. }) => {
                st.dragging = false;
            }
            _ => {}
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
        // The DEFAULT arrow, everywhere (the owner's call; a crosshair briefly stood on
        // the 2D field). Sliders here show the stock arrow, and so does this.
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
        let bounds = layout.bounds();
        let (cx, cy) = marker_centre(self.marker, self.axis, (bounds.width, bounds.height));
        let (cx, cy) = (bounds.x + cx, bounds.y + cy);
        let style = (self.marker_style)(theme);
        let d = self.marker_diameter;
        let quad = Quad {
            bounds: Rectangle { x: cx - d / 2.0, y: cy - d / 2.0, width: d, height: d },
            border: Border {
                radius: (d / 2.0).into(),
                width: style.border_width,
                color: style.border,
            },
            shadow: style.shadow,
            ..Quad::default()
        };
        // The marker's OWN LAYER, which is what puts it in front of the gradient at
        // all: within one iced layer every quad is drawn before every image, whatever
        // order the widget issues them in, so a quad in the content's layer sits UNDER
        // the image and is invisible (this widget shipped that bug once). The clip is a
        // diameter around the ring, room enough for the shadow's blur.
        let clip = Rectangle {
            x: cx - d,
            y: cy - d,
            width: d * 2.0,
            height: d * 2.0,
        };
        renderer.with_layer(clip, |renderer| {
            renderer.fill_quad(
                quad,
                Background::Color(style.fill.unwrap_or(Color::TRANSPARENT)),
            );
        });
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

impl<'a, Msg: 'a> From<ColorField<'a, Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: ColorField<'a, Msg>) -> Self {
        cosmic::Element::new(w)
    }
}

/// DRAGON-630: the two pure placements. What is pinned is the slider contract — clamped
/// reporting, edge reachability, and a strip's marker riding its own centre line.
#[cfg(test)]
mod field_geometry_tests {
    use super::*;

    /// The corners and the middle answer exactly, so the caller's value mapping needs
    /// no correction terms.
    #[test]
    fn positions_map_linearly_and_the_corners_are_exact() {
        let size = (200.0, 100.0);
        assert_eq!(field_position((0.0, 0.0), size), (0.0, 0.0));
        assert_eq!(field_position((200.0, 100.0), size), (1.0, 1.0));
        assert_eq!(field_position((100.0, 50.0), size), (0.5, 0.5));
    }

    /// A drag that leaves the field keeps answering the nearest edge: the slider
    /// contract, and the reason dragging captures the pointer.
    #[test]
    fn outside_clamps_to_the_edge() {
        let size = (200.0, 100.0);
        assert_eq!(field_position((-30.0, 50.0), size), (0.0, 0.5));
        assert_eq!(field_position((900.0, -5.0), size), (1.0, 0.0));
    }

    /// Degenerate geometry answers zero, never NaN: a zero-sized field can appear for a
    /// frame while a window is opening.
    #[test]
    fn degenerate_fields_answer_zero() {
        assert_eq!(field_position((10.0, 10.0), (0.0, 0.0)), (0.0, 0.0));
        assert_eq!(field_position((f32::NAN, 10.0), (100.0, 100.0)).0, 0.0);
    }

    /// The strip's marker rides the centre line whatever y the caller reports, which is
    /// what lets one state field serve both a square and its strips.
    #[test]
    fn a_strip_marker_rides_the_centre() {
        assert_eq!(marker_centre((0.25, 0.9), FieldAxis::X, (200.0, 20.0)), (50.0, 10.0));
        assert_eq!(
            marker_centre((0.25, 0.9), FieldAxis::Both, (200.0, 20.0)),
            (50.0, 18.0)
        );
    }

    /// An out-of-range or unfinished marker clamps rather than drawing off the field.
    #[test]
    fn the_marker_clamps_into_the_field() {
        assert_eq!(marker_centre((2.0, -1.0), FieldAxis::Both, (100.0, 50.0)), (100.0, 0.0));
        assert_eq!(marker_centre((f32::NAN, 0.5), FieldAxis::Both, (100.0, 50.0)).0, 0.0);
    }

    /// The owner's report: a strip's thumb is BIGGER than the strip, and the overhang
    /// must be grabbable. A point above the strip but inside the thumb counts; the same
    /// point counts from the horizontal overhang at an end position; a point past the
    /// thumb does not.
    #[test]
    fn the_marker_is_grabbable_across_its_whole_circle() {
        // A 200x20 strip with a 24pt thumb at the middle: 2pt of overhang each side.
        let (size, d) = ((200.0, 20.0), 24.0);
        assert!(
            within_marker((100.0, -1.5), (0.5, 0.5), FieldAxis::X, size, d),
            "the top overhang grabs"
        );
        assert!(
            within_marker((-8.0, 10.0), (0.0, 0.5), FieldAxis::X, size, d),
            "the left overhang at t=0 grabs"
        );
        assert!(
            !within_marker((100.0, -20.0), (0.5, 0.5), FieldAxis::X, size, d),
            "well past the thumb does not"
        );
        assert!(
            !within_marker((0.0, 10.0), (1.0, 0.5), FieldAxis::X, size, d),
            "the far end's thumb does not grab from the near end"
        );
        // The 2D field's ring too (the owner asked for it by name): a ring parked in
        // the square's corner overhangs two edges at once, and both overhangs grab.
        let sq = (400.0, 180.0);
        assert!(
            within_marker((-5.0, -5.0), (0.0, 0.0), FieldAxis::Both, sq, 14.0),
            "the corner overhang grabs on the square"
        );
        assert!(
            !within_marker((30.0, 30.0), (0.0, 0.0), FieldAxis::Both, sq, 14.0),
            "inside the square but off the ring is the field's own hit, not the marker's"
        );
    }
}
