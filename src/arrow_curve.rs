//! The arrow's CURVE geometry (DRAGON-470): the one pure module the live annotation canvas
//! (`crate::widgets::annotation_canvas`) and the full-resolution bake
//! (`crate::app::preview::annotate`) both derive a curved arrow from — the same
//! shared-by-both-renderers role [`crate::pen_stroke`] plays for the pencil and
//! [`crate::badge`] for the step marker, and for the same reason: a curve that draws one way on
//! screen and another way in the exported PNG is a bug the user only finds after they have
//! shared the file.
//!
//! # The model: a THIRD handle that sits ON the curve
//! An arrow has always been two points, `a` (tail) and `b` (head), each with a drag node. It now
//! has a third node at the middle of the shaft. Dragging it bends the arrow; it is not a
//! free-floating Bezier control that hovers off the ink (the classic vector-editor affordance
//! that reads as "why is my handle over there"), it is a point that stays UNDER the pointer,
//! because the curve is defined to pass through it:
//!
//! * the drawn shaft is the quadratic Bezier `a → c → b`;
//! * the handle `p` is the curve's own midpoint, `B(0.5) = (a + 2c + b) / 4`;
//! * so the control point is `c = 2p − (a + b) / 2` ([`control`]), the unique parabola through
//!   the three points at uniform parameterization.
//!
//! A QUADRATIC (one control) and not a cubic (two) because the ticket asks for ONE new handle: a
//! cubic would need two, or an invented rule for deriving the second, and a parabola through
//! three points is the whole of what "bend the arrow around" means. It also degenerates
//! perfectly: with `p` at the chord midpoint, `c` is the chord midpoint too, all three control
//! points are collinear and evenly spaced, and the Bezier IS the straight segment. That is why
//! the default arrow can stay byte-identical rather than merely close — see the parity contract
//! below.
//!
//! # Storage: the bend is RELATIVE to the chord
//! The model stores a [`Bend`], the handle's offset from the chord midpoint expressed in the
//! chord's OWN frame: `along` × the chord vector plus `across` × its perpendicular, both
//! dimensionless fractions of the chord length. `Bend::STRAIGHT` (both zero) is the arrow we
//! have always drawn.
//!
//! WHY relative and not an absolute stored point:
//!
//! * **The whole arrow moves as one.** The bend is invariant under every similarity of the two
//!   endpoints, so a translate (`translated_kind`) and a group scale (`group_scaled_kind`) carry
//!   the curve rigidly with no extra mapping code to get wrong. Those gestures move the ARROW,
//!   and everything about it travels together.
//! * **It cannot desynchronize from the endpoints.** An absolute control point is a third piece
//!   of geometry every clamp, resize and duplication path would have to remember to map; the
//!   bend is two numbers that mean the same thing in source pixels, in screen pixels and at bake
//!   scale.
//!
//! The trade is that a bent arrow squashed by a NON-uniform gesture would not shear its arc —
//! but no arrow gesture is non-uniform (endpoints move freely, group scale is uniform), so the
//! case does not arise.
//!
//! # Dragging ONE node moves ONE node
//! The rule the repo owner set after using it: "whenever we move 1 of the 3 nodes along the
//! arrow, the other nodes should not move". So an ENDPOINT drag re-derives the bend against the
//! new chord through the handle's UNCHANGED position ([`bend_pinning_handle`]), and the bend
//! handle's own drag has never touched the endpoints. The three nodes are independent.
//!
//! This overrides the convention the first cut shipped, which kept the stored bend across an
//! endpoint drag so the arc scaled and rotated with the chord (a similarity of the curve you
//! drew). That is what curved connectors do in a drawing tool, and CleanShot X's curved arrows
//! are a curvature control rather than a placed point, so it had a defensible pedigree; ShareX
//! and the Windows 11 Snipping Tool do not curve arrows at all, so there was no third opinion.
//! It still lost, and correctly: on a screenshot annotation you place the bend to dodge something
//! in the picture, and a handle that slides off that spot the moment you nudge an endpoint is
//! fighting you. The tie-breaker is the user, not the convention.
//!
//! Two honest exceptions, both physical rather than stylistic:
//!
//! * a STRAIGHT arrow stays straight through an endpoint drag. Its middle node is a derived
//!   midpoint, not a point anybody placed, so pinning it would invent a bend nobody asked for;
//! * if holding the handle would push the drawn curve off the picture, [`fit_bend`] reduces the
//!   bow until it fits, which moves the handle. Staying on the canvas wins over staying put.
//!
//! # The parity contract (live == bake)
//! Both renderers draw the SAME parabola through their own native quadratic primitive: iced's
//! `Path::quadratic_curve_to` on the canvas, tiny-skia's `PathBuilder::quad_to` in the bake.
//! Each flattens to its own sub-pixel tolerance, exactly as they already do for the shared
//! rounded-box corner. Everything either of them needs beyond the raw path comes from HERE and
//! nowhere else:
//!
//! * [`control`] — the control point, derived in whatever space the caller is drawing in;
//! * [`head_dir`] / [`tail_dir`] — the unit TANGENT at each end, so the arrowhead barbs splay
//!   around the direction the curve actually arrives from, not around the chord;
//! * [`arc_len`] — the shaft length the arrowhead is sized against, so a bowed arrow does not
//!   get the tiny head its (short) chord would imply;
//! * [`flatten`] — the polyline hit-testing and rubber-band selection measure against, so a
//!   curved arrow is grabbable along its ink instead of along a chord that may be nowhere near
//!   it;
//! * [`bbox`] / [`spanned_bounds`] — the exact bounding box of the parabola (endpoints plus the
//!   per-axis vertex), which every clamp, chrome rect and band test reasons about.
//!
//! [`control`] commutes with any similarity (`control(M·a, M·b, bend) == M·control(a, b, bend)`,
//! pinned by a unit test), which is what lets the canvas derive it from mapped SCREEN points and
//! the bake from `scale`-multiplied SOURCE points and still get the same drawing at two
//! resolutions.
//!
//! Every function here early-returns the straight answer for [`Bend::STRAIGHT`] — the segment
//! endpoints, the chord direction, the chord length, the endpoint bbox — so an arrow nobody has
//! bent takes numerically the same path it took before this module existed.

