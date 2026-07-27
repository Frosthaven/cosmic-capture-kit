//! A slider wrapper that draws its breakpoint notches ALIGNED WITH THE THUMB.
//!
//! # The stock-iced bug this exists to work around (DRAGON-343)
//!
//! `iced_widget::slider` (libcosmic rev `4657b6a`, `widget/src/slider.rs`) draws the
//! thumb and a `.breakpoints` tick over **two different spans**:
//!
//! ```text
//! thumb:  x = bounds.x + (bounds.width - handle_width)     * t          // + handle_width/2 centre
//! tick:   x = bounds.x + (bounds.width - BREAKPOINT_WIDTH) * t          // + BREAKPOINT_WIDTH/2 centre
//! ```
//!
//! The thumb has to inset by its own width so it never overhangs the rail; the tick insets
//! by the tick's width (2px) instead. So at rail fraction `t` the tick centre sits
//!
//! ```text
//! (handle_width - BREAKPOINT_WIDTH) * (t - 0.5)
//! ```
//!
//! px away from the thumb centre. The two agree only at `t = 0.5` (or if the thumb were
//! 2px wide). Worse, `handle_width` is not constant: cosmic's slider theme grows the thumb
//! 20 → 26 on hover/drag, so the gap visibly BREATHES as the pointer approaches the
//! control. For the preview editor's zoom slider (rail 120px, 100% sitting at `t = 1/3` of
//! a 50…200% range) that is ~3px adrift at rest and ~4px while dragging — plainly visible,
//! and multiplied by the display scale, which is why HiDPI (macOS Retina, Windows 125/150%)
//! showed it first.
//!
//! **This is an upstream drawing bug, not a scale-factor or app-math bug.** The tick should
//! use the thumb's span. When upstream fixes it (the one-line change is to compute the tick
//! offset over `bounds.width - handle_width` and place it at
//! `bounds.x + offset + handle_width / 2 - BREAKPOINT_WIDTH / 2`), this whole module can be
//! deleted and the caller can go back to `.breakpoints(&[…])`.
//!
//! # What we do instead
//!
//! We do NOT hand `.breakpoints` to the slider at all (so no ticks are drawn by iced,
//! whatever its version does). We wrap the slider and draw the notches ourselves at the
//! thumb's own span, replicating iced's draw metrics exactly — including reading
//! `handle_width` back out of the SAME theme class/status the slider itself resolves, so
//! the hover/drag thumb growth moves our notch in lockstep.
//!
//! Everything else is a pure pass-through: layout, events, cursor and the slider's own
//! detent behaviour are untouched. (The magnetic 100% detent is the app's
//! `preview::viewport::snap_to_hundred`, applied on the message — `.breakpoints` never
//! provided gravitation despite its doc comment; it is draw-only in this iced revision.)

use cosmic::iced::advanced::Renderer as _;
use cosmic::iced::core::renderer::Quad;
use cosmic::iced::core::widget::{Operation, Tree, tree};
use cosmic::iced::core::{
    Background, Border, Clipboard, Color, Event, Layout, Length, Rectangle, Shell, Size, keyboard,
    layout, mouse, overlay, renderer, touch,
};
use cosmic::iced::widget::slider::{Catalog, HandleShape};
use cosmic::style::iced::Slider as SliderClass;
use cosmic::widget::Widget;

/// Tick width, matching iced's `BREAKPOINT_WIDTH`.
pub const BREAKPOINT_WIDTH: f32 = 2.0;
/// Tick height, matching iced's breakpoint quad.
pub const NOTCH_HEIGHT: f32 = 8.0;
/// Gap between the rail centre-line and the top of the tick, matching iced (`rail_y + 6.0`).
pub const NOTCH_GAP: f32 = 6.0;

/// Where `value` sits on the rail, as a 0…1 fraction of `start..=end`. A degenerate range
/// (`start >= end`) pins to 0.0, exactly as iced's draw does.
pub fn rail_fraction(value: f32, start: f32, end: f32) -> f32 {
    if start >= end {
        0.0
    } else {
        ((value - start) / (end - start)).clamp(0.0, 1.0)
    }
}

