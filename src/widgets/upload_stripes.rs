//! The upload meter's FINALIZE animation (DRAGON-537): slanted blocks, slightly darker than
//! the accent, sweeping left to right inside the full accent fill.
//!
//! # Why it exists
//!
//! The finalize wait (every byte delivered and acknowledged, outcome not arrived) held a
//! full accent bar perfectly still for several seconds. That is the same "a bar frozen on
//! one value reads as broken" problem the meter's spinner rule was written against, just at
//! the far end of the transfer. The sweep keeps the accent (the colour still says
//! "running") while the motion says the app is doing something.
//!
//! # How it draws
//!
//! A leaf widget painting quads, like `output_selection`: the full rounded fill first, then
//! the dark blocks. A `Quad` cannot rotate, so the slant is drawn as 1px ROWS, each row's
//! blocks shifted [`SLOPE`] further left than the row above: at the bar's 5px height that
//! is a handful of quads per frame, which is nothing. The row geometry ([`stripe_spans`])
//! is pure and unit-tested; the widget only paints its answer. The blocks are inset from
//! the bar's rounded ends by the corner radius, so their square edges never overwrite the
//! rounding.
//!
//! # How it animates
//!
//! The phase is app STATE (`EditState::upload_anim`), advanced by
//! `sub_upload_finalize_anim`'s ~30fps tick through [`advance`]: the house animation
//! pattern (the folder refresh glyph's spin, the scanner's re-read wheel), not a
//! self-driving widget, so the redraw cadence is visible in `subscriptions.rs` beside every
//! other timer and vanishes the instant no meter is finalizing.

use cosmic::iced::core::renderer::Quad;
use cosmic::iced::core::widget::Tree;
use cosmic::iced::core::{Background, Border, Length, Rectangle, Shadow, Size};
use cosmic::widget::Widget;

/// One dark-plus-light period of the stripe pattern, px. Two-ish periods are visible across
/// the meter's 80px track: blocks, per the owner's ask, not a fine candy stripe.
pub const STRIPE_PITCH: f32 = 14.0;

/// The dark block's width within one period, px: half the period, so block and gap read as
/// equals.
const STRIPE_W: f32 = 7.0;

/// How far each 1px row down shifts LEFT, px. 1.0 is a 45 degree lean, trailing the motion
/// like an italic stroke.
const SLOPE: f32 = 1.0;

/// How far the pattern travels per animation tick, px. At the ~30fps tick this is about
/// 21px/s: visibly moving at a glance, slow enough not to shimmer in a 5px-tall bar.
const STRIPE_STEP: f32 = 0.7;

/// One animation tick's worth of phase. Pure; unit-tested. Wrapped at [`STRIPE_PITCH`] so
/// the phase never grows without bound and equal phases draw equal pictures.
pub fn advance(phase: f32) -> f32 {
    (phase + STRIPE_STEP) % STRIPE_PITCH
}

/// The dark blocks' x-spans for one 1px row, relative to the bar's left edge. Pure;
/// unit-tested.
///
/// The pattern lives in the bar's own coordinates and every row samples the SAME pattern,
/// offset by `row` times [`SLOPE`]; `inset` clips both ends so the square-edged blocks stay
/// clear of the fill's rounded corners. `rem_euclid`, not `%`: the row offset can push the
/// base negative, and a negative remainder would skip the leftmost block.
pub fn stripe_spans(width: f32, inset: f32, row: f32, phase: f32) -> Vec<(f32, f32)> {
    let lo = inset;
    let hi = (width - inset).max(lo);
    let base = (phase - row * SLOPE).rem_euclid(STRIPE_PITCH);
    let mut spans = Vec::new();
    // One period before the window, so a block straddling the left edge is not skipped.
    let mut start = base - STRIPE_PITCH;
    while start < hi {
        let a = start.max(lo);
        let b = (start + STRIPE_W).min(hi);
        if b > a {
            spans.push((a, b));
        }
        start += STRIPE_PITCH;
    }
    spans
}

/// The blocks' colour: the fill's own tint, slightly darkened. Pure; unit-tested.
///
/// A multiply on the channels rather than a blend toward a theme token, so the blocks stay
/// THE ACCENT to the eye (the owner's ask: "slightly darker than the accent color") on any
/// accent the user picks, light or dark. Alpha is untouched: the fill under the blocks is
/// opaque already.
pub fn block_tint(fill: cosmic::iced::Color) -> cosmic::iced::Color {
    const DARKEN: f32 = 0.82;
    cosmic::iced::Color {
        r: fill.r * DARKEN,
        g: fill.g * DARKEN,
        b: fill.b * DARKEN,
        a: fill.a,
    }
}

/// The animated finalize bar: a fixed-size leaf for the meter's track shell.
pub struct UploadStripes {
    width: f32,
    height: f32,
    phase: f32,
    tint: fn(&cosmic::Theme) -> cosmic::iced::Color,
}

/// Build the finalize bar at `width` by `height`, its pattern at `phase`, filled in `tint`.
pub fn upload_stripes<Msg: 'static>(
    width: f32,
    height: f32,
    phase: f32,
    tint: fn(&cosmic::Theme) -> cosmic::iced::Color,
) -> cosmic::Element<'static, Msg> {
    cosmic::Element::new(UploadStripes { width, height, phase, tint })
}