/// How far the bend handle may be dragged from the chord midpoint, in CHORD LENGTHS. Two chord
/// lengths is already a fold-over loop far bigger than the arrow itself; past that a drag is a
/// slip, not an intent, and unbounded values would let one flick produce geometry no later
/// gesture can recover from (the bbox, and with it every clamp, grows with the bend).
pub const BEND_LIMIT: f32 = 2.0;

/// Below this — as a fraction of the chord length, so it is resolution-independent — a bend
/// reads as perfectly STRAIGHT. On a 400px arrow this is 0.04px of bow, far under one device
/// pixel at any zoom, and it is what guarantees a freshly drawn arrow (and one whose handle is
/// dragged back to the middle) renders through the untouched straight path rather than through a
/// parabola that merely rounds to it.
const STRAIGHT_EPS: f32 = 1e-4;

/// The BEND of an arrow: where its third handle sits, relative to the chord.
///
/// `along` and `across` are fractions of the chord length, in the chord's own frame: the handle
/// is `midpoint + along · (b − a) + across · perp(b − a)`, with `perp((x, y)) = (−y, x)`. Both
/// zero is the straight arrow. Because the frame rides the endpoints, the pair is invariant
/// under translation, rotation and uniform scale of the arrow — see the module doc.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Bend {
    /// Offset along the chord, in chord lengths. Slides the curve's midpoint toward one end
    /// (which skews the parabola); zero keeps it centred.
    pub along: f32,
    /// Offset perpendicular to the chord, in chord lengths. THIS is the bow: positive is toward
    /// the chord's left normal `(−dy, dx)`.
    pub across: f32,
}

impl Bend {
    /// The arrow as it has always been drawn: no bend at all.
    pub const STRAIGHT: Self = Self { along: 0.0, across: 0.0 };

    /// Whether this bend renders as a straight arrow — the DEFAULT path every renderer here
    /// early-returns to. A non-finite component reads as straight too (defensive: a NaN must
    /// degrade to the shape we know how to draw, never propagate into a path builder). Pure —
    /// unit-tested.
    pub fn is_straight(self) -> bool {
        !(self.along.is_finite() && self.across.is_finite())
            || (self.along.abs() <= STRAIGHT_EPS && self.across.abs() <= STRAIGHT_EPS)
    }
}

/// The midpoint of the chord `a`–`b`.
fn mid(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5)
}

/// `v` normalized, or `None` when it is too short (or non-finite) to carry a direction.
fn unit(v: (f32, f32)) -> Option<(f32, f32)> {
    let len = (v.0 * v.0 + v.1 * v.1).sqrt();
    if len.is_finite() && len > 1e-6 {
        Some((v.0 / len, v.1 / len))
    } else {
        None
    }
}

/// The quadratic Bezier CONTROL point of the arrow `a` → `b` bent by `bend`, in the SAME space as
/// `a` and `b` (source px for the bake, screen px for the canvas — it commutes with any
/// similarity, which is the live/bake parity guarantee).
///
/// `c = 2p − (a + b)/2` where `p` is the handle, i.e. the parabola through `a`, `p`, `b`. A
/// straight bend returns the chord MIDPOINT, where the Bezier degenerates to the segment exactly.
/// Pure — unit-tested.
pub fn control(a: (f32, f32), b: (f32, f32), bend: Bend) -> (f32, f32) {
    let m = mid(a, b);
    if bend.is_straight() {
        return m;
    }
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    // c = m + 2·(along·d + across·perp(d)) — twice the handle's own offset, since the handle is
    // the curve's midpoint and a quadratic's midpoint sits halfway between the chord midpoint
    // and its control point.
    (
        m.0 + 2.0 * (bend.along * dx - bend.across * dy),
        m.1 + 2.0 * (bend.along * dy + bend.across * dx),
    )
}

/// Where the arrow's third HANDLE sits: the curve's own midpoint `B(0.5)`, in the same space as
/// `a` and `b`. The straight arrow puts it exactly on the chord midpoint. Pure — unit-tested.
pub fn handle(a: (f32, f32), b: (f32, f32), bend: Bend) -> (f32, f32) {
    let m = mid(a, b);
    if bend.is_straight() {
        return m;
    }
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    (
        m.0 + bend.along * dx - bend.across * dy,
        m.1 + bend.along * dy + bend.across * dx,
    )
}