/// The thumb's width in px for a resolved handle shape — a faithful copy of the sizing
/// clamps at the top of iced's `slider::draw`, so our notch tracks the real thumb (which
/// grows 20 → 26 on hover/drag under cosmic's theme).
pub fn handle_extent(shape: &HandleShape, border_width: f32, bounds_w: f32, bounds_h: f32) -> f32 {
    let border_width = border_width.min(bounds_h / 2.0).min(bounds_w / 2.0);
    match shape {
        HandleShape::Circle { radius } => {
            let radius = radius
                .max(2.0 * border_width)
                .min(bounds_h / 2.0)
                .min(bounds_w / 2.0 + 2.0 * border_width);
            radius * 2.0
        }
        HandleShape::Rectangle { width, .. } => f32::from(*width).max(2.0 * border_width),
    }
}

/// The x of the THUMB's centre at rail fraction `t` — iced's own handle offset plus half a
/// handle. This is the position the user reads as "the value", and therefore the position
/// the notch must sit at.
pub fn thumb_centre_x(bounds_x: f32, bounds_w: f32, handle_w: f32, t: f32) -> f32 {
    bounds_x + (bounds_w - handle_w) * t + handle_w / 2.0
}

/// The notch quad for rail fraction `t`: a `BREAKPOINT_WIDTH`-wide tick CENTRED on the
/// thumb centre, in the band below the rail where iced draws its own breakpoints.
pub fn notch_bounds(bounds: Rectangle, handle_w: f32, t: f32) -> Rectangle {
    let rail_y = bounds.y + bounds.height / 2.0;
    Rectangle {
        x: thumb_centre_x(bounds.x, bounds.width, handle_w, t) - BREAKPOINT_WIDTH / 2.0,
        y: rail_y + NOTCH_GAP,
        width: BREAKPOINT_WIDTH,
        height: NOTCH_HEIGHT,
    }
}

/// Mirrors the private `slider::State` fields we need to resolve the same theme STATUS the
/// slider resolves (its own state is not public, so we track the same two transitions).
#[derive(Default)]
struct NotchState {
    is_dragging: bool,
    modifiers: keyboard::Modifiers,
}

pub struct NotchedSlider<'a, Msg> {
    content: cosmic::Element<'a, Msg>,
    range: (f32, f32),
    notches: Vec<f32>,
    class: SliderClass,
}

/// Wrap a `slider` so `notches` (in slider-VALUE units) are drawn aligned with its thumb.
/// The slider must NOT be given `.breakpoints` — this replaces them.
///
/// `class` MUST be the SAME class the wrapped slider wears: the notch is placed off the thumb's
/// resolved width, so a slider with a custom (e.g. rescaled) thumb must hand its class through
/// or the tick would be aligned to a thumb that isn't there. [`notched_slider`] passes the
/// stock class (`SliderClass::default()`) for a slider that uses the default styling.
pub fn notched_slider<'a, Msg: 'a>(
    content: impl Into<cosmic::Element<'a, Msg>>,
    range: std::ops::RangeInclusive<f32>,
    notches: Vec<f32>,
    class: SliderClass,
) -> cosmic::Element<'a, Msg> {
    cosmic::Element::new(NotchedSlider {
        content: content.into(),
        range: (*range.start(), *range.end()),
        notches,
        class,
    })
}

impl<'a, Msg> Widget<Msg, cosmic::Theme, cosmic::Renderer> for NotchedSlider<'a, Msg> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<NotchState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(NotchState::default())
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
        // Track exactly what the slider tracks, so `draw` can resolve the same theme status
        // (and therefore the same thumb width) it does. Purely observational — we never
        // capture the event.
        let state = tree.state.downcast_mut::<NotchState>();
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers;
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                // A command-click resets to the default instead of dragging (iced's rule).
                state.is_dragging =
                    cursor.position_over(layout.bounds()).is_some() && !state.modifiers.command();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. })
            | Event::Touch(touch::Event::FingerLost { .. }) => state.is_dragging = false,
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
        let bounds = layout.bounds();
        let state = tree.state.downcast_ref::<NotchState>();
        // The status the slider itself will draw with (its `draw` reads a status latched on
        // the RedrawRequested that precedes this frame's draw, so this matches).
        let status = if state.is_dragging {
            cosmic::iced::widget::slider::Status::Dragged
        } else if cursor.is_over(bounds) {
            cosmic::iced::widget::slider::Status::Hovered
        } else {
            cosmic::iced::widget::slider::Status::Active
        };
        let s = <cosmic::Theme as Catalog>::style(theme, &self.class, status);
        let handle_w = handle_extent(
            &s.handle.shape,
            s.handle.border_width,
            bounds.width,
            bounds.height,
        );
        // Notches go UNDER the slider, exactly as iced draws its own: the thumb overlaps the
        // tick band, and must paint over it.
        for &value in &self.notches {
            let t = rail_fraction(value, self.range.0, self.range.1);
            renderer.fill_quad(
                Quad {
                    bounds: notch_bounds(bounds, handle_w, t),
                    border: Border {
                        radius: 0.0.into(),
                        width: 0.0,
                        color: Color::TRANSPARENT,
                    },
                    ..Quad::default()
                },
                Background::Color(s.breakpoint.color),
            );
        }
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

