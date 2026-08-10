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

    /// **Pure**, unit-tested: this rectangle TRANSLATED by `(dx, dy)`, kept inside `bounds`
    /// (DRAGON-599, the arrow / `hjkl` region nudge).
    ///
    /// **It moves and it never resizes.** The size is preserved by construction, because the
    /// clamp is applied to the TRANSLATION and then the whole rectangle is shifted by it,
    /// rather than to the four edges independently. Clamping edges is the obvious way to write
    /// this and it is wrong: pushing a rectangle into a wall would shrink it against that wall
    /// and the user would silently lose a region they had sized deliberately. Walking into a
    /// wall stops the move instead.
    ///
    /// The two axes are independent, so sliding along a wall works: a region already flush
    /// against the top still moves left and right, and only the vertical half of a diagonal
    /// step is refused.
    ///
    /// A rectangle WIDER than the bounds cannot satisfy both walls at once, so that axis
    /// refuses to move at all rather than picking a wall to favour. That is reachable in
    /// practice: a region dragged across two monitors is wider than either one, and on a
    /// non-rectangular desktop the bounding box has corners no output covers.
    ///
    /// A rectangle ALREADY outside the bounds (a display was unplugged under a remembered
    /// region) can still be walked back IN, one step at a time, but never further OUT, and
    /// never in the direction opposite the key. Snapping it flush in one tap would be a jump
    /// nobody asked for, and letting it drift further out would strand it for good.
    ///
    /// `self` is normalized first, so a rectangle dragged right-to-left (which is stored with
    /// `left > right`) moves the same way as one dragged the other way. `bounds` is expected
    /// normalized, as `(left, top, right, bottom)`.
    pub fn nudged(self, dx: i32, dy: i32, bounds: (i32, i32, i32, i32)) -> Self {
        let r = self.normalize();
        let (bl, bt, br, bb) = bounds;
        // The allowed travel on one axis: at most `d`, never past a border, never further out
        // than the rectangle already is, and ZERO when the span does not fit at all. The two
        // `room` terms are signed and straddle zero, so the clamp can never invert.
        let travel = |d: i32, lo: i32, hi: i32, blo: i32, bhi: i32| -> i32 {
            if hi - lo > bhi - blo {
                return 0;
            }
            let room_back = (blo - lo).min(0);
            let room_on = (bhi - hi).max(0);
            d.clamp(room_back, room_on)
        };
        let mx = travel(dx, r.left, r.right, bl, br);
        let my = travel(dy, r.top, r.bottom, bt, bb);
        Self {
            left: r.left + mx,
            top: r.top + my,
            right: r.right + mx,
            bottom: r.bottom + my,
        }
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
/// The Linux portal-frozen FALLBACK overlay (`lab/flatpak`) adds a third kind of bridge:
/// a sandboxed / layer-shell-less session draws region selection in ONE fullscreen
/// toplevel over a frozen frame of the PORTAL-GRANTED monitor, and the compositor, not
/// us, decides which monitor that toplevel maps on. When it lands on a monitor of a
/// different geometry, the frame is shown LETTERBOXED ([`Self::letterbox`]): one uniform
/// scale that keeps its aspect, capped at 1 so a smaller frame is never enlarged,
/// centred, bars on the leftover axis (all four sides when the cap bites). Round 1 shipped a
/// PER-AXIS stretch instead (`ContentFit::Fill` plus a `stretched()` constructor); it
/// mapped correctly but DISTORTED the still whenever the shapes differed, and the owner
/// requires the captured frame keep its aspect, so the per-axis form is gone and must not
/// come back.
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
    /// POINT-space position of the frame's first pixel inside the surface: the letterbox
    /// bars of the fallback bridge. `(0.0, 0.0)` on every uniform bridge, where the
    /// surface shows capture pixels edge to edge (subtracting and adding a literal zero
    /// is bit-exact, which is what keeps those bridges byte-identical).
    offset: (f32, f32),
    /// The letterbox extras. `None` on every uniform bridge.
    letterbox: Option<Letterbox>,
}

/// The letterbox bridge's extras (`lab/flatpak`), absent on every uniform bridge.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Letterbox {
    /// The frozen frame's CAPTURE extent: what [`OverlayUnits::to_capture`] clamps into
    /// and what [`OverlayUnits::visible_capture_size`] reports.
    frame: (u32, u32),
    /// The fallback window's POINT extent: the real iced viewport
    /// ([`OverlayUnits::size_to_point`]'s answer).
    viewport: (f32, f32),
    /// The frame's displayed POINT extent (`frame * scale`): the letterbox destination
    /// the backdrop draws the still at ([`OverlayUnits::letterbox_dest`]).
    dest: (f32, f32),
}