/// The inverse of [`handle`]: the [`Bend`] that puts the third handle at `h` — what a drag of that
/// handle resolves to.
///
/// Both components are clamped to ±[`BEND_LIMIT`]. A degenerate chord (the two endpoints on top of
/// each other) has no frame to express a bend in, so it stays STRAIGHT rather than inventing one.
/// Pure — unit-tested (round-trips with [`handle`]).
pub fn bend_from_handle(a: (f32, f32), b: (f32, f32), h: (f32, f32)) -> Bend {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    // Spelled with `is_finite` first so a NaN endpoint takes this branch too, rather than falling
    // through into a division that would poison the stored bend.
    if !len2.is_finite() || len2 <= 1e-12 {
        return Bend::STRAIGHT;
    }
    let m = mid(a, b);
    let (wx, wy) = (h.0 - m.0, h.1 - m.1);
    let hold = |v: f32| if v.is_finite() { v.clamp(-BEND_LIMIT, BEND_LIMIT) } else { 0.0 };
    Bend {
        along: hold((wx * dx + wy * dy) / len2),
        across: hold((dx * wy - dy * wx) / len2),
    }
}

/// The bend an ENDPOINT drag leaves behind: the one that keeps the handle exactly where it is
/// while the chord moves under it (the repo owner's node-independence rule, see the module doc).
///
/// `old_a`/`old_b`/`bend` describe the arrow BEFORE the drag — in the app that is the gesture's
/// own pre-drag snapshot, so the pinned point is fixed for the whole drag and the curve
/// re-derives against it on every motion event rather than drifting from the previous frame.
///
/// A STRAIGHT arrow returns straight, and that early return is load-bearing: its handle is a
/// DERIVED midpoint, so re-deriving it against a chord that has moved would read the old midpoint
/// as a placed point and bend an arrow nobody bent. Everything else falls out of
/// [`bend_from_handle`], including its guards: a chord dragged to nothing has no frame to hold a
/// handle in and degrades to straight (no NaN, no panic), and because the pin comes from the
/// UNCHANGED snapshot, dragging back out of that degenerate position restores the curve rather
/// than latching the collapse. Pure — unit-tested.
pub fn bend_pinning_handle(
    old_a: (f32, f32),
    old_b: (f32, f32),
    bend: Bend,
    new_a: (f32, f32),
    new_b: (f32, f32),
) -> Bend {
    if bend.is_straight() {
        return Bend::STRAIGHT;
    }
    bend_from_handle(new_a, new_b, handle(old_a, old_b, bend))
}

/// The point at parameter `t` on the quadratic `a` → `c` → `b`. Pure — unit-tested.
pub fn point_at(a: (f32, f32), c: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    let s = 1.0 - t;
    let (w0, w1, w2) = (s * s, 2.0 * s * t, t * t);
    (
        w0 * a.0 + w1 * c.0 + w2 * b.0,
        w0 * a.1 + w1 * c.1 + w2 * b.1,
    )
}

/// The unit TANGENT where the curve ARRIVES at the head `b`, pointing forward — the direction the
/// arrowhead's barbs splay around. A quadratic's end tangent is `b − c`; when that is degenerate
/// (the control sits on the head) it falls back to the chord, and a fully degenerate arrow to
/// `(1, 0)` so no caller can divide by zero. Pure — unit-tested.
pub fn head_dir(a: (f32, f32), c: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    unit((b.0 - c.0, b.1 - c.1))
        .or_else(|| unit((b.0 - a.0, b.1 - a.1)))
        .unwrap_or((1.0, 0.0))
}

/// The unit tangent where the curve LEAVES the tail `a`, pointing forward (toward the head) — the
/// axis the tail's drag node is pushed out along. Same fallbacks as [`head_dir`]. Pure —
/// unit-tested.
pub fn tail_dir(a: (f32, f32), c: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    unit((c.0 - a.0, c.1 - a.1))
        .or_else(|| unit((b.0 - a.0, b.1 - a.1)))
        .unwrap_or((1.0, 0.0))
}

/// How many even segments [`arc_len`] measures the curve with. A parabola's length converges
/// quickly; 16 holds the error under a tenth of a percent for any bend this editor can produce,
/// and it only ever sizes an arrowhead.
const ARC_STEPS: usize = 16;

/// The LENGTH of the drawn shaft `a` → `c` → `b` — what the arrowhead is sized against, so a
/// bowed arrow gets a head proportional to the ink rather than to its (much shorter) chord.
/// Returns the chord exactly when the control is the chord midpoint. Pure — unit-tested.
pub fn arc_len(a: (f32, f32), c: (f32, f32), b: (f32, f32)) -> f32 {
    // The straight case is the chord, computed the way every caller already computes it.
    if is_degenerate_control(a, c, b) {
        return (b.0 - a.0).hypot(b.1 - a.1);
    }
    let mut total = 0.0;
    let mut prev = a;
    for i in 1..=ARC_STEPS {
        let p = point_at(a, c, b, i as f32 / ARC_STEPS as f32);
        total += (p.0 - prev.0).hypot(p.1 - prev.1);
        prev = p;
    }
    total
}

/// Whether the control point `c` leaves the quadratic indistinguishable from the segment `a`–`b`:
/// it is the chord midpoint (to within the same relative slack [`Bend::is_straight`] uses). Lets
/// the query functions take the straight path even when they are handed a control point directly.
fn is_degenerate_control(a: (f32, f32), c: (f32, f32), b: (f32, f32)) -> bool {
    let m = mid(a, b);
    let (ox, oy) = (c.0 - m.0, c.1 - m.1);
    if !(ox.is_finite() && oy.is_finite()) {
        return true;
    }
    let chord = (b.0 - a.0).hypot(b.1 - a.1);
    ox.hypot(oy) <= STRAIGHT_EPS * chord.max(1.0)
}