impl<'a, Msg: 'a> From<NotchedSlider<'a, Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: NotchedSlider<'a, Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rail the preview editor's zoom slider actually uses.
    const RAIL_W: f32 = 120.0;
    const RAIL_H: f32 = 22.0;
    /// Cosmic's slider thumb: 20 at rest, 26 while hovered/dragged.
    const THUMB_REST: f32 = 20.0;
    const THUMB_ACTIVE: f32 = 26.0;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rectangle {
        Rectangle {
            x,
            y,
            width: w,
            height: h,
        }
    }

    /// STOCK iced's breakpoint placement — the bug. Kept here as the thing our notch must
    /// NOT do; the alignment tests below measure our notch against the thumb, and this
    /// against the thumb, and assert only ours coincides.
    fn stock_notch_centre_x(bounds_x: f32, bounds_w: f32, t: f32) -> f32 {
        bounds_x + (bounds_w - BREAKPOINT_WIDTH) * t + BREAKPOINT_WIDTH / 2.0
    }

    /// The centre of OUR notch quad.
    fn our_notch_centre_x(bounds: Rectangle, handle_w: f32, t: f32) -> f32 {
        let q = notch_bounds(bounds, handle_w, t);
        q.x + q.width / 2.0
    }

    /// THE INVARIANT: our notch centre IS the thumb centre — for every rail fraction, every
    /// thumb size (rest / hover / drag), every rail width and any surface scale (the widget
    /// works in logical px, so a scale factor is a pure multiplier on both sides — proven
    /// here by running the identical assertion at 1.0 / 1.25 / 1.5 / 2.0).
    #[test]
    fn notch_centre_equals_thumb_centre_at_every_fraction_and_scale() {
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            let bounds = rect(7.0 * scale, 3.0 * scale, RAIL_W * scale, RAIL_H * scale);
            for handle_w in [THUMB_REST * scale, THUMB_ACTIVE * scale, 2.0, 40.0 * scale] {
                for t in [0.0f32, 1.0 / 3.0, 0.25, 0.5, 0.75, 1.0] {
                    let thumb = thumb_centre_x(bounds.x, bounds.width, handle_w, t);
                    let ours = our_notch_centre_x(bounds, handle_w, t);
                    assert!(
                        (ours - thumb).abs() < 1e-3,
                        "scale {scale} handle {handle_w} t {t}: notch {ours} vs thumb {thumb}"
                    );
                }
            }
        }
    }

    /// The 100% notch on the REAL zoom slider (50…200% range → `t = 1/3`) lands on the
    /// thumb, where stock iced's tick is visibly adrift. This is the DRAGON-343 regression:
    /// stock is ~3px off at rest and ~4px off while dragging, ours is exact.
    #[test]
    fn hundred_percent_notch_is_exact_where_stock_iced_drifts() {
        let bounds = rect(0.0, 0.0, RAIL_W, RAIL_H);
        let t = rail_fraction(100.0, 50.0, 200.0);
        assert!((t - 1.0 / 3.0).abs() < 1e-6, "100% sits a third along a 50..200 rail");
        for (handle_w, min_drift) in [(THUMB_REST, 2.5f32), (THUMB_ACTIVE, 3.5)] {
            let thumb = thumb_centre_x(bounds.x, bounds.width, handle_w, t);
            assert!(
                (our_notch_centre_x(bounds, handle_w, t) - thumb).abs() < 1e-3,
                "our notch is on the thumb (handle {handle_w})"
            );
            let stock_drift = (stock_notch_centre_x(bounds.x, bounds.width, t) - thumb).abs();
            assert!(
                stock_drift > min_drift,
                "stock iced drifts by {stock_drift}px at handle {handle_w} — the bug"
            );
        }
    }

    /// The one fraction where stock iced happens to be right is the exact midpoint (all
    /// three of iced's spans evaluate to `width / 2` there). Documents WHY the drift is a
    /// function of `t`, not of DPI: it vanishes at `t = 0.5` on every scale factor.
    #[test]
    fn stock_iced_only_agrees_at_the_midpoint() {
        let bounds = rect(0.0, 0.0, RAIL_W, RAIL_H);
        let thumb = thumb_centre_x(bounds.x, bounds.width, THUMB_REST, 0.5);
        assert!((stock_notch_centre_x(bounds.x, bounds.width, 0.5) - thumb).abs() < 1e-3);
        assert!((thumb - RAIL_W / 2.0).abs() < 1e-3);
    }

    /// The drift stock iced shows is exactly `(handle_width - BREAKPOINT_WIDTH) * (t - 0.5)`
    /// — the formula quoted in the module docs, so a future reader can check the claim.
    #[test]
    fn stock_drift_matches_the_documented_formula() {
        let bounds = rect(0.0, 0.0, RAIL_W, RAIL_H);
        for handle_w in [THUMB_REST, THUMB_ACTIVE] {
            for t in [0.0f32, 1.0 / 3.0, 0.5, 0.9] {
                let predicted = (handle_w - BREAKPOINT_WIDTH) * (t - 0.5);
                let actual = stock_notch_centre_x(bounds.x, bounds.width, t)
                    - thumb_centre_x(bounds.x, bounds.width, handle_w, t);
                assert!(
                    (actual - predicted).abs() < 1e-3,
                    "handle {handle_w} t {t}: {actual} vs {predicted}"
                );
            }
        }
    }

    /// `handle_extent` reproduces cosmic's slider thumb: a 20px rectangle at rest, 26px
    /// hovered/dragged, and it does NOT get clamped away by a normal rail's bounds.
    #[test]
    fn handle_extent_matches_the_cosmic_theme_thumb() {
        let radius = cosmic::iced::core::border::Radius::from(4.0);
        for (w, want) in [(20u16, 20.0f32), (26, 26.0)] {
            let shape = HandleShape::Rectangle {
                width: w,
                height: w,
                border_radius: radius,
            };
            assert_eq!(handle_extent(&shape, 0.0, RAIL_W, RAIL_H), want);
        }
        // A bordered thumb can only grow to twice its border, never shrink below it.
        let shape = HandleShape::Rectangle {
            width: 2,
            height: 2,
            border_radius: radius,
        };
        assert_eq!(handle_extent(&shape, 3.0, RAIL_W, RAIL_H), 6.0);
        // Circles report their diameter, with iced's radius clamps applied.
        assert_eq!(
            handle_extent(&HandleShape::Circle { radius: 8.0 }, 0.0, RAIL_W, RAIL_H),
            16.0
        );
        assert_eq!(
            handle_extent(&HandleShape::Circle { radius: 40.0 }, 0.0, RAIL_W, RAIL_H),
            RAIL_H,
            "a radius past half the height clamps to the rail height"
        );
    }

    /// The notch band matches iced's: `rail_y + 6`, 2×8 px — so swapping our notch in for
    /// `.breakpoints` is visually a no-op apart from the x it fixes.
    #[test]
    fn notch_band_matches_iceds_breakpoint_quad() {
        let bounds = rect(0.0, 10.0, RAIL_W, RAIL_H);
        let q = notch_bounds(bounds, THUMB_REST, 0.5);
        assert_eq!(q.y, bounds.y + bounds.height / 2.0 + NOTCH_GAP);
        assert_eq!(q.width, BREAKPOINT_WIDTH);
        assert_eq!(q.height, NOTCH_HEIGHT);
    }

    #[test]
    fn rail_fraction_spans_and_clamps() {
        assert_eq!(rail_fraction(50.0, 50.0, 200.0), 0.0);
        assert_eq!(rail_fraction(200.0, 50.0, 200.0), 1.0);
        assert_eq!(rail_fraction(125.0, 50.0, 200.0), 0.5);
        // Out-of-range values pin to the ends rather than drawing off the rail.
        assert_eq!(rail_fraction(10.0, 50.0, 200.0), 0.0);
        assert_eq!(rail_fraction(999.0, 50.0, 200.0), 1.0);
        // A degenerate range behaves like iced's own guard.
        assert_eq!(rail_fraction(100.0, 200.0, 200.0), 0.0);
        assert_eq!(rail_fraction(100.0, 200.0, 50.0), 0.0);
    }
}
