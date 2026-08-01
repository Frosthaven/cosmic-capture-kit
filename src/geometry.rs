//! Pure rectangle + quad geometry in global compositor logical coordinates.
//!
//! No widget, no rendering: just the hit-testing and normalization the region-selection
//! overlay and the capture pipeline share, kept here so it can be unit-tested without a
//! compositor. Grab radii are passed in by the caller, so the widget keeps its own tuning
//! constants and this module stays a pure function of its inputs.

/// A corner handle of a rectangle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Corner {
    Nw,
    Ne,
    Sw,
    Se,
}

/// An edge of a rectangle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    N,
    S,
    E,
    W,
}

/// A rectangle in global compositor logical coordinates, as `(left, top, right,
/// bottom)`. The persisted form on disk is the bare `(i32, i32, i32, i32)` tuple
/// (see `to_tuple`/`from_tuple`); runtime code uses this named type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GlobalRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl GlobalRect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }

    /// Build from a `(left, top, right, bottom)` tuple — the on-disk persisted form.
    pub fn from_tuple((left, top, right, bottom): (i32, i32, i32, i32)) -> Self {
        Self { left, top, right, bottom }
    }

    /// Decompose into a `(left, top, right, bottom)` tuple — the on-disk persisted form.
    pub fn to_tuple(self) -> (i32, i32, i32, i32) {
        (self.left, self.top, self.right, self.bottom)
    }

    /// Order the corners so `left <= right` and `top <= bottom`.
    pub fn normalize(self) -> Self {
        Self {
            left: self.left.min(self.right),
            top: self.top.min(self.bottom),
            right: self.left.max(self.right),
            bottom: self.top.max(self.bottom),
        }
    }

    /// The corner handle (if any) within `radius` px of global point `g`.
    pub fn corner_at(self, g: (i32, i32), radius: f32) -> Option<Corner> {
        let near = |cx: i32, cy: i32| (g.0 - cx).abs() as f32 <= radius && (g.1 - cy).abs() as f32 <= radius;
        if near(self.left, self.top) {
            Some(Corner::Nw)
        } else if near(self.right, self.top) {
            Some(Corner::Ne)
        } else if near(self.left, self.bottom) {
            Some(Corner::Sw)
        } else if near(self.right, self.bottom) {
            Some(Corner::Se)
        } else {
            None
        }
    }

    /// The edge (if any) within `thickness` px of global point `g`, when `g` is within
    /// the rectangle's span on the perpendicular axis.
    pub fn edge_at(self, g: (i32, i32), thickness: f32) -> Option<Edge> {
        let on_x = g.0 >= self.left && g.0 <= self.right;
        let on_y = g.1 >= self.top && g.1 <= self.bottom;
        if on_x && (g.1 - self.top).abs() as f32 <= thickness {
            Some(Edge::N)
        } else if on_x && (g.1 - self.bottom).abs() as f32 <= thickness {
            Some(Edge::S)
        } else if on_y && (g.0 - self.left).abs() as f32 <= thickness {
            Some(Edge::W)
        } else if on_y && (g.0 - self.right).abs() as f32 <= thickness {
            Some(Edge::E)
        } else {
            None
        }
    }

    /// The edge whose CENTERED handle is under `g`: within `perp` px of the side AND within
    /// `half_len` of that side's midpoint. A bigger, easier resize target than the thin edge
    /// band, without covering the whole wall (DRAGON-208 — the whole edge still resizes via
    /// [`edge_at`]; this just makes the wall handle the easy hit).
    pub fn edge_handle_at(self, g: (i32, i32), half_len: f32, perp: f32) -> Option<Edge> {
        let midx = (self.left + self.right) as f32 / 2.0;
        let midy = (self.top + self.bottom) as f32 / 2.0;
        let near_mx = (g.0 as f32 - midx).abs() <= half_len;
        let near_my = (g.1 as f32 - midy).abs() <= half_len;
        if near_mx && (g.1 - self.top).abs() as f32 <= perp {
            Some(Edge::N)
        } else if near_mx && (g.1 - self.bottom).abs() as f32 <= perp {
            Some(Edge::S)
        } else if near_my && (g.0 - self.left).abs() as f32 <= perp {
            Some(Edge::W)
        } else if near_my && (g.0 - self.right).abs() as f32 <= perp {
            Some(Edge::E)
        } else {
            None
        }
    }

    /// Whether global point `g` is strictly inside the rectangle (edges excluded).
    pub fn contains(self, g: (i32, i32)) -> bool {
        g.0 > self.left && g.0 < self.right && g.1 > self.top && g.1 < self.bottom
    }
}