/// The most segments [`flatten`] may emit — the point at which it stops honouring the requested
/// tolerance and starts honouring this instead (the true bound is spelled out on [`flatten`]).
/// A parabola needs very few segments for sub-pixel accuracy, so the cap is only a ceiling on
/// cost; it sits high enough that no curve this editor can produce reaches it (256 segments hold
/// a quarter-pixel on a `|a − 2c + b|` of 65 000 px, an order of magnitude past a bent arrow
/// spanning an 8K canvas at maximum zoom), and the vector is per-call, so the headroom is cheap.
const FLATTEN_MAX: usize = 256;

/// The curve `a` → `c` → `b` as a POLYLINE whose deviation from the true curve is at most `tol`
/// (in the caller's own units — screen px for the canvas hit-test, source px for the band test).
/// Always includes both endpoints; a straight curve returns exactly `[a, b]`, so the existing
/// segment maths runs on exactly the two points it always did.
///
/// The count comes from the parabola's constant second derivative `2·(a − 2c + b)`: a chord over
/// a parameter span `h` deviates by at most `|B''|·h²/8`, so `n = ½·√(|a − 2c + b| / tol)`
/// segments hold the error under `tol`.
///
/// The one case where `tol` is NOT honoured is a curve needing more than [`FLATTEN_MAX`]
/// segments: the deviation is then `|a − 2c + b| / (4·FLATTEN_MAX²)` instead, which is the honest
/// bound to quote at a call site. Pure — unit-tested (the sampled curve really is within `tol`,
/// and the capped case within its own bound).
pub fn flatten(a: (f32, f32), c: (f32, f32), b: (f32, f32), tol: f32) -> Vec<(f32, f32)> {
    if is_degenerate_control(a, c, b) {
        return vec![a, b];
    }
    let (dx, dy) = (a.0 - 2.0 * c.0 + b.0, a.1 - 2.0 * c.1 + b.1);
    let d = dx.hypot(dy);
    let tol = if tol.is_finite() && tol > 1e-4 { tol } else { 1e-4 };
    let want = 0.5 * (d / tol).sqrt();
    let n = if want.is_finite() { (want.ceil() as usize).clamp(2, FLATTEN_MAX) } else { FLATTEN_MAX };
    let mut out = Vec::with_capacity(n + 1);
    out.push(a);
    for i in 1..n {
        out.push(point_at(a, c, b, i as f32 / n as f32));
    }
    out.push(b);
    out
}