impl OverlayUnits {
    /// Guard a factor: non-finite or non-positive degrades to the identity rather
    /// than being trusted: a zero would divide the whole overlay to infinity, and a
    /// negative would mirror it, both far worse than "unscaled".
    fn guard(factor: f32) -> f32 {
        if factor.is_finite() && factor > 0.0 { factor } else { 1.0 }
    }

    /// Build a UNIFORM bridge for one output. A non-finite or non-positive `factor`
    /// degrades to the identity (see [`Self::guard`]).
    pub fn new(origin: (i32, i32), factor: f32) -> Self {
        Self { origin, factor: Self::guard(factor), offset: (0.0, 0.0), letterbox: None }
    }

    /// Pure, unit-tested: build the LETTERBOX bridge, the Linux fallback overlay's
    /// mismatch guard (`lab/flatpak`). `frame` is the portal-granted monitor's logical
    /// size (the frozen frame's capture extent); `win` is the fullscreen toplevel's
    /// ACTUAL size in points, as its resize event reported it. Wayland gives a client no
    /// say in which monitor a fullscreen toplevel maps on, so the two can disagree.
    ///
    /// The mapping is uniform-scale-plus-offset: `scale = min(win/frame)` per the two
    /// axes, CAPPED at 1 (POINTS per capture unit, so the WHOLE frame fits at its own
    /// aspect and is never enlarged), and the offsets centre the displayed frame,
    /// leaving equal bars on the leftover axis. The cap is the owner's call from the
    /// third live test: a smaller monitor's capture must not be blown up, so a frame
    /// smaller than the window renders at NATIVE size, centred, bars on all four
    /// sides. A window point maps into the frame by
    /// subtracting the offsets and dividing by the scale; points in the BARS land
    /// outside the frame and CLAMP to its edge ([`Self::to_capture`]), which is what
    /// confines selection to visible pixels. A matching window computes scale 1 and
    /// zero offsets, identical to the uniform bridge over the whole window. Degenerate
    /// inputs (zero frame; zero / negative / non-finite window) degrade to the plain
    /// uniform identity, the same safe side [`Self::guard`] takes.
    // Its one production caller is the Linux `OutputState::units`; compiled into every
    // test build so the mapping is proven on any host (the house pattern).
    #[cfg(any(target_os = "linux", test))]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn letterbox(origin: (i32, i32), frame: (u32, u32), win: (f32, f32)) -> Self {
        let ok = |v: f32| v.is_finite() && v >= 1.0;
        if frame.0 == 0 || frame.1 == 0 || !ok(win.0) || !ok(win.1) {
            return Self::new(origin, 1.0);
        }
        let (fw, fh) = (frame.0 as f32, frame.1 as f32);
        // POINTS per CAPTURE unit: the one uniform scale at which the whole frame fits,
        // capped at 1 so a frame smaller than the window is shown at native size (bars
        // all around) instead of blown up. The clamp geometry below reads the capped
        // dest, so point mapping and selection confinement follow the cap for free.
        let scale = (win.0 / fw).min(win.1 / fh).min(1.0);
        // The displayed extent comes from the scale DIRECTLY (not back through the
        // reciprocal factor), so friendly ratios stay exact and the offsets derived
        // from it centre the frame without float drift.
        let dest = (fw * scale, fh * scale);
        let offset = (
            ((win.0 - dest.0) / 2.0).max(0.0),
            ((win.1 - dest.1) / 2.0).max(0.0),
        );
        Self {
            origin,
            factor: Self::guard(1.0 / scale),
            offset,
            letterbox: Some(Letterbox { frame, viewport: win, dest }),
        }
    }

    /// The unscaled bridge — capture space IS point space, only shifted by `origin`. The
    /// Linux/macOS case, and what every pre-DRAGON-448 call site did implicitly.
    pub const fn identity(origin: (i32, i32)) -> Self {
        Self { origin, factor: 1.0, offset: (0.0, 0.0), letterbox: None }
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
    /// factor lands on the same integer it used to. On the letterbox bridge the result
    /// is CLAMPED into the frame before the origin shift: a point in the bars has no
    /// captured pixel under it, so it maps to the nearest frame edge instead of past it.
    pub fn to_capture(self, p: (f32, f32)) -> (i32, i32) {
        let (cx, cy) = self.capture_offset_f(p);
        (cx as i32 + self.origin.0, cy as i32 + self.origin.1)
    }

    /// Pure, unit-tested: the FRACTIONAL capture offset of a surface-local POINT from
    /// this output's own top-left, letterbox clamp applied, origin NOT added.
    ///
    /// The float half of [`Self::to_capture`], which is now written in terms of it, so
    /// there is exactly one copy of the scale-and-clamp arithmetic. `to_capture` is
    /// byte-identical to before: it still truncates the OFFSET and adds the origin
    /// afterwards, which is not the same as truncating the sum on a monitor whose origin
    /// is negative.
    ///
    /// It exists because the colour picker (DRAGON-582) must resolve a POINT to a single
    /// SOURCE PIXEL, and the truncation `to_capture` performs throws away exactly the
    /// sub-capture-unit precision that decides which pixel that is. On a HiDPI output the
    /// snapshot has two or three image pixels per capture unit, so a truncated coordinate
    /// can never address the last column of the last unit, and the screen's furthest edge
    /// pixel would be unreachable.
    pub fn capture_offset_f(self, p: (f32, f32)) -> (f32, f32) {
        let cx = (p.0 - self.offset.0) * self.factor;
        let cy = (p.1 - self.offset.1) * self.factor;
        match self.letterbox {
            Some(lb) => (
                cx.clamp(0.0, lb.frame.0 as f32),
                cy.clamp(0.0, lb.frame.1 as f32),
            ),
            None => (cx, cy),
        }
    }

    /// **OUT**: a global CAPTURE coordinate → the surface-local POINT to draw/place it at.
    pub fn to_point(self, g: (i32, i32)) -> (f32, f32) {
        (
            (g.0 - self.origin.0) as f32 / self.factor + self.offset.0,
            (g.1 - self.origin.1) as f32 / self.factor + self.offset.1,
        )
    }

    /// A CAPTURE-space LENGTH in points (no origin shift, no letterbox offset). Kept for
    /// the on-screen "feel" constants; a WIDTH-AND-HEIGHT pair reads better through the
    /// pair form [`Self::size_f_to_point`].
    pub fn len_to_point(self, n: f32) -> f32 {
        n / self.factor
    }

    /// A POINT-space LENGTH in capture units — the on-screen "feel" constants (grab radii,
    /// drag thresholds) that must stay the same SIZE on screen at any scale. The pair
    /// form is [`Self::size_to_capture`].
    pub fn len_to_capture(self, n: f32) -> f32 {
        n * self.factor
    }

    /// A CAPTURE-space (width, height) extent in points: the pair convenience of
    /// [`Self::len_to_point`]. Extents carry no origin and no letterbox offset; both
    /// axes share the one uniform factor (the round-1 per-axis form died with the
    /// stretched bridge, see the type doc).
    pub fn size_f_to_point(self, size: (f32, f32)) -> (f32, f32) {
        (size.0 / self.factor, size.1 / self.factor)
    }

    /// A POINT-space (width, height) extent in capture units: the pair counterpart of
    /// [`Self::size_f_to_point`].
    pub fn size_to_capture(self, size: (f32, f32)) -> (f32, f32) {
        (size.0 * self.factor, size.1 * self.factor)
    }

    /// An output's CAPTURE size (`OutputState::logical_size`) as the POINT extent of the
    /// surface iced laid out for it — the viewport every overlay layout must fit inside.
    /// On the letterbox bridge that surface is the fallback WINDOW, whose real size the
    /// constructor was handed, so the answer is the window's extent regardless of `size`
    /// (the frame's own displayed extent is smaller by the bars; [`Self::letterbox_dest`]
    /// carries that one).
    pub fn size_to_point(self, size: (u32, u32)) -> (f32, f32) {
        match self.letterbox {
            Some(lb) => lb.viewport,
            None => (size.0 as f32 / self.factor, size.1 as f32 / self.factor),
        }
    }

    /// Pure, unit-tested: the CAPTURE extent of the pixels a viewport of `size` POINTS
    /// actually SHOWS. A uniform bridge shows capture pixels edge to edge, so this is
    /// exactly [`Self::size_to_capture`]; the letterbox bridge's bars show NOTHING, so
    /// the answer is the frozen frame's own extent. This is what confines the
    /// region-selection walls to the visible image instead of the whole window
    /// (`lab/flatpak`).
    pub fn visible_capture_size(self, size: (f32, f32)) -> (f32, f32) {
        match self.letterbox {
            Some(lb) => (lb.frame.0 as f32, lb.frame.1 as f32),
            None => self.size_to_capture(size),
        }
    }

    /// The POINT-space placement of the frozen frame inside the fallback window: the
    /// letterbox `(offset, displayed extent)` the backdrop must draw the still at, so
    /// the pixels on screen and [`Self::to_capture`]'s mapping share ONE math source and
    /// cannot drift. `None` on every uniform bridge, which is what keeps the layer-shell
    /// backdrop path byte-identical.
    // Its one production caller is the Linux fallback backdrop (`with_frozen_bg`);
    // compiled into every test build so the placement is proven on any host.
    #[cfg(any(target_os = "linux", test))]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn letterbox_dest(self) -> Option<((f32, f32), (f32, f32))> {
        let lb = self.letterbox?;
        Some((self.offset, lb.dest))
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

#[cfg(test)]
mod letterbox_tests {
    use super::OverlayUnits;

    /// A matching window (the common case: the fullscreen toplevel landed on the granted
    /// monitor) is byte-identical to the plain uniform bridge over the whole window:
    /// scale 1, zero offsets, and the clamp inert because every window point is already
    /// inside the frame.
    #[test]
    fn a_matching_window_is_byte_identical_to_the_uniform_bridge() {
        for origin in [(0, 0), (1920, 200), (-2560, -1440)] {
            let lb = OverlayUnits::letterbox(origin, (1920, 1080), (1920.0, 1080.0));
            let plain = OverlayUnits::new(origin, 1.0);
            assert_eq!(lb.factor(), 1.0);
            for p in [(0.0f32, 0.0f32), (10.4, 10.6), (960.0, 540.0), (1920.0, 1080.0)] {
                assert_eq!(lb.to_capture(p), plain.to_capture(p), "origin {origin:?} p {p:?}");
            }
            for g in [origin, (origin.0 + 300, origin.1 + 600), (origin.0 + 1920, origin.1 + 1080)] {
                assert_eq!(lb.to_point(g), plain.to_point(g), "origin {origin:?} g {g:?}");
            }
            // The viewport is the window; the frame fills it edge to edge.
            assert_eq!(lb.size_to_point((1920, 1080)), (1920.0, 1080.0));
            assert_eq!(lb.visible_capture_size((1920.0, 1080.0)), (1920.0, 1080.0));
            assert_eq!(lb.letterbox_dest(), Some(((0.0, 0.0), (1920.0, 1080.0))));
        }
    }

    /// A window with LESS height per width than the frame needs (an ultrawide frame on a
    /// 16:9 monitor) letterboxes with TOP/BOTTOM bars: uniform scale from the width, the
    /// frame centred vertically, and bar points clamping to the frame's edge rows.
    #[test]
    fn an_ultrawide_frame_letterboxes_top_and_bottom() {
        // Granted monitor 5120x1440 at (0, 0); the toplevel landed on 1920x1080.
        // scale = min(1920/5120, 1080/1440) = 0.375: shown 1920x540, bars 270 each.
        let u = OverlayUnits::letterbox((0, 0), (5120, 1440), (1920.0, 1080.0));
        assert_eq!(u.letterbox_dest(), Some(((0.0, 270.0), (1920.0, 540.0))));
        // The displayed frame's corners map onto the frame's corners (the far one is
        // pinned by the clamp), and the window centre is the frame centre: a uniform
        // scale keeps the aspect, where the round-1 per-axis stretch did not.
        assert_eq!(u.to_capture((0.0, 270.0)), (0, 0));
        assert_eq!(u.to_capture((1920.0, 810.0)), (5120, 1440));
        assert_eq!(u.to_capture((960.0, 540.0)), (2560, 720));
        // Bar points have no captured pixel under them: they CLAMP to the frame edge
        // instead of mapping past it (the window's own top/bottom rows included).
        assert_eq!(u.to_capture((500.0, 0.0)), (1333, 0));
        assert_eq!(u.to_capture((500.0, 1080.0)), (1333, 1440));
        // The viewport the layout must fit inside is the WINDOW's size; the pixels the
        // selection walls confine to are the FRAME's.
        assert_eq!(u.size_to_point((5120, 1440)), (1920.0, 1080.0));
        assert_eq!(u.visible_capture_size((1920.0, 1080.0)), (5120.0, 1440.0));
        // Whole capture coordinates inside the frame survive the round trip to within
        // the one capture unit the truncation allows.
        for g in [(0, 0), (2560, 720), (5119, 1439)] {
            let back = u.to_capture(u.to_point(g));
            assert!(
                (back.0 - g.0).abs() <= 1 && (back.1 - g.1).abs() <= 1,
                "{g:?} -> {back:?}"
            );
        }
        // A non-origin monitor keeps its origin unshifted and unscaled, applied AFTER
        // the clamp, exactly like the uniform form.
        let u = OverlayUnits::letterbox((1920, 200), (5120, 1440), (1920.0, 1080.0));
        assert_eq!(u.to_capture((0.0, 0.0)), (1920, 200));
    }

    /// A window with MORE width per height than the frame needs letterboxes with SIDE
    /// bars: uniform scale from the height, the frame centred horizontally, and points
    /// in either bar clamping to the frame's edge columns.
    #[test]
    fn a_wider_window_letterboxes_the_sides() {
        // Granted monitor 1920x1080 at (100, 50); the toplevel landed on 2560x1080.
        // scale = min(2560/1920, 1080/1080) = 1.0: shown 1920x1080, bars 320 each side.
        let u = OverlayUnits::letterbox((100, 50), (1920, 1080), (2560.0, 1080.0));
        assert_eq!(u.factor(), 1.0);
        assert_eq!(u.letterbox_dest(), Some(((320.0, 0.0), (1920.0, 1080.0))));
        // The displayed frame's corners map onto the frame's corners.
        assert_eq!(u.to_capture((320.0, 0.0)), (100, 50));
        assert_eq!(u.to_capture((2240.0, 1080.0)), (2020, 1130));
        // A left-bar point clamps to the frame's left edge; a right-bar point (and the
        // window's own far corner) to the right edge.
        assert_eq!(u.to_capture((0.0, 540.0)), (100, 590));
        assert_eq!(u.to_capture((2560.0, 1080.0)), (2020, 1130));
        // And the draw direction places frame coordinates at their on-screen spot,
        // inside the bars.
        assert_eq!(u.to_point((100, 50)), (320.0, 0.0));
        assert_eq!(u.to_point((2020, 1130)), (2240.0, 1080.0));
        assert_eq!(u.size_to_point((1920, 1080)), (2560.0, 1080.0));
        assert_eq!(u.visible_capture_size((2560.0, 1080.0)), (1920.0, 1080.0));
    }

    /// A frame SMALLER than the window in both axes renders at NATIVE size, centred,
    /// bars on all four sides. The scale is capped at 1: this test's first shape once
    /// upscaled to fill (scale 1.5, no bars), and the owner's third live test overruled
    /// that: a smaller monitor's capture must not be blown up. The clamp geometry reads
    /// the capped dest, so every bar edge confines selection to the native-size frame.
    #[test]
    fn a_smaller_frame_renders_at_native_size_with_bars_all_around() {
        // 1280x720 frame on a 1920x1080 window: uncapped scale would be 1.5; capped it
        // is 1.0, shown 1280x720, bars 320 each side and 180 top/bottom.
        let u = OverlayUnits::letterbox((0, 0), (1280, 720), (1920.0, 1080.0));
        assert_eq!(u.factor(), 1.0, "capture unit per point stays 1:1, never magnified");
        let Some((offset, dest)) = u.letterbox_dest() else {
            panic!("a well-formed letterbox must carry a dest");
        };
        assert_eq!(offset, (320.0, 180.0), "the native-size frame is centred");
        assert_eq!(dest, (1280.0, 720.0), "the dest is the frame's own extent");
        // The displayed frame's corners and centre map onto the frame's own.
        assert_eq!(u.to_capture((320.0, 180.0)), (0, 0));
        assert_eq!(u.to_capture((1600.0, 900.0)), (1280, 720));
        assert_eq!(u.to_capture((960.0, 540.0)), (640, 360));
        // Points in all FOUR bars clamp to the nearest frame edge instead of mapping
        // past it: left, right, top, bottom, plus the window's own far corner.
        assert_eq!(u.to_capture((0.0, 540.0)), (0, 360));
        assert_eq!(u.to_capture((1920.0, 540.0)), (1280, 360));
        assert_eq!(u.to_capture((960.0, 0.0)), (640, 0));
        assert_eq!(u.to_capture((960.0, 1080.0)), (640, 720));
        assert_eq!(u.to_capture((1920.0, 1080.0)), (1280, 720));
        // The layout viewport is still the WINDOW; the selection walls confine to the
        // FRAME.
        assert_eq!(u.size_to_point((1280, 720)), (1920.0, 1080.0));
        assert_eq!(u.visible_capture_size((1920.0, 1080.0)), (1280.0, 720.0));
        // The origin stays unshifted and unscaled, applied AFTER the clamp.
        let u = OverlayUnits::letterbox((100, 50), (1280, 720), (1920.0, 1080.0));
        assert_eq!(u.to_capture((0.0, 0.0)), (100, 50));
        assert_eq!(u.to_point((100, 50)), (320.0, 180.0));
    }

    /// Degenerate sizes never poison the mapping: a zero frame or a zero / negative /
    /// non-finite window size degrades to the plain uniform identity (no clamp, no
    /// offset), instead of dividing the selection to infinity.
    #[test]
    fn degenerate_sizes_degrade_to_the_uniform_identity() {
        for (frame, win) in [
            ((0u32, 0u32), (1920.0f32, 1080.0f32)),
            ((1920, 0), (1920.0, 1080.0)),
            ((1920, 1080), (0.0, 1080.0)),
            ((1920, 1080), (f32::NAN, 1080.0)),
            ((1920, 1080), (1920.0, -5.0)),
            ((1920, 1080), (f32::INFINITY, 1080.0)),
        ] {
            let u = OverlayUnits::letterbox((10, 20), frame, win);
            assert_eq!(u, OverlayUnits::new((10, 20), 1.0), "frame {frame:?} win {win:?}");
            assert_eq!(u.letterbox_dest(), None);
            // The plain shifted mapping, unclamped — exactly the uniform bridge.
            assert_eq!(u.to_capture((5.0, 5.0)), (15, 25));
            assert_eq!(u.to_capture((-30.0, 5.0)), (-20, 25));
        }
    }
}

/// DRAGON-599: the keyboard nudge moves a drawn region and never resizes it.
#[cfg(test)]
mod nudged_tests {
    use super::GlobalRect;

    /// A 1920x1080 desktop at the origin, and a 200x100 region well inside it.
    const OUT: (i32, i32, i32, i32) = (0, 0, 1920, 1080);

    fn r() -> GlobalRect {
        GlobalRect::new(500, 400, 700, 500)
    }

    fn size(g: GlobalRect) -> (i32, i32) {
        (g.right - g.left, g.bottom - g.top)
    }

    /// The plain case: one unit per call, in each of the four directions, size untouched.
    #[test]
    fn one_step_moves_the_whole_rectangle() {
        assert_eq!(r().nudged(-1, 0, OUT), GlobalRect::new(499, 400, 699, 500));
        assert_eq!(r().nudged(1, 0, OUT), GlobalRect::new(501, 400, 701, 500));
        assert_eq!(r().nudged(0, -1, OUT), GlobalRect::new(500, 399, 700, 499));
        assert_eq!(r().nudged(0, 1, OUT), GlobalRect::new(500, 401, 700, 501));
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            assert_eq!(size(r().nudged(dx, dy, OUT)), size(r()), "({dx},{dy})");
        }
    }

    /// **The rule this function exists for.** At a wall the move STOPS; it does not shrink the
    /// region against the wall, which is what clamping the four edges instead of the
    /// translation would silently do. Every wall, with the size checked at each.
    #[test]
    fn a_wall_stops_the_move_and_never_shrinks_the_region() {
        let walls = [
            (GlobalRect::new(0, 400, 200, 500), (-1, 0)),
            (GlobalRect::new(1720, 400, 1920, 500), (1, 0)),
            (GlobalRect::new(500, 0, 700, 100), (0, -1)),
            (GlobalRect::new(500, 980, 700, 1080), (0, 1)),
        ];
        for (rect, (dx, dy)) in walls {
            let out = rect.nudged(dx, dy, OUT);
            assert_eq!(out, rect, "{rect:?} + ({dx},{dy}) must not move");
            assert_eq!(size(out), size(rect), "{rect:?} + ({dx},{dy}) must not resize");
        }
    }

    /// Every CORNER, both of its walls, and the diagonal into it. A corner is where an
    /// edge-clamping implementation shrinks in two directions at once.
    #[test]
    fn every_corner_holds_its_size() {
        let corners = [
            (GlobalRect::new(0, 0, 200, 100), (-1, -1)),
            (GlobalRect::new(1720, 0, 1920, 100), (1, -1)),
            (GlobalRect::new(0, 980, 200, 1080), (-1, 1)),
            (GlobalRect::new(1720, 980, 1920, 1080), (1, 1)),
        ];
        for (rect, (dx, dy)) in corners {
            let out = rect.nudged(dx, dy, OUT);
            assert_eq!(out, rect, "{rect:?} is cornered");
            assert_eq!(size(out), size(rect));
        }
    }

    /// The axes are independent, so a region flush against one wall still slides ALONG it.
    /// Without this, a region parked at the top edge would be stuck there.
    #[test]
    fn a_region_at_a_wall_still_slides_along_it() {
        let top = GlobalRect::new(500, 0, 700, 100);
        assert_eq!(top.nudged(-1, 0, OUT), GlobalRect::new(499, 0, 699, 100));
        assert_eq!(top.nudged(1, 0, OUT), GlobalRect::new(501, 0, 701, 100));
        // A DIAGONAL into that wall keeps its horizontal half.
        assert_eq!(top.nudged(1, -1, OUT), GlobalRect::new(501, 0, 701, 100));
    }

    /// A region that does not FIT on an axis refuses to move on it, rather than picking a wall
    /// to favour. Reachable in practice: a region dragged across two displays is wider than
    /// either, and a non-rectangular desktop's bounding box has corners no output covers. The
    /// other axis is unaffected.
    #[test]
    fn a_region_wider_than_the_bounds_refuses_that_axis() {
        let wide = GlobalRect::new(-50, 400, 2000, 500);
        assert_eq!(wide.nudged(-1, 0, OUT), wide);
        assert_eq!(wide.nudged(1, 0, OUT), wide);
        // Vertically it fits, so it still moves there.
        assert_eq!(wide.nudged(1, 1, OUT), GlobalRect::new(-50, 401, 2000, 501));
    }

    /// A remembered region left outside the bounds (a display was unplugged under it) walks
    /// back IN one step at a time, never further out, and never against the key.
    #[test]
    fn an_out_of_bounds_region_walks_back_in_but_never_further_out() {
        let off_right = GlobalRect::new(2000, 400, 2100, 500);
        assert_eq!(off_right.nudged(-1, 0, OUT), GlobalRect::new(1999, 400, 2099, 500));
        assert_eq!(off_right.nudged(1, 0, OUT), off_right, "never further out");
        let off_left = GlobalRect::new(-300, 400, -100, 500);
        assert_eq!(off_left.nudged(1, 0, OUT), GlobalRect::new(-299, 400, -99, 500));
        assert_eq!(off_left.nudged(-1, 0, OUT), off_left, "never further out");
    }

    /// A region dragged right-to-left is stored un-normalized (`left > right`). It moves the
    /// same way as one dragged the other way, and comes back normalized.
    #[test]
    fn an_unnormalized_rectangle_moves_like_a_normal_one() {
        let backwards = GlobalRect::new(700, 500, 500, 400);
        assert_eq!(backwards.nudged(-1, 0, OUT), GlobalRect::new(499, 400, 699, 500));
        assert_eq!(backwards.nudged(0, 1, OUT), GlobalRect::new(500, 401, 700, 501));
    }

    /// A desktop whose origin is not `(0,0)` (a second monitor left of the primary) walls at
    /// its own borders, not at zero.
    #[test]
    fn the_bounds_origin_is_honoured() {
        let bounds = (-1920, -200, 1920, 1080);
        let rect = GlobalRect::new(-1920, -200, -1720, -100);
        assert_eq!(rect.nudged(-1, -1, bounds), rect, "flush at the far origin");
        assert_eq!(rect.nudged(1, 1, bounds), GlobalRect::new(-1919, -199, -1719, -99));
    }

    /// A zero step is the identity, wherever the region is. Cheap, and it pins that a refused
    /// axis and an unrequested one look the same to the caller.
    #[test]
    fn a_zero_step_changes_nothing() {
        assert_eq!(r().nudged(0, 0, OUT), r());
        let cornered = GlobalRect::new(0, 0, 200, 100);
        assert_eq!(cornered.nudged(0, 0, OUT), cornered);
    }
}