impl From<(i32, i32, i32, i32)> for GlobalRect {
    fn from(t: (i32, i32, i32, i32)) -> Self {
        Self::from_tuple(t)
    }
}

impl From<GlobalRect> for (i32, i32, i32, i32) {
    fn from(r: GlobalRect) -> Self {
        r.to_tuple()
    }
}

// ── The overlay units bridge (DRAGON-448) ─────────────────────────────────────

/// THE bridge between the two coordinate spaces a capture overlay lives in.
///
/// **`OutputState` geometry is CAPTURE space.** That is PHYSICAL pixels on Windows and
/// points on macOS and Linux — the per-platform units contract documented on
/// [`crate::platform::backend::OutputDesc`]. Do not "fix" it: `screenshot::region`, the
/// WGC crop, `output_for_selection`, the frozen flats and every `Selection` that reaches
/// the capture path all consume it exactly as it is.
///
/// **Everything iced hands us or renders is POINT space.** winit sizes the overlay window
/// by the monitor's own scale factor, so a 3840x2160 display at 300% gives iced a
/// 1280x720 viewport. Pointer positions, widget bounds, `Padding`, `Rectangle`s — points,
/// all of them.
///
/// **These two functions are the only bridge**: [`OverlayUnits::to_capture`] going in
/// (pointer/geometry from iced → the physical `Selection` the capture path expects) and
/// [`OverlayUnits::to_point`] going out (output geometry → where a widget is placed).
/// Nothing else may multiply or divide by an output's scale. A stray `* scale` at a call
/// site is exactly the DRAGON-448 bug: at scale S the rubber band and the committed rect
/// came back S× off and the toolbar was placed outside the viewport, on every Windows
/// machine above 100% scaling — 125% and 150% laptop defaults included.
///
/// `factor` is CAPTURE units per POINT for ONE output. Windows: that monitor's `dpi / 96`.
/// macOS: `1.0` — its `OutputDesc`s are points AND its overlay app-space is points, so a
/// Retina Mac is 1.0 HERE even though its backing scale is 2.0 (do not confuse this with
/// `source_scale`, a different and already-correct thing about captured MEDIA). Linux:
/// `1.0` — a layer surface's app space is points. Multi-monitor is per-OUTPUT: a 100% +
/// 300% pair carries one of these EACH, never one global scale.
///
/// At `factor == 1.0` every conversion here is the exact identity (an f32 multiply and
/// divide by 1.0 are bit-exact), which is what keeps Linux, macOS and every 96-DPI
/// Windows box byte-identical.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayUnits {
    /// The output's top-left in CAPTURE space (`OutputState::logical_pos`).
    origin: (i32, i32),
    /// CAPTURE units per POINT for this output. Always finite and `> 0`.
    factor: f32,
}

impl OverlayUnits {
    /// Build a bridge for one output. A non-finite or non-positive `factor` degrades to
    /// the identity rather than being trusted: a zero would divide the whole overlay to
    /// infinity, and a negative would mirror it — both far worse than "unscaled".
    pub fn new(origin: (i32, i32), factor: f32) -> Self {
        let factor = if factor.is_finite() && factor > 0.0 { factor } else { 1.0 };
        Self { origin, factor }
    }

    /// The unscaled bridge — capture space IS point space, only shifted by `origin`. The
    /// Linux/macOS case, and what every pre-DRAGON-448 call site did implicitly.
    pub const fn identity(origin: (i32, i32)) -> Self {
        Self { origin, factor: 1.0 }
    }

    /// The output's top-left in CAPTURE space.
    pub fn origin(self) -> (i32, i32) {
        self.origin
    }

    /// CAPTURE units per POINT.
    pub fn factor(self) -> f32 {
        self.factor
    }

    /// **IN**: a surface-local POINT (as iced reports it) → a global CAPTURE coordinate.
    ///
    /// This is what builds the physical `Selection` a capture consumes. The truncating
    /// `as i32` is deliberate and matches what the region widget always did, so a 1.0
    /// factor lands on the same integer it used to.
    pub fn to_capture(self, p: (f32, f32)) -> (i32, i32) {
        (
            (p.0 * self.factor) as i32 + self.origin.0,
            (p.1 * self.factor) as i32 + self.origin.1,
        )
    }