/// The EXACT bounding box `(x0, y0, x1, y1)` of the curve `a` → `c` → `b`: the endpoints plus, per
/// axis, the parabola's vertex when it falls strictly inside the span. A straight curve gives the
/// endpoints' own min/max. Pure — unit-tested.
pub fn bbox(a: (f32, f32), c: (f32, f32), b: (f32, f32)) -> (f32, f32, f32, f32) {
    let axis = |p0: f32, p1: f32, p2: f32| {
        let (mut lo, mut hi) = (p0.min(p2), p0.max(p2));
        let denom = p0 - 2.0 * p1 + p2;
        // Relative guard: a chord-midpoint control leaves `denom` at (or within a rounding step
        // of) zero, where the "vertex" is meaningless and would divide into infinity.
        let scale = p0.abs().max(p1.abs()).max(p2.abs()).max(1.0);
        if denom.abs() > 1e-6 * scale {
            let t = (p0 - p1) / denom;
            if t > 0.0 && t < 1.0 {
                let s = 1.0 - t;
                let v = s * s * p0 + 2.0 * s * t * p1 + t * t * p2;
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        (lo, hi)
    };
    let (x0, x1) = axis(a.0, c.0, b.0);
    let (y0, y1) = axis(a.1, c.1, b.1);
    (x0, y0, x1, y1)
}

/// [`bbox`] for an arrow given as endpoints + [`Bend`] — the outer extent every clamp, selection
/// chrome and band test reasons about. A straight arrow returns its endpoints' min/max, the
/// identical numbers the chord's own rect has always given. Pure — unit-tested.
pub fn spanned_bounds(a: (f32, f32), b: (f32, f32), bend: Bend) -> (f32, f32, f32, f32) {
    if bend.is_straight() {
        return (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
    }
    bbox(a, control(a, b, bend), b)
}

/// `bend` with both components multiplied by `k` — the one-parameter family [`fit_bend`] searches.
fn scaled(bend: Bend, k: f32) -> Bend {
    Bend { along: bend.along * k, across: bend.across * k }
}

/// Whether the whole drawn curve fits inside the box `lo`..`hi`.
fn fits(a: (f32, f32), b: (f32, f32), bend: Bend, lo: (f32, f32), hi: (f32, f32)) -> bool {
    let (x0, y0, x1, y1) = spanned_bounds(a, b, bend);
    x0 >= lo.0 && y0 >= lo.1 && x1 <= hi.0 && y1 <= hi.1
}

/// Bisection steps [`fit_bend`] takes. 24 halvings resolve the surviving fraction to ~6·10⁻⁸ of
/// the bend, far under a pixel of bow on any canvas, and each step costs one [`bbox`].
const FIT_STEPS: usize = 24;

/// The largest fraction of `bend` whose WHOLE drawn curve still fits inside the box `lo`..`hi` —
/// how a bent arrow is held on the picture when a drag moves its ENDPOINTS or its bend handle.
///
/// WHY a fraction of the bend rather than a clamp on some point: the endpoints are what the user
/// is dragging, so they must land where they were put; the bow is the only part left to give.
/// Clamping the bend HANDLE alone is not enough, and the reason is worth stating because it looks
/// like it should be: the handle is the curve's farthest excursion PERPENDICULAR to the chord,
/// but not along it. A handle dragged toward the head sends the shaft overshooting PAST the head
/// (by up to ⅛ of the chord) while the handle itself sits comfortably inside the frame, and an
/// endpoint drag can scale a chord-relative bow far off-picture without moving the handle
/// through the boundary at all. Both were live bugs; both are what this function exists for.
///
/// Returns `bend` UNCHANGED (same bits) whenever it already fits, so an ordinary drag preserves
/// the stored numbers exactly. Otherwise the curve is flattened just enough, never past straight.
///
/// The search is a bisection, which is exact here because the family is MONOTONE: every point of
/// the curve is `straight(t) + 4·k·t·(1−t)·v` for a fixed offset `v`, so scaling `k` moves each
/// coordinate linearly and the bounding box only ever grows with `k`.
///
/// The box is first WIDENED to include the chord's own extent, so `k = 0` always fits and the
/// answer is always "the largest bow that adds nothing to whatever the endpoints already do". The
/// bend is only ever asked to pay for its OWN overhang: an arrow whose endpoint sits a rounding
/// step outside the margin (or outside it by policy) must not have the user's curve flattened for
/// it, which is what a strict test would do the moment the bisection found nothing that fits.
/// Pure — unit-tested.
pub fn fit_bend(
    a: (f32, f32),
    b: (f32, f32),
    bend: Bend,
    lo: (f32, f32),
    hi: (f32, f32),
) -> Bend {
    // Nothing to give, or nothing to fix.
    if bend.is_straight() || fits(a, b, bend, lo, hi) {
        return bend;
    }
    let (sx0, sy0, sx1, sy1) = spanned_bounds(a, b, Bend::STRAIGHT);
    let lo = (lo.0.min(sx0), lo.1.min(sy0));
    let hi = (hi.0.max(sx1), hi.1.max(sy1));
    let (mut good, mut bad) = (0.0_f32, 1.0_f32);
    for _ in 0..FIT_STEPS {
        let k = 0.5 * (good + bad);
        if fits(a, b, scaled(bend, k), lo, hi) {
            good = k;
        } else {
            bad = k;
        }
    }
    scaled(bend, good)
}

#[cfg(test)]
mod bend_tests {
    use super::*;

    #[test]
    fn the_default_bend_is_straight() {
        assert!(Bend::default().is_straight());
        assert!(Bend::STRAIGHT.is_straight());
    }

    #[test]
    fn a_sub_pixel_bend_reads_as_straight_and_a_real_one_does_not() {
        // 1e-5 of a chord: 0.004 px on a 400px arrow.
        assert!(Bend { along: 0.0, across: 1e-5 }.is_straight());
        assert!(Bend { along: 1e-5, across: 0.0 }.is_straight());
        assert!(!Bend { along: 0.0, across: 0.02 }.is_straight());
        // A pure ALONG offset skews the parabola (it can overshoot past the head), so it is NOT
        // straight either — both components have to be quiet.
        assert!(!Bend { along: 0.3, across: 0.0 }.is_straight());
    }

    #[test]
    fn a_non_finite_bend_degrades_to_straight() {
        assert!(Bend { along: f32::NAN, across: 0.0 }.is_straight());
        assert!(Bend { along: 0.0, across: f32::INFINITY }.is_straight());
    }

    #[test]
    fn the_handle_of_a_straight_arrow_is_the_chord_midpoint() {
        let h = handle((10.0, 20.0), (110.0, 60.0), Bend::STRAIGHT);
        assert!((h.0 - 60.0).abs() < 1e-4 && (h.1 - 40.0).abs() < 1e-4);
    }

    #[test]
    fn handle_and_bend_from_handle_round_trip() {
        let (a, b) = ((10.0, 20.0), (110.0, 60.0));
        for want in [(60.0, 10.0), (30.0, 90.0), (61.0, 41.0), (-20.0, 40.0)] {
            let bend = bend_from_handle(a, b, want);
            let got = handle(a, b, bend);
            assert!(
                (got.0 - want.0).abs() < 1e-3 && (got.1 - want.1).abs() < 1e-3,
                "{want:?} → {bend:?} → {got:?}"
            );
        }
    }

    #[test]
    fn a_handle_on_the_midpoint_is_exactly_straight() {
        let (a, b) = ((10.0, 20.0), (110.0, 60.0));
        let bend = bend_from_handle(a, b, (60.0, 40.0));
        assert!(bend.is_straight());
    }

    #[test]
    fn a_wild_drag_is_held_to_the_bend_limit() {
        let (a, b) = ((0.0, 0.0), (100.0, 0.0));
        let bend = bend_from_handle(a, b, (50.0, -100_000.0));
        assert!(bend.across.abs() <= BEND_LIMIT + 1e-6, "{bend:?}");
        let bend = bend_from_handle(a, b, (100_000.0, 0.0));
        assert!(bend.along.abs() <= BEND_LIMIT + 1e-6, "{bend:?}");
    }

    #[test]
    fn a_degenerate_chord_has_no_bend_frame() {
        assert!(bend_from_handle((5.0, 5.0), (5.0, 5.0), (40.0, 90.0)).is_straight());
    }

    #[test]
    fn the_chord_frame_is_invariant_under_a_similarity() {
        // The property the RELATIVE storage buys: translate / rotate / uniform-scale the two
        // endpoints and the same numbers describe the same curve, which is why `translated_kind`
        // and `group_scaled_kind` carry the bend verbatim. (It is NOT what an endpoint DRAG does
        // — that pins the handle instead, see `pin_tests`.)
        let (a, b) = ((0.0, 0.0), (100.0, 0.0));
        let bend = Bend { along: 0.1, across: -0.25 };
        let p = handle(a, b, bend);
        let map = |q: (f32, f32)| (-3.0 * q.1 + 12.0, 3.0 * q.0 - 5.0);
        let again = bend_from_handle(map(a), map(b), map(p));
        assert!((again.along - bend.along).abs() < 1e-4, "{again:?}");
        assert!((again.across - bend.across).abs() < 1e-4, "{again:?}");
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    /// The bend handle after an endpoint drag, given the arrow before it.
    fn moved(old_a: (f32, f32), old_b: (f32, f32), bend: Bend, new_a: (f32, f32), new_b: (f32, f32)) -> (f32, f32) {
        handle(new_a, new_b, bend_pinning_handle(old_a, old_b, bend, new_a, new_b))
    }

    #[test]
    fn the_handle_holds_its_place_while_an_endpoint_moves() {
        let (a, b) = ((100.0, 200.0), (300.0, 200.0));
        let bend = Bend { along: 0.1, across: -0.3 };
        let pinned = handle(a, b, bend);
        // Lengthen, shorten, rotate, and move the OTHER end: the handle does not budge.
        for (na, nb) in [
            ((100.0, 200.0), (900.0, 200.0)),
            ((100.0, 200.0), (160.0, 200.0)),
            ((100.0, 200.0), (300.0, 600.0)),
            ((-50.0, 40.0), (300.0, 200.0)),
        ] {
            let got = moved(a, b, bend, na, nb);
            assert!(
                (got.0 - pinned.0).abs() < 1e-2 && (got.1 - pinned.1).abs() < 1e-2,
                "{na:?}→{nb:?}: handle moved {pinned:?} → {got:?}"
            );
        }
    }

    #[test]
    fn a_straight_arrow_stays_straight_through_an_endpoint_drag() {
        // Its middle node is a DERIVED midpoint, so pinning the old one would bend an arrow
        // nobody bent. The handle is expected to move here, and only here.
        let (a, b) = ((0.0, 0.0), (100.0, 0.0));
        let out = bend_pinning_handle(a, b, Bend::STRAIGHT, a, (400.0, 250.0));
        assert!(out.is_straight(), "{out:?}");
        assert_eq!(out, Bend::STRAIGHT);
    }

    #[test]
    fn a_chord_dragged_to_nothing_degrades_and_recovers() {
        let (a, b) = ((100.0, 200.0), (300.0, 200.0));
        let bend = Bend { along: 0.0, across: -0.3 };
        // Endpoint B dragged exactly onto A: no chord, so no frame to hold a handle in.
        let collapsed = bend_pinning_handle(a, b, bend, a, a);
        assert!(collapsed.is_straight(), "{collapsed:?}");
        assert!(collapsed.along.is_finite() && collapsed.across.is_finite(), "no NaN");
        // Dragged back out in the SAME gesture (the pin comes from the unchanged snapshot), the
        // curve returns exactly — the collapse never latched.
        let out = bend_pinning_handle(a, b, bend, a, b);
        assert_eq!(out, bend, "{out:?}");
        let back = moved(a, b, bend, a, (280.0, 200.0));
        let pinned = handle(a, b, bend);
        assert!((back.0 - pinned.0).abs() < 1e-2 && (back.1 - pinned.1).abs() < 1e-2, "{back:?}");
    }

    #[test]
    fn a_pin_far_outside_the_new_chord_is_still_bounded() {
        // Shrink the chord to a stub while the handle stays put: the fractions blow up, and the
        // BEND_LIMIT clamp is what keeps the geometry finite and sane.
        let (a, b) = ((100.0, 200.0), (300.0, 200.0));
        let bend = Bend { along: 0.0, across: -0.5 };
        let out = bend_pinning_handle(a, b, bend, a, (104.0, 200.0));
        assert!(out.along.abs() <= BEND_LIMIT + 1e-6, "{out:?}");
        assert!(out.across.abs() <= BEND_LIMIT + 1e-6, "{out:?}");
        let (x0, y0, x1, y1) = spanned_bounds(a, (104.0, 200.0), out);
        assert!(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite(), "finite box");
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;

    #[test]
    fn a_straight_bend_puts_the_control_on_the_chord_midpoint() {
        let c = control((10.0, 20.0), (110.0, 60.0), Bend::STRAIGHT);
        assert!((c.0 - 60.0).abs() < 1e-4 && (c.1 - 40.0).abs() < 1e-4);
    }

    #[test]
    fn a_straight_curve_samples_exactly_on_the_segment() {
        let (a, b) = ((10.0, 20.0), (110.0, 60.0));
        let c = control(a, b, Bend::STRAIGHT);
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let p = point_at(a, c, b, t);
            let want = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            assert!((p.0 - want.0).abs() < 1e-3 && (p.1 - want.1).abs() < 1e-3, "t={t}");
        }
    }

    #[test]
    fn the_curve_passes_through_the_handle() {
        let (a, b) = ((0.0, 0.0), (200.0, 40.0));
        for bend in [
            Bend { along: 0.0, across: 0.3 },
            Bend { along: -0.2, across: -0.15 },
            Bend { along: 0.4, across: 0.0 },
        ] {
            let c = control(a, b, bend);
            let mid_pt = point_at(a, c, b, 0.5);
            let h = handle(a, b, bend);
            assert!(
                (mid_pt.0 - h.0).abs() < 1e-3 && (mid_pt.1 - h.1).abs() < 1e-3,
                "{bend:?}: {mid_pt:?} vs {h:?}"
            );
        }
    }

    #[test]
    fn the_control_point_commutes_with_a_similarity() {
        // THE live-vs-bake parity guarantee: the canvas derives the control from SCREEN points and
        // the bake from source points multiplied by the raster scale. Those differ by a similarity
        // (uniform scale + translation, and rotation for good measure), so the two must agree.
        let (a, b) = ((12.0, -7.0), (140.0, 96.0));
        let bend = Bend { along: 0.13, across: -0.42 };
        let map = |q: (f32, f32)| (2.5 * q.0 - 1.5 * q.1 + 30.0, 1.5 * q.0 + 2.5 * q.1 - 12.0);
        let direct = control(map(a), map(b), bend);
        let mapped = map(control(a, b, bend));
        assert!(
            (direct.0 - mapped.0).abs() < 1e-2 && (direct.1 - mapped.1).abs() < 1e-2,
            "{direct:?} vs {mapped:?}"
        );
    }
}

#[cfg(test)]
mod tangent_tests {
    use super::*;

    #[test]
    fn a_straight_arrow_points_along_its_chord_at_both_ends() {
        let (a, b) = ((0.0, 0.0), (100.0, 0.0));
        let c = control(a, b, Bend::STRAIGHT);
        assert!((head_dir(a, c, b).0 - 1.0).abs() < 1e-5);
        assert!(head_dir(a, c, b).1.abs() < 1e-5);
        assert!((tail_dir(a, c, b).0 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn the_head_follows_the_curve_not_the_chord() {
        let (a, b) = ((0.0, 0.0), (100.0, 0.0));
        let bend = Bend { along: 0.0, across: -0.3 };
        let c = control(a, b, bend);
        let h = head_dir(a, c, b);
        // A bow above the chord arrives at the head heading DOWN-right, never straight right.
        assert!(h.0 > 0.0 && h.1 > 0.05, "{h:?}");
        // Unit length.
        assert!((h.0.hypot(h.1) - 1.0).abs() < 1e-4);
        // Symmetric bend: the tail leaves as steeply as the head arrives, mirrored.
        let t = tail_dir(a, c, b);
        assert!((t.0 - h.0).abs() < 1e-4 && (t.1 + h.1).abs() < 1e-4, "{t:?} vs {h:?}");
    }

    #[test]
    fn a_fully_degenerate_arrow_still_yields_a_direction() {
        let p = (7.0, 7.0);
        let d = head_dir(p, p, p);
        assert!((d.0.hypot(d.1) - 1.0).abs() < 1e-5);
        let d = tail_dir(p, p, p);
        assert!((d.0.hypot(d.1) - 1.0).abs() < 1e-5);
    }
}

#[cfg(test)]
mod length_tests {
    use super::*;

    #[test]
    fn a_straight_shaft_measures_its_chord() {
        let (a, b) = ((10.0, 10.0), (110.0, 10.0));
        assert!((arc_len(a, control(a, b, Bend::STRAIGHT), b) - 100.0).abs() < 1e-3);
    }

    #[test]
    fn a_bowed_shaft_is_longer_than_its_chord() {
        let (a, b) = ((0.0, 0.0), (100.0, 0.0));
        let bend = Bend { along: 0.0, across: 0.5 };
        let len = arc_len(a, control(a, b, bend), b);
        assert!(len > 100.0, "{len}");
        // Sanity: a 50px-deep bow over a 100px chord is well under twice the chord.
        assert!(len < 200.0, "{len}");
    }
}

#[cfg(test)]
mod flatten_tests {
    use super::*;

    #[test]
    fn a_straight_curve_flattens_to_its_two_endpoints() {
        let (a, b) = ((10.0, 20.0), (110.0, 60.0));
        let pts = flatten(a, control(a, b, Bend::STRAIGHT), b, 0.25);
        assert_eq!(pts, vec![a, b]);
    }

    #[test]
    fn the_polyline_stays_within_the_tolerance_of_the_curve() {
        let (a, b) = ((0.0, 0.0), (400.0, 120.0));
        let bend = Bend { along: 0.1, across: 0.45 };
        let c = control(a, b, bend);
        let tol = 0.25;
        let pts = flatten(a, c, b, tol);
        assert!(pts.len() >= 3 && pts.len() <= super::FLATTEN_MAX + 1, "{}", pts.len());
        // Every true-curve sample must sit within `tol` (plus a hair of float slack) of the
        // polyline the hit-test measures against.
        for i in 0..=200 {
            let t = i as f32 / 200.0;
            let p = point_at(a, c, b, t);
            let mut best = f32::INFINITY;
            for w in pts.windows(2) {
                best = best.min(seg_dist(p, w[0], w[1]));
            }
            assert!(best <= tol + 1e-3, "t={t}: {best}");
        }
    }

    #[test]
    fn a_capped_curve_still_meets_the_bound_its_doc_quotes() {
        // Past FLATTEN_MAX the promise is `|a − 2c + b| / (4·n²)`, not `tol`. Force the cap with
        // an absurd bend and hold the result to that stated bound (and to the cap itself).
        let (a, b) = ((0.0, 0.0), (400_000.0, 0.0));
        let c = control(a, b, Bend { along: 0.0, across: 1.0 });
        let pts = flatten(a, c, b, 0.25);
        assert_eq!(pts.len(), super::FLATTEN_MAX + 1, "the cap really is in play");
        let d = (a.0 - 2.0 * c.0 + b.0).hypot(a.1 - 2.0 * c.1 + b.1);
        let bound = d / (4.0 * (super::FLATTEN_MAX * super::FLATTEN_MAX) as f32);
        for i in 0..=400 {
            let t = i as f32 / 400.0;
            let p = point_at(a, c, b, t);
            let mut best = f32::INFINITY;
            for w in pts.windows(2) {
                best = best.min(seg_dist(p, w[0], w[1]));
            }
            assert!(best <= bound * 1.05 + 1e-2, "t={t}: {best} > {bound}");
        }
    }

    #[test]
    fn a_gentler_curve_needs_fewer_segments() {
        let (a, b) = ((0.0, 0.0), (400.0, 0.0));
        let gentle = flatten(a, control(a, b, Bend { along: 0.0, across: 0.02 }), b, 0.25);
        let hard = flatten(a, control(a, b, Bend { along: 0.0, across: 0.8 }), b, 0.25);
        assert!(gentle.len() < hard.len(), "{} vs {}", gentle.len(), hard.len());
    }

    /// Distance from `p` to the segment `q`–`r` (test helper).
    fn seg_dist(p: (f32, f32), q: (f32, f32), r: (f32, f32)) -> f32 {
        let (vx, vy) = (r.0 - q.0, r.1 - q.1);
        let len2 = vx * vx + vy * vy;
        if len2 <= f32::EPSILON {
            return (p.0 - q.0).hypot(p.1 - q.1);
        }
        let t = (((p.0 - q.0) * vx + (p.1 - q.1) * vy) / len2).clamp(0.0, 1.0);
        (p.0 - (q.0 + vx * t)).hypot(p.1 - (q.1 + vy * t))
    }
}

#[cfg(test)]
mod bounds_tests {
    use super::*;

    #[test]
    fn a_straight_arrow_bounds_exactly_on_its_endpoints() {
        for (a, b) in [
            ((10.0, 20.0), (110.0, 60.0)),
            ((110.0, 60.0), (10.0, 20.0)),
            ((0.0, 90.0), (90.0, 0.0)),
        ] {
            let got = spanned_bounds(a, b, Bend::STRAIGHT);
            let want = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
            assert_eq!(got, want, "{a:?}→{b:?}");
        }
    }

    #[test]
    fn fitting_leaves_a_bend_that_already_fits_exactly_alone() {
        let (a, b) = ((10.0, 100.0), (110.0, 100.0));
        let bend = Bend { along: 0.05, across: -0.2 };
        let got = fit_bend(a, b, bend, (0.0, 0.0), (200.0, 200.0));
        assert_eq!(got, bend, "an ordinary drag keeps the stored numbers");
        assert_eq!(fit_bend(a, b, Bend::STRAIGHT, (0.0, 0.0), (1.0, 1.0)), Bend::STRAIGHT);
    }

    #[test]
    fn fitting_shrinks_a_bow_that_leaves_the_box() {
        // A 100px chord along the top of a 200×200 box, bowed 60px UP: 50px off-picture.
        let (a, b) = ((50.0, 10.0), (150.0, 10.0));
        let bend = Bend { along: 0.0, across: -0.6 };
        assert!(!fits(a, b, bend, (0.0, 0.0), (200.0, 200.0)), "the premise");
        let got = fit_bend(a, b, bend, (0.0, 0.0), (200.0, 200.0));
        assert!(fits(a, b, got, (0.0, 0.0), (200.0, 200.0)), "and now it does: {got:?}");
        // Flattened, but not straightened: the 10px of headroom is still used.
        assert!(got.across.abs() < bend.across.abs(), "{got:?}");
        assert!(!got.is_straight(), "{got:?}");
        // TIGHT: it keeps as much bow as the box allows (the apex lands on the boundary).
        let (_, y0, _, _) = spanned_bounds(a, b, got);
        assert!(y0.abs() < 0.5, "apex pinned to the edge, got y0={y0}");
    }

    #[test]
    fn fitting_holds_an_along_chord_overshoot_too() {
        // The handle dragged ONTO the head: the shaft overshoots past it by ⅛ of the chord, even
        // though the handle itself is 20px inside the right edge. This is repro (a).
        let (a, b) = ((0.0, 100.0), (180.0, 100.0));
        let bend = Bend { along: 0.5, across: 0.0 };
        let (_, _, x1, _) = spanned_bounds(a, b, bend);
        assert!(x1 > 200.0, "the premise: the shaft runs to {x1}");
        let got = fit_bend(a, b, bend, (0.0, 0.0), (200.0, 200.0));
        assert!(fits(a, b, got, (0.0, 0.0), (200.0, 200.0)), "{got:?}");
    }

    #[test]
    fn a_bowed_arrow_bounds_cover_the_bow() {
        let (a, b) = ((0.0, 0.0), (100.0, 0.0));
        let bend = Bend { along: 0.0, across: -0.4 };
        let (x0, y0, x1, y1) = spanned_bounds(a, b, bend);
        let c = control(a, b, bend);
        // Every sample of the real curve lies inside the reported box.
        for i in 0..=100 {
            let p = point_at(a, c, b, i as f32 / 100.0);
            assert!(p.0 >= x0 - 1e-3 && p.0 <= x1 + 1e-3, "{p:?} in x[{x0},{x1}]");
            assert!(p.1 >= y0 - 1e-3 && p.1 <= y1 + 1e-3, "{p:?} in y[{y0},{y1}]");
        }
        // And the box is TIGHT: the bow's own apex is the handle, 40px above the chord.
        let h = handle(a, b, bend);
        assert!((y0 - h.1).abs() < 1e-3, "{y0} vs {}", h.1);
        // The bbox never balloons past the control hull.
        assert!(x0 >= -1e-3 && x1 <= 100.0 + 1e-3, "x[{x0},{x1}]");
    }
}
