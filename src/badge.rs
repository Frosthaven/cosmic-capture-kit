//! The SEQUENCE BADGE's pure geometry, numeral shapes and ink rule (DRAGON-340) — the one
//! module both the live canvas ([`crate::widgets::annotation_canvas`]) and the full-resolution
//! bake ([`crate::app::preview::annotate::rasterize_scene`]) build the badge from, so display
//! and export can never drift. The same split `crate::pen_stroke` uses for the pencil: the
//! MODEL (an [`crate::app::preview::annotate::AnnotKind::Badge`] square) lives in the app, the
//! DRAWING is derived here from pure functions of that square, and each renderer only maps the
//! result into its own space (the canvas through its zoom, the bake through its raster scale).
//!
//! # What a badge is
//! A SOLID disc in the annotation colour, a clear GAP, then an OUTER RING at the current line
//! weight — with the badge's ordinal centred on the disc in a fixed-advance (monospace)
//! numeral, inked in whichever of two tones actually contrasts with the disc.
//!
//! ```text
//!        ┌───────────────── the model square (side = 2·outer_r)
//!        │   ╭───────────╮
//!        │   │  ╭─────╮  │   ring      : stroke `ring_w`, centreline radius `outer_r`
//!        │   │  │ 12  │  │   gap       : `gap`, clear between the ring's inner edge and the disc
//!        │   │  ╰─────╯  │   disc      : filled, radius `disc_r`
//!        │   ╰───────────╯   numerals  : cap height `digit_h`, stroke `digit_stroke`
//! ```
//!
//! # Scale: every length here is SOURCE px (this is what makes the bake work)
//! The model square, [`GAP`] and the ring weight are all in IMAGE SOURCE pixels — the same
//! space every other annotation stores its geometry in. [`metrics`] is therefore a pure
//! function of the badge's SOURCE side and the SOURCE ring weight, and knows nothing about
//! zoom, display scale or capture resolution.
//!
//! Each renderer then applies exactly ONE uniform factor to everything the metrics produced:
//!
//! * the **bake** multiplies by its raster `scale` (`1.0` at full capture resolution) —
//!   identical to how a box outline's `stroke_w * scale` works;
//! * the **canvas** multiplies by `iss` (image → screen px at the current zoom).
//!
//! Two classes of figure come out of that, and the distinction is the whole scaling story:
//!
//! * **Absolute weights** — the ring ([`Metrics::ring_w`], the 2/4/6px line-weight preset) and
//!   the [`GAP`] are constants in SOURCE px, and stay constant however large the badge is. They
//!   are exactly as thick as a 2/4/6px box outline on the same capture, which is the point: the
//!   ticket asks the ring to *match the current line weight*.
//! * **Derived sizes** — the disc is whatever radius those weights leave, and the numerals are
//!   then fitted to the disc, so both grow with the badge.
//!
//! Both classes then take the SAME single render factor, so nothing is ever measured in screen
//! px and nothing can vanish at export size: at export the raster scale is `1.0`, the LARGEST
//! the figures ever render at (a 2px gap in the editor is 2 capture px in the exported PNG).
//! What can get thin is the *on-screen preview when zoomed out* — a zoom artefact, identical
//! for every stroked annotation in the editor.
//!
//! Badge SIZE feeds back into the absolute weights in exactly one place: the proportional
//! CEILINGS ([`RING_MAX_FRAC`], [`GAP_MAX_FRAC`]). A tiny badge can't spend 6px on a ring plus
//! 2px of gap and still have a disc left, so both thin out below those fractions, and the
//! numerals are sized to whatever disc survives. A normally-sized badge never meets the caps
//! and shows exactly the requested weights.
//!
//! # Why the numerals are hand-authored vector paths and not a font
//! A font would have to resolve identically in the canvas (iced/cosmic-text) and in the
//! off-thread tiny-skia bake, on Linux, macOS AND Windows, for the bake to match what the user
//! saw. It cannot be relied on to: "the monospace font" is a per-system fallback. These digits
//! are stroked polylines on a fixed grid ([`digit_unit_polylines`]) with a CONSTANT advance, so
//! they are monospace by construction, byte-identical on every platform, and the canvas and the
//! bake stroke the very same points — the pencil's parity contract, applied to numerals.