    /// **OUT**: a global CAPTURE coordinate → the surface-local POINT to draw/place it at.
    pub fn to_point(self, g: (i32, i32)) -> (f32, f32) {
        (
            (g.0 - self.origin.0) as f32 / self.factor,
            (g.1 - self.origin.1) as f32 / self.factor,
        )
    }

    /// A CAPTURE-space LENGTH in points (no origin shift) — widths, heights, radii.
    pub fn len_to_point(self, n: f32) -> f32 {
        n / self.factor
    }

    /// A POINT-space LENGTH in capture units — the on-screen "feel" constants (grab radii,
    /// drag thresholds) that must stay the same SIZE on screen at any scale.
    pub fn len_to_capture(self, n: f32) -> f32 {
        n * self.factor
    }

    /// An output's CAPTURE size (`OutputState::logical_size`) as the POINT extent of the
    /// surface iced laid out for it — the viewport every overlay layout must fit inside.
    pub fn size_to_point(self, size: (u32, u32)) -> (f32, f32) {
        (self.len_to_point(size.0 as f32), self.len_to_point(size.1 as f32))
    }
}

/// Whether global point `g` lies inside the convex quad `poly` (corners in order) — true
/// when `g` is on the same side of all four edges. Works for either winding.
pub fn point_in_quad(g: (i32, i32), poly: &[(i32, i32); 4]) -> bool {
    let mut sign = 0i64;
    for i in 0..4 {
        let (ax, ay) = poly[i];
        let (bx, by) = poly[(i + 1) % 4];
        let cross = (bx - ax) as i64 * (g.1 - ay) as i64 - (by - ay) as i64 * (g.0 - ax) as i64;
        if cross != 0 {
            if sign == 0 {
                sign = cross.signum();
            } else if cross.signum() != sign {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_orders_corners() {
        assert_eq!(GlobalRect::new(10, 20, 5, 8).normalize(), GlobalRect::new(5, 8, 10, 20));
        assert_eq!(GlobalRect::new(0, 0, 4, 4).normalize(), GlobalRect::new(0, 0, 4, 4));
    }

    #[test]
    fn contains_excludes_edges() {
        let r = GlobalRect::new(0, 0, 10, 10);
        assert!(r.contains((5, 5)));
        assert!(!r.contains((0, 5)), "left edge is not strictly inside");
        assert!(!r.contains((5, 10)), "bottom edge is not strictly inside");
        assert!(!r.contains((20, 20)));
    }

    #[test]
    fn corner_at_picks_the_nearest_handle() {
        let r = GlobalRect::new(0, 0, 100, 100);
        assert_eq!(r.corner_at((2, 2), 16.0), Some(Corner::Nw));
        assert_eq!(r.corner_at((98, 2), 16.0), Some(Corner::Ne));
        assert_eq!(r.corner_at((2, 98), 16.0), Some(Corner::Sw));
        assert_eq!(r.corner_at((98, 98), 16.0), Some(Corner::Se));
        assert_eq!(r.corner_at((50, 50), 16.0), None);
    }

    #[test]
    fn edge_at_picks_the_side() {
        let r = GlobalRect::new(0, 0, 100, 100);
        assert_eq!(r.edge_at((50, 1), 8.0), Some(Edge::N));
        assert_eq!(r.edge_at((50, 99), 8.0), Some(Edge::S));
        assert_eq!(r.edge_at((1, 50), 8.0), Some(Edge::W));
        assert_eq!(r.edge_at((99, 50), 8.0), Some(Edge::E));
        assert_eq!(r.edge_at((50, 50), 8.0), None);
    }

    #[test]
    fn edge_handle_at_is_a_bigger_centered_target() {
        let r = GlobalRect::new(0, 0, 200, 100);
        // Centre of the top wall, within the perpendicular tolerance -> N.
        assert_eq!(r.edge_handle_at((100, 5), 30.0, 12.0), Some(Edge::N));
        // A point too far along the wall from the midpoint -> not the handle (the thin
        // edge_at band still covers the rest of the wall).
        assert_eq!(r.edge_handle_at((160, 2), 30.0, 12.0), None);
        // The handle reaches FARTHER from the edge (perp 12) than edge_at's band would.
        assert_eq!(r.edge_handle_at((100, 11), 30.0, 12.0), Some(Edge::N));
        assert_eq!(r.edge_at((100, 11), 8.0), None);
        // Left/right walls use the vertical midpoint.
        assert_eq!(r.edge_handle_at((3, 50), 30.0, 12.0), Some(Edge::W));
        assert_eq!(r.edge_handle_at((197, 50), 30.0, 12.0), Some(Edge::E));
    }

    #[test]
    fn point_in_quad_handles_either_winding() {
        let cw = [(0, 0), (10, 0), (10, 10), (0, 10)];
        assert!(point_in_quad((5, 5), &cw));
        assert!(!point_in_quad((15, 5), &cw));
        let ccw = [(0, 0), (0, 10), (10, 10), (10, 0)];
        assert!(point_in_quad((5, 5), &ccw));
    }
}

#[cfg(test)]
mod overlay_units_tests {
    use super::OverlayUnits;

    /// THE regression pin (DRAGON-448): at factor 1.0 every conversion is the EXACT
    /// identity of what shipped before — a shifted truncation in, a shifted widening out.
    /// This is the dev box (96 DPI), Linux, macOS, and every existing test, so if this
    /// ever moves the whole fix has changed behaviour it had no business changing.
    #[test]
    fn factor_one_is_byte_identical_to_the_old_plain_offset() {
        for origin in [(0, 0), (1920, 0), (-2560, -1440), (37, -11)] {
            let u = OverlayUnits::new(origin, 1.0);
            assert_eq!(u.factor(), 1.0);
            assert_eq!(u.origin(), origin);
            for p in [(0.0f32, 0.0f32), (10.4, 10.6), (1919.9, 1079.2), (0.5, 0.5)] {
                // The old widget did exactly `p.x as i32 + origin.0`.
                assert_eq!(u.to_capture(p), (p.0 as i32 + origin.0, p.1 as i32 + origin.1));
            }
            for g in [(0, 0), (5, 7), (-9, 3), (4000, 2200)] {
                // The old draw path did exactly `(g - origin) as f32`.
                assert_eq!(
                    u.to_point(g),
                    ((g.0 - origin.0) as f32, (g.1 - origin.1) as f32)
                );
            }
            for n in [0.0f32, 8.0, 16.5, 1080.0] {
                assert_eq!(u.len_to_point(n), n);
                assert_eq!(u.len_to_capture(n), n);
            }
            assert_eq!(u.size_to_point((1920, 1080)), (1920.0, 1080.0));
        }
    }

    /// A pointer POINT becomes the PHYSICAL selection coordinate the capture path wants,
    /// at every Windows scale step — including on a monitor whose physical origin is not
    /// zero. The contract is `origin + point * factor`, so viewport (10,10) on a 300%
    /// monitor starting at physical x=3840 is physical (3870, 3870-3840 = 30 down from
    /// its own top): global (3870, 30) when that monitor's top is 0.
    #[test]
    fn a_pointer_point_maps_to_the_physical_selection() {
        for (factor, want) in [
            (1.0f32, (3850, 10)),
            (1.25, (3852, 12)),
            (1.5, (3855, 15)),
            (2.0, (3860, 20)),
            (3.0, (3870, 30)),
        ] {
            let u = OverlayUnits::new((3840, 0), factor);
            assert_eq!(u.to_capture((10.0, 10.0)), want, "factor {factor}");
        }
        // The origin is added AFTER scaling, never scaled itself — scaling it would put
        // the selection on a different monitor entirely.
        let u = OverlayUnits::new((3840, 2160), 3.0);
        assert_eq!(u.to_capture((0.0, 0.0)), (3840, 2160));
        // A monitor above/left of the primary carries negative capture coords.
        let u = OverlayUnits::new((-2560, -1440), 2.0);
        assert_eq!(u.to_capture((100.0, 50.0)), (-2360, -1340));
    }

    /// The round trip point → physical → point is the identity (within the float epsilon
    /// the truncation allows), so a committed rect lands back exactly where it was drawn.
    #[test]
    fn the_round_trip_is_the_identity() {
        for factor in [1.0f32, 1.25, 1.5, 2.0, 3.0] {
            for origin in [(0, 0), (3840, 0), (-1280, -720)] {
                let u = OverlayUnits::new(origin, factor);
                for p in [(0.0f32, 0.0f32), (10.0, 10.0), (640.0, 360.0), (1279.0, 719.0)] {
                    let back = u.to_point(u.to_capture(p));
                    // `to_capture` truncates to a whole capture unit, so the return trip
                    // can only lose sub-capture-unit detail: under one point at 1x, less
                    // at any higher factor.
                    assert!(
                        (back.0 - p.0).abs() < 1.0 && (back.1 - p.1).abs() < 1.0,
                        "factor {factor} origin {origin:?}: {p:?} -> {back:?}"
                    );
                }
                // Whole capture coordinates round-trip EXACTLY the other way.
                for g in [origin, (origin.0 + 300, origin.1 + 600)] {
                    assert_eq!(u.to_capture(u.to_point(g)), g, "factor {factor}");
                }
            }
        }
    }

    /// A two-monitor desktop with DIFFERENT factors resolves each overlay independently —
    /// the same viewport point on each maps onto its OWN monitor, never through one global
    /// scale. This is the mixed-DPI case a single shared factor would silently ruin.
    #[test]
    fn each_monitor_carries_its_own_factor() {
        // Left: 3840x2160 at 300% (1280x720 points). Right: 1920x1080 at 100%, abutting
        // it at physical x=3840.
        let hi = OverlayUnits::new((0, 0), 3.0);
        let lo = OverlayUnits::new((3840, 0), 1.0);
        assert_eq!(hi.size_to_point((3840, 2160)), (1280.0, 720.0));
        assert_eq!(lo.size_to_point((1920, 1080)), (1920.0, 1080.0));
        // The SAME viewport point lands in a different place on each.
        assert_eq!(hi.to_capture((100.0, 100.0)), (300, 300));
        assert_eq!(lo.to_capture((100.0, 100.0)), (3940, 100));
        // Each overlay's own bottom-right point maps to its own monitor's far corner.
        assert_eq!(hi.to_capture((1280.0, 720.0)), (3840, 2160));
        assert_eq!(lo.to_capture((1920.0, 1080.0)), (5760, 1080));
        // And a capture coordinate on the low-DPI monitor is NOT reachable through the
        // high-DPI bridge's point space (it lands well past its 1280x720 viewport).
        let (px, _) = hi.to_point((4000, 0));
        assert!(px > 1280.0, "cross-monitor point {px} must fall outside this viewport");
    }

    /// The output's point extent IS the iced viewport, at every scale step — the number
    /// every overlay layout has to fit inside.
    #[test]
    fn the_point_size_is_the_iced_viewport() {
        for (px, factor, want) in [
            ((1920u32, 1080u32), 1.0f32, (1920.0f32, 1080.0f32)),
            ((2560, 1440), 1.25, (2048.0, 1152.0)),
            ((3840, 2160), 1.5, (2560.0, 1440.0)),
            ((3840, 2160), 2.0, (1920.0, 1080.0)),
            ((3840, 2160), 3.0, (1280.0, 720.0)),
        ] {
            assert_eq!(OverlayUnits::new((0, 0), factor).size_to_point(px), want);
        }
    }

    /// A broken factor can only ever be the identity — never a zero (which would divide
    /// the layout to infinity) or a negative (which would mirror it).
    #[test]
    fn a_broken_factor_degrades_to_the_identity() {
        for bad in [0.0f32, -2.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let u = OverlayUnits::new((10, 20), bad);
            assert_eq!(u.factor(), 1.0, "{bad} must not be trusted");
            assert_eq!(u.to_capture((5.0, 5.0)), (15, 25));
            assert_eq!(u.to_point((15, 25)), (5.0, 5.0));
        }
        // A sub-1.0 factor is NOT nonsense (a hypothetical down-scaled output) and is
        // honoured; only the impossible values degrade.
        assert_eq!(OverlayUnits::new((0, 0), 0.5).factor(), 0.5);
    }

    /// Lengths convert without the origin: a grab radius in points is the same SIZE on
    /// screen at any scale, and a capture extent reads back as its point width.
    #[test]
    fn lengths_ignore_the_origin() {
        let u = OverlayUnits::new((3840, 2160), 2.0);
        assert_eq!(u.len_to_capture(16.0), 32.0);
        assert_eq!(u.len_to_point(32.0), 16.0);
        assert_eq!(u.len_to_point(u.len_to_capture(8.0)), 8.0);
    }
}
