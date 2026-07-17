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