/// The clear gap (SOURCE px) between the filled disc's edge and the outer ring's INNER edge.
/// The ticket's "small 2px gap". Capped proportionally on a small badge — see [`GAP_MAX_FRAC`].
pub const GAP: f32 = 2.0;

/// The most of the badge's SIDE the outer ring's weight may take. A 6px ring on a 20px badge
/// would leave no disc at all, so the ring thins with the badge below this ceiling; above it
/// (any badge wider than `ring_w / RING_MAX_FRAC`) the ring is exactly the selected line
/// weight.
pub const RING_MAX_FRAC: f32 = 0.18;

/// The most of the ring's INNER radius the [`GAP`] may take, so a small badge keeps a visible
/// disc instead of dissolving into ring + air.
pub const GAP_MAX_FRAC: f32 = 0.18;

/// Numeral ink-box WIDTH as a fraction of the cap height — the monospace cell the glyphs in
/// [`digit_unit_polylines`] are drawn inside.
pub const DIGIT_W_FRAC: f32 = 0.66;

/// Numeral ADVANCE as a fraction of the cap height: the fixed pen-to-pen step between two
/// digits. Wider than [`DIGIT_W_FRAC`] by the side bearing, so "11" doesn't touch.
pub const DIGIT_ADV_FRAC: f32 = 0.76;

/// Numeral STROKE weight as a fraction of the cap height.
pub const DIGIT_STROKE_FRAC: f32 = 0.15;

/// How much of the disc's radius the numerals' INK may reach (measured on the ink box's
/// half-diagonal, stroke included), so "99" sits comfortably inside the circle instead of
/// touching it.
pub const TEXT_FIT: f32 = 0.94;

/// The LIGHT numeral ink — near-white, for a dark badge colour.
pub const INK_LIGHT: [u8; 3] = [255, 255, 255];

/// The DARK numeral ink — near-black, for a light badge colour. Not pure black: it matches the
/// tone dark UI text uses and keeps the numeral from reading as a hole.
pub const INK_DARK: [u8; 3] = [26, 26, 26];

// ── the ink (contrast) rule ─────────────────────────────────────────────────────────────