impl<Msg> Widget<Msg, cosmic::Theme, cosmic::Renderer> for UploadStripes {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.width), Length::Fixed(self.height))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &cosmic::Renderer,
        limits: &cosmic::iced::core::layout::Limits,
    ) -> cosmic::iced::core::layout::Node {
        cosmic::iced::core::layout::Node::new(limits.resolve(
            Length::Fixed(self.width),
            Length::Fixed(self.height),
            Size::new(self.width, self.height),
        ))
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        _style: &cosmic::iced::core::renderer::Style,
        layout: cosmic::iced::core::Layout<'_>,
        _cursor: cosmic::iced::core::mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use cosmic::iced::core::Renderer as _;
        let bounds = layout.bounds();
        let fill = (self.tint)(theme);
        // The full fill, rounded exactly as the plain bar's fill is (radius = half height).
        renderer.fill_quad(
            Quad {
                bounds,
                border: Border { radius: (self.height / 2.0).into(), ..Default::default() },
                shadow: Shadow::default(),
                snap: true,
            },
            Background::Color(fill),
        );
        let dark = block_tint(fill);
        let inset = self.height / 2.0;
        let rows = bounds.height.round().max(1.0) as u32;
        for row in 0..rows {
            for (a, b) in stripe_spans(bounds.width, inset, row as f32, self.phase) {
                renderer.fill_quad(
                    Quad {
                        bounds: Rectangle {
                            x: bounds.x + a,
                            y: bounds.y + row as f32,
                            width: b - a,
                            height: 1.0,
                        },
                        border: Border::default(),
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    Background::Color(dark),
                );
            }
        }
    }
}

impl<Msg: 'static> From<UploadStripes> for cosmic::Element<'static, Msg> {
    fn from(w: UploadStripes) -> cosmic::Element<'static, Msg> {
        cosmic::Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The meter track's real footprint (`chrome.rs`), which is the one surface this draws.
    const W: f32 = 80.0;
    const INSET: f32 = 2.5;

    /// Every span stays inside the inset window, ordered and non-overlapping, at any phase:
    /// a block outside the window would square off the fill's rounded ends.
    #[test]
    fn spans_stay_inside_the_window_and_do_not_overlap() {
        for phase in [0.0f32, 0.3, 6.9, 7.0, 13.9] {
            for row in 0..5 {
                let spans = stripe_spans(W, INSET, row as f32, phase);
                assert!(!spans.is_empty(), "phase {phase} row {row} drew nothing");
                let mut last_end = f32::MIN;
                for (a, b) in spans {
                    assert!(a >= INSET - 1e-4 && b <= W - INSET + 1e-4, "({a},{b}) leaves the window");
                    assert!(b > a, "an empty span survived");
                    assert!(a > last_end, "spans out of order or overlapping");
                    last_end = b;
                }
            }
        }
    }

    /// One full pitch of phase is the identity: the sweep loops seamlessly, which is what
    /// lets [`advance`] wrap instead of growing forever.
    #[test]
    fn a_full_pitch_of_phase_is_the_identity() {
        for row in 0..5 {
            assert_eq!(
                stripe_spans(W, INSET, row as f32, 3.25),
                stripe_spans(W, INSET, row as f32, 3.25 + STRIPE_PITCH),
                "row {row} does not tile at the pitch"
            );
        }
        let wrapped = advance(STRIPE_PITCH - STRIPE_STEP / 2.0);
        assert!((0.0..STRIPE_PITCH).contains(&wrapped), "advance must stay inside one pitch");
    }

    /// Each row down is the row above shifted by [`SLOPE`]: the slant is real, not five
    /// copies of the same row.
    #[test]
    fn rows_lean_by_the_slope() {
        // Compared in the pattern's own space (an unclipped window), where the shift is
        // exact rather than truncated by the edges.
        let wide = 10.0 * STRIPE_PITCH;
        let top = stripe_spans(wide, 0.0, 0.0, 5.0);
        let below = stripe_spans(wide, 0.0, 1.0, 5.0 + SLOPE);
        assert_eq!(top, below, "shifting the phase by SLOPE must cancel one row's lean");
        assert_ne!(
            stripe_spans(wide, 0.0, 0.0, 5.0),
            stripe_spans(wide, 0.0, 1.0, 5.0),
            "adjacent rows must not draw identically, or the blocks have no slant"
        );
    }

    /// The blocks stay the accent, only darker, whatever the accent is: every channel is
    /// reduced by the same factor and alpha is untouched.
    #[test]
    fn the_block_tint_darkens_every_channel_and_keeps_alpha() {
        let accent = cosmic::iced::Color { r: 0.2, g: 0.55, b: 0.9, a: 1.0 };
        let dark = block_tint(accent);
        assert!(dark.r < accent.r && dark.g < accent.g && dark.b < accent.b);
        assert!((dark.r / accent.r - dark.b / accent.b).abs() < 1e-6, "the hue must not shift");
        assert_eq!(dark.a, accent.a);
        // Black cannot get darker, and must not become something else.
        let black = cosmic::iced::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.5 };
        assert_eq!(block_tint(black).a, 0.5);
        assert_eq!(block_tint(black).r, 0.0);
    }
}