/// One sRGB channel (0..1) linearised, per the WCAG 2.x relative-luminance definition.
fn linearize(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 2.x RELATIVE LUMINANCE of an sRGB colour whose channels are 0..1. Pure — unit-tested.
pub fn relative_luminance(rgb: [f32; 3]) -> f32 {
    0.2126 * linearize(rgb[0]) + 0.7152 * linearize(rgb[1]) + 0.0722 * linearize(rgb[2])
}

/// The WCAG contrast RATIO (1..21) between two relative luminances. Pure — unit-tested.
pub fn contrast_ratio(a: f32, b: f32) -> f32 {
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whether the numerals on a badge filled with `fill` (sRGB 0..1) must be inked DARK
/// ([`INK_DARK`]) rather than LIGHT ([`INK_LIGHT`]): whichever of the two tones has the HIGHER
/// WCAG contrast ratio against the fill wins. A real contrast rule, not a luminance threshold
/// pulled out of the air — the crossover falls where the two ratios are equal (≈ 0.202
/// relative luminance for this ink pair), which is *above* mid-grey, so a 50% grey badge
/// correctly takes dark ink.
///
/// The badge colour is user-editable and can change at any time, so every renderer calls this
/// at DRAW time from the item's current colour — the ink is never stored and can never go
/// stale. Pure — unit-tested.
pub fn prefers_dark_ink(fill: [f32; 3]) -> bool {
    let l = relative_luminance(fill);
    let dark = contrast_ratio(l, relative_luminance([
        INK_DARK[0] as f32 / 255.0,
        INK_DARK[1] as f32 / 255.0,
        INK_DARK[2] as f32 / 255.0,
    ]));
    let light = contrast_ratio(l, relative_luminance([
        INK_LIGHT[0] as f32 / 255.0,
        INK_LIGHT[1] as f32 / 255.0,
        INK_LIGHT[2] as f32 / 255.0,
    ]));
    dark > light
}

/// [`prefers_dark_ink`] over straight-alpha RGBA BYTES, returning the ink's RGB bytes — the
/// convenience the model/bake side uses. Alpha is ignored: the ink is chosen against the
/// colour the user picked, not against whatever happens to be behind a translucent badge.
pub fn ink_rgb8(fill: [u8; 4]) -> [u8; 3] {
    let norm = [fill[0] as f32 / 255.0, fill[1] as f32 / 255.0, fill[2] as f32 / 255.0];
    if prefers_dark_ink(norm) { INK_DARK } else { INK_LIGHT }
}

// ── geometry ────────────────────────────────────────────────────────────────────────────

/// Every length a badge is drawn from, in SOURCE px — the single output of the geometry model.
/// See the module doc for how each renderer scales these.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Metrics {
    /// The outer ring's CENTRELINE radius (half the model square's side).
    pub outer_r: f32,
    /// The outer ring's stroke weight (the selected line weight, capped on a small badge).
    pub ring_w: f32,
    /// The clear gap between the disc's edge and the ring's inner edge.
    pub gap: f32,
    /// The filled disc's radius.
    pub disc_r: f32,
    /// The numerals' cap height.
    pub digit_h: f32,
    /// The numerals' stroke weight.
    pub digit_stroke: f32,
    /// The fixed pen-to-pen step between two digits (monospace advance).
    pub digit_advance: f32,
    /// The whole number's ink-box width (CENTRELINE extents, stroke excluded).
    pub text_w: f32,
}

/// The badge geometry for a model square of `side` SOURCE px carrying `ring_w` line weight and
/// a `digits`-digit ordinal.
///
/// Order of construction (each step spends what the previous one left):
/// 1. `outer_r = side / 2` — the ring's centreline, so the ring straddles the model square's
///    inscribed circle exactly like a box outline straddles its rect (which is what makes
///    `kind_draw_margin` a plain `ring_w / 2`).
/// 2. the ring weight, capped at [`RING_MAX_FRAC`] of the side;
/// 3. the [`GAP`], capped at [`GAP_MAX_FRAC`] of the ring's inner radius;
/// 4. the disc = whatever radius is left;
/// 5. the numerals, sized so their ink box's half-diagonal reaches [`TEXT_FIT`] of the disc
///    radius — the exact fit that keeps a two-digit number inside a circle.
///
/// Degenerate input (a zero or negative side) yields all-zero metrics, which every renderer
/// draws as nothing. Pure — unit-tested.
pub fn metrics(side: f32, ring_w: f32, digits: u32) -> Metrics {
    let side = side.max(0.0);
    let outer_r = side * 0.5;
    let ring = ring_w.max(0.0).min(side * RING_MAX_FRAC);
    // The ring's INNER edge: the first radius the disc could reach.
    let inner = (outer_r - ring * 0.5).max(0.0);
    let gap = GAP.min(inner * GAP_MAX_FRAC);
    let disc_r = (inner - gap).max(0.0);
    let n = digits.max(1) as f32;
    // Half the numerals' ink box, in cap heights: width (advance × the gaps + one cell + the
    // stroke straddling both ends) and height (cap height + the stroke).
    let w_term = (n - 1.0) * DIGIT_ADV_FRAC + DIGIT_W_FRAC + DIGIT_STROKE_FRAC;
    let h_term = 1.0 + DIGIT_STROKE_FRAC;
    let digit_h = 2.0 * disc_r * TEXT_FIT / (w_term * w_term + h_term * h_term).sqrt();
    Metrics {
        outer_r,
        ring_w: ring,
        gap,
        disc_r,
        digit_h,
        digit_stroke: digit_h * DIGIT_STROKE_FRAC,
        digit_advance: digit_h * DIGIT_ADV_FRAC,
        text_w: digit_h * ((n - 1.0) * DIGIT_ADV_FRAC + DIGIT_W_FRAC),
    }
}

/// How many decimal digits `n` prints as (at least 1) — the count [`metrics`] sizes for.
pub fn digit_count(n: u32) -> u32 {
    let mut c = 1;
    let mut v = n;
    while v >= 10 {
        v /= 10;
        c += 1;
    }
    c
}

// ── the numerals ────────────────────────────────────────────────────────────────────────

/// How many line segments one full 360° arc is flattened into. The numerals are drawn at a few
/// tens of px at most and stroked with round joins, so this is smooth well past any zoom the
/// preview offers, and it keeps the canvas and the bake on ONE flattening (no renderer-specific
/// curve subdivision to diverge on).
const ARC_STEPS: usize = 64;

/// Append the elliptical arc `a0..a1` (DEGREES, counter-clockwise in maths convention with y
/// pointing DOWN on screen, i.e. `+sin` is upward) of the ellipse centred `(cx, cy)` with radii
/// `(rx, ry)` to `out`, skipping the first point when `out` already ends on it.
fn arc(out: &mut Vec<(f32, f32)>, cx: f32, cy: f32, rx: f32, ry: f32, a0: f32, a1: f32) {
    let steps = ((a1 - a0).abs() / 360.0 * ARC_STEPS as f32).ceil().max(2.0) as usize;
    for i in 0..=steps {
        let t = a0 + (a1 - a0) * (i as f32 / steps as f32);
        let r = t.to_radians();
        let p = (cx + rx * r.cos(), cy - ry * r.sin());
        if i == 0 && out.last().is_some_and(|l| (l.0 - p.0).abs() < 1e-4 && (l.1 - p.1).abs() < 1e-4) {
            continue;
        }
        out.push(p);
    }
}

/// The polyline CENTRELINES of one decimal digit inside its UNIT CELL: `x` and `y` both run
/// `0..1`, `x` spanning the monospace cell's width and `y` the cap height, `y` pointing DOWN.
/// The caller scales `x` by [`DIGIT_W_FRAC`]`× digit_h` and `y` by `digit_h`.
///
/// A geometric, single-weight numeral set (the look of a technical mono face) drawn from lines
/// and elliptical arcs, so it strokes cleanly at any size with round caps/joins. Every glyph's
/// ink stays inside roughly `0.10..0.92 × 0.04..0.96` of the cell, which is the margin the
/// advance and the [`TEXT_FIT`] fit are tuned against. A digit outside `0..=9` yields no ink.
/// Pure — unit-tested.
pub fn digit_unit_polylines(digit: u8) -> Vec<Vec<(f32, f32)>> {
    let mut out: Vec<Vec<(f32, f32)>> = Vec::new();
    match digit {
        0 => {
            let mut p = Vec::new();
            arc(&mut p, 0.50, 0.50, 0.38, 0.46, 90.0, 450.0);
            out.push(p);
        }
        1 => {
            out.push(vec![(0.20, 0.26), (0.52, 0.05), (0.52, 0.95)]);
            out.push(vec![(0.22, 0.95), (0.82, 0.95)]);
        }
        2 => {
            let mut p = Vec::new();
            arc(&mut p, 0.50, 0.30, 0.38, 0.26, 180.0, -35.0);
            p.push((0.14, 0.95));
            p.push((0.88, 0.95));
            out.push(p);
        }
        3 => {
            let mut p = vec![(0.16, 0.05), (0.82, 0.05), (0.45, 0.44)];
            arc(&mut p, 0.45, 0.70, 0.36, 0.26, 90.0, -165.0);
            out.push(p);
        }
        4 => {
            out.push(vec![(0.68, 0.05), (0.10, 0.70), (0.92, 0.70)]);
            out.push(vec![(0.68, 0.05), (0.68, 0.95)]);
        }
        5 => {
            let mut p = vec![(0.84, 0.05), (0.20, 0.05), (0.20, 0.44), (0.50, 0.44)];
            arc(&mut p, 0.50, 0.70, 0.38, 0.26, 90.0, -160.0);
            out.push(p);
        }
        6 => {
            // The spine sweeps from the top down into the bowl's leftmost point…
            let mut spine = Vec::new();
            arc(&mut spine, 0.62, 0.66, 0.50, 0.60, 90.0, 180.0);
            out.push(spine);
            // …where the closed bowl begins.
            let mut bowl = Vec::new();
            arc(&mut bowl, 0.50, 0.66, 0.38, 0.30, 180.0, 540.0);
            out.push(bowl);
        }
        7 => {
            out.push(vec![(0.12, 0.05), (0.88, 0.05), (0.34, 0.95)]);
        }
        8 => {
            let mut top = Vec::new();
            arc(&mut top, 0.50, 0.28, 0.31, 0.23, 90.0, 450.0);
            out.push(top);
            let mut bot = Vec::new();
            arc(&mut bot, 0.50, 0.72, 0.38, 0.23, 90.0, 450.0);
            out.push(bot);
        }
        9 => {
            // The 6, rotated: closed bowl on top, spine sweeping down from its rightmost point.
            let mut bowl = Vec::new();
            arc(&mut bowl, 0.50, 0.34, 0.38, 0.30, 0.0, 360.0);
            out.push(bowl);
            let mut spine = Vec::new();
            arc(&mut spine, 0.38, 0.34, 0.50, 0.60, 0.0, -90.0);
            out.push(spine);
        }
        _ => {}
    }
    out
}

/// The whole ordinal `number` as polyline CENTRELINES in SOURCE px, centred on `center` (the
/// badge's centre) using the numeral sizes in `m` — what both renderers stroke at
/// `m.digit_stroke` with round caps/joins. The digits are laid out on the fixed
/// [`Metrics::digit_advance`] grid, so the number is monospace and centred as a block.
/// Pure — unit-tested.
pub fn number_polylines(number: u32, m: &Metrics, center: (f32, f32)) -> Vec<Vec<(f32, f32)>> {
    if m.digit_h <= 0.0 {
        return Vec::new();
    }
    let text = number.to_string();
    let cell_w = m.digit_h * DIGIT_W_FRAC;
    let left = center.0 - m.text_w * 0.5;
    let top = center.1 - m.digit_h * 0.5;
    let mut out = Vec::new();
    for (i, ch) in text.bytes().enumerate() {
        let ox = left + i as f32 * m.digit_advance;
        for poly in digit_unit_polylines(ch.wrapping_sub(b'0')) {
            out.push(
                poly.into_iter()
                    .map(|(x, y)| (ox + x * cell_w, top + y * m.digit_h))
                    .collect(),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    // ── the ink (contrast) rule ─────────────────────────────────────────────────────────

    /// WCAG anchors: black is 0, white is 1, and the ratio between them is 21:1.
    #[test]
    fn relative_luminance_and_ratio_hit_the_wcag_anchors() {
        assert!(relative_luminance([0.0, 0.0, 0.0]).abs() < 1e-6);
        assert!((relative_luminance([1.0, 1.0, 1.0]) - 1.0).abs() < 1e-4);
        let r = contrast_ratio(0.0, 1.0);
        assert!((r - 21.0).abs() < 0.01, "black/white contrast is 21:1, got {r}");
        // Symmetric in its arguments.
        assert_eq!(contrast_ratio(0.0, 1.0), contrast_ratio(1.0, 0.0));
    }

    /// The ink picker across a spread of real badge colours — including the mid-greys that sit
    /// either side of the crossover, which is where a naive "luminance > 0.5" rule goes wrong.
    #[rstest]
    // Unambiguously dark fills → light ink.
    #[case([0, 0, 0], false)]
    #[case([20, 20, 20], false)]
    #[case([200, 0, 0], false)] // saturated red is dark by luminance
    #[case([0, 0, 255], false)]
    #[case([90, 90, 90], false)]
    // Just BELOW the crossover (relative luminance ≈ 0.178) → still light ink.
    #[case([117, 117, 117], false)]
    // Just ABOVE it (mid-grey #808080, luminance ≈ 0.216) → dark ink, though it is "50% grey".
    #[case([128, 128, 128], true)]
    #[case([140, 140, 140], true)]
    // Unambiguously light fills → dark ink.
    #[case([255, 255, 255], true)]
    #[case([255, 255, 0], true)]
    #[case([0, 255, 0], true)]
    #[case([255, 200, 120], true)]
    fn ink_picks_whichever_tone_actually_contrasts(#[case] fill: [u8; 3], #[case] dark: bool) {
        let norm = [fill[0] as f32 / 255.0, fill[1] as f32 / 255.0, fill[2] as f32 / 255.0];
        assert_eq!(prefers_dark_ink(norm), dark, "fill {fill:?}");
        assert_eq!(ink_rgb8([fill[0], fill[1], fill[2], 255]), if dark { INK_DARK } else { INK_LIGHT });
    }

    /// Whatever the fill, the chosen ink is the one with the BETTER contrast ratio — the actual
    /// rule, checked directly rather than through the crossover.
    #[test]
    fn the_chosen_ink_always_has_the_higher_contrast_ratio() {
        let lum = |c: [u8; 3]| {
            relative_luminance([c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0])
        };
        for r in (0..=255).step_by(17) {
            for g in (0..=255).step_by(51) {
                for b in (0..=255).step_by(51) {
                    let fill = [r as u8, g, b];
                    let l = lum(fill);
                    let chosen = ink_rgb8([fill[0], fill[1], fill[2], 255]);
                    let other = if chosen == INK_DARK { INK_LIGHT } else { INK_DARK };
                    assert!(
                        contrast_ratio(l, lum(chosen)) >= contrast_ratio(l, lum(other)),
                        "fill {fill:?} picked the worse ink"
                    );
                }
            }
        }
    }

    /// Alpha never enters the decision — the ink tracks the picked colour, not the backdrop.
    #[test]
    fn ink_ignores_alpha() {
        assert_eq!(ink_rgb8([255, 255, 255, 10]), ink_rgb8([255, 255, 255, 255]));
    }

    // ── geometry ────────────────────────────────────────────────────────────────────────

    /// The layout budget adds up exactly: ring centreline + half ring + gap + disc = the radius.
    #[rstest]
    #[case(120.0, 2.0, 1)]
    #[case(120.0, 4.0, 2)]
    #[case(200.0, 6.0, 2)]
    #[case(64.0, 4.0, 1)]
    fn the_radius_budget_is_exact(#[case] side: f32, #[case] ring: f32, #[case] digits: u32) {
        let m = metrics(side, ring, digits);
        assert!((m.outer_r - side / 2.0).abs() < 1e-4);
        let spent = m.ring_w / 2.0 + m.gap + m.disc_r;
        assert!((spent - m.outer_r).abs() < 1e-4, "budget {spent} != radius {}", m.outer_r);
        assert!(m.disc_r > 0.0);
    }

    /// A comfortably-sized badge gets EXACTLY the requested ring weight and the full 2px gap —
    /// the proportional caps only bite on small badges.
    #[rstest]
    #[case(2.0)]
    #[case(4.0)]
    #[case(6.0)]
    fn a_normal_badge_gets_the_exact_line_weight_and_gap(#[case] ring: f32) {
        let m = metrics(120.0, ring, 2);
        assert_eq!(m.ring_w, ring);
        assert_eq!(m.gap, GAP);
    }

    /// A tiny badge thins its ring and gap instead of eating the disc — the whole point of the
    /// proportional ceilings. The disc always survives.
    #[test]
    fn a_tiny_badge_thins_the_ring_and_gap_rather_than_losing_the_disc() {
        let m = metrics(16.0, 6.0, 2);
        assert!(m.ring_w < 6.0, "ring must thin on a 16px badge, got {}", m.ring_w);
        assert!(m.gap < GAP);
        assert!(m.disc_r > 0.0 && m.digit_h > 0.0);
        // Still a coherent budget.
        assert!((m.ring_w / 2.0 + m.gap + m.disc_r - m.outer_r).abs() < 1e-4);
    }

    /// Degenerate sizes produce nothing to draw rather than NaNs or negative radii.
    #[rstest]
    #[case(0.0)]
    #[case(-10.0)]
    fn a_degenerate_badge_is_all_zeroes(#[case] side: f32) {
        let m = metrics(side, 4.0, 1);
        assert_eq!(m.disc_r, 0.0);
        assert_eq!(m.digit_h, 0.0);
        assert!(number_polylines(1, &m, (0.0, 0.0)).is_empty());
    }

    /// The two classes of figure behave as documented: the ring and gap are ABSOLUTE source-px
    /// weights that a bigger badge does not inflate (so the ring always matches the line weight
    /// the user picked), while the disc — and with it the numerals — grow with the badge.
    #[test]
    fn absolute_weights_stay_absolute_while_the_disc_and_numerals_grow() {
        let small = metrics(100.0, 4.0, 2);
        let big = metrics(400.0, 4.0, 2);
        assert_eq!(small.ring_w, big.ring_w, "the ring is an absolute weight");
        assert_eq!(small.gap, big.gap, "the gap is an absolute weight");
        // Every source px the badge gained went to the disc.
        assert!((big.disc_r - small.disc_r - (big.outer_r - small.outer_r)).abs() < 1e-3);
        // The numerals are a fixed proportion OF THE DISC, so they follow it.
        assert!((big.digit_h / big.disc_r - small.digit_h / small.disc_r).abs() < 1e-4);
        assert!(big.digit_h > small.digit_h && big.digit_stroke > small.digit_stroke);
    }

    /// The numerals scale linearly with the disc they are fitted to — the property that lets a
    /// badge drawn tiny and the same badge drawn huge be one design at two sizes.
    #[test]
    fn the_numerals_are_linear_in_the_disc() {
        let a = metrics(100.0, 0.0, 2);
        let b = metrics(200.0, 0.0, 2);
        let k = b.disc_r / a.disc_r;
        for (x, y) in [
            (a.digit_h, b.digit_h),
            (a.digit_stroke, b.digit_stroke),
            (a.digit_advance, b.digit_advance),
            (a.text_w, b.text_w),
        ] {
            assert!((x * k - y).abs() < 1e-3, "{x} * {k} != {y}");
        }
    }

    /// Two digits fit inside the disc with room to spare, and one digit is drawn LARGER than
    /// two — the numerals are sized to the number they carry.
    #[rstest]
    #[case(1)]
    #[case(9)]
    #[case(10)]
    #[case(42)]
    #[case(99)]
    fn the_number_ink_stays_inside_the_disc(#[case] n: u32) {
        let side = 120.0;
        let m = metrics(side, 4.0, digit_count(n));
        let c = (500.0, 300.0);
        let polys = number_polylines(n, &m, c);
        assert!(!polys.is_empty(), "{n} draws ink");
        let limit = m.disc_r - m.digit_stroke / 2.0;
        for p in &polys {
            for &(x, y) in p {
                let d = ((x - c.0).powi(2) + (y - c.1).powi(2)).sqrt();
                assert!(d <= limit, "{n}: point at {d} escapes the disc ({limit})");
            }
        }
    }

    #[test]
    fn one_digit_is_drawn_larger_than_two() {
        let one = metrics(120.0, 4.0, 1);
        let two = metrics(120.0, 4.0, 2);
        assert!(one.digit_h > two.digit_h);
        assert_eq!(one.disc_r, two.disc_r); // same disc — only the numerals resize
    }

    /// The numerals are MONOSPACE: the second digit of a two-digit number is the first one
    /// translated by EXACTLY one [`Metrics::digit_advance`], whatever the two digits are — a
    /// fixed pen-to-pen grid, not a per-glyph width.
    #[test]
    fn the_numerals_sit_on_a_fixed_advance_grid() {
        let m = metrics(120.0, 4.0, 2);
        for d in 1..=9u32 {
            let polys = number_polylines(d * 11, &m, (0.0, 0.0)); // "dd" — the same glyph twice
            let per = digit_unit_polylines(d as u8).len();
            assert_eq!(polys.len(), 2 * per, "digit {d} draws a different glyph twice");
            for i in 0..per {
                let a = &polys[i];
                let b = &polys[per + i];
                assert_eq!(a.len(), b.len());
                for (p, q) in a.iter().zip(b) {
                    assert!(
                        (q.0 - p.0 - m.digit_advance).abs() < 1e-3 && (q.1 - p.1).abs() < 1e-3,
                        "digit {d}: advance drifted"
                    );
                }
            }
        }
    }

    /// The number is CENTRED on the badge centre — a one- and a two-digit ordinal both sit
    /// symmetrically about it (within the monospace cell's own side bearings).
    #[rstest]
    #[case(7)]
    #[case(11)]
    #[case(99)]
    fn the_number_is_centred_on_the_badge(#[case] n: u32) {
        let m = metrics(120.0, 4.0, digit_count(n));
        let polys = number_polylines(n, &m, (0.0, 0.0));
        let xs: Vec<f32> = polys.iter().flatten().map(|q| q.0).collect();
        let ys: Vec<f32> = polys.iter().flatten().map(|q| q.1).collect();
        let mid = |v: &[f32]| {
            (v.iter().cloned().fold(f32::INFINITY, f32::min)
                + v.iter().cloned().fold(f32::NEG_INFINITY, f32::max))
                / 2.0
        };
        assert!(mid(&xs).abs() < m.digit_h * 0.1, "x off-centre by {}", mid(&xs));
        assert!(mid(&ys).abs() < m.digit_h * 0.1, "y off-centre by {}", mid(&ys));
    }

    /// Every decimal digit draws ink, and stays inside its unit cell (the margin the advance
    /// and the disc fit are tuned against).
    #[test]
    fn every_digit_draws_inside_its_unit_cell() {
        for d in 0..=9u8 {
            let polys = digit_unit_polylines(d);
            assert!(!polys.is_empty(), "digit {d} draws nothing");
            for p in &polys {
                assert!(p.len() >= 2, "digit {d} has a degenerate stroke");
                for &(x, y) in p {
                    assert!((-0.001..=1.001).contains(&x), "digit {d} x={x} escapes the cell");
                    assert!((-0.001..=1.001).contains(&y), "digit {d} y={y} escapes the cell");
                }
            }
        }
        // A non-digit byte inks nothing rather than panicking.
        assert!(digit_unit_polylines(10).is_empty());
        assert!(digit_unit_polylines(200).is_empty());
    }

    #[rstest]
    #[case(0, 1)]
    #[case(9, 1)]
    #[case(10, 2)]
    #[case(99, 2)]
    #[case(100, 3)]
    fn digit_count_matches_the_printed_form(#[case] n: u32, #[case] want: u32) {
        assert_eq!(digit_count(n), want);
        assert_eq!(digit_count(n) as usize, n.to_string().len());
    }
}
