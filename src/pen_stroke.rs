//! Freehand pen-stroke BEAUTIFICATION (DRAGON-342): the one pure module that turns a raw
//! pointer trail into the inky, pressure-looking ribbon both the live canvas and the
//! full-resolution bake draw.
//!
//! The pencil (`AnnotKind::Pen`, DRAGON-338) used to be a fixed-width polyline stroked with
//! round caps/joins — honest, but it looked like a mouse trail: every hand tremor showed, both
//! ends stopped dead at full weight, and a loop read exactly like a straight line. Three pure
//! stages fix that, and BOTH renderers run the SAME ones so display and bake can never drift:
//!
//! 1. **Smoothing** ([`smooth_path`]) — binomial low-pass (endpoints pinned) → CAUSAL
//!    opening-window decimation ([`decimate`]) → CENTRIPETAL Catmull-Rom resample. The low-pass
//!    kills tremor, the decimation throws away the points the curve doesn't need (jitter-scale
//!    detail included), and the centripetal spline puts back a smooth curve THROUGH the
//!    survivors — centripetal (α = 0.5) because uniform Catmull-Rom overshoots into cusps
//!    exactly where a hand-drawn stroke doubles back. Endpoints survive all three stages
//!    EXACTLY.
//! 2. **Pseudo-pressure → width** ([`width_profile`]) — see below.
//! 3. **Fill geometry** ([`stroke_fill_polygons`]) — a variable-width ribbon can't be a stroked
//!    polyline, so it is emitted as FILLED polygons: one quad per segment (offset by the local
//!    half-width at each end) plus round discs at the caps and at joints whose turn would
//!    otherwise leave a visible wedge. Every polygon is wound the SAME way and the whole group
//!    fills in ONE non-zero-winding fill, so overlapping passes of a scribble union cleanly
//!    (no cancellation holes where a stroke doubles back over itself, and no double-composited
//!    seams when the group draws at partial alpha).
//!
//! # The pseudo-pressure model
//! A mouse/trackpad has no stylus pressure, so the width SIMULATES it from the gesture, using
//! the two classic proxies a real nib obeys:
//!
//! * **Speed** — a hand pressing deliberately moves slowly; a light flick is fast. The pointer
//!   trail is thinned at [`crate::app::preview::annotate::PEN_MIN_STEP`], so the GAP between
//!   consecutive raw samples is a per-event speed measure that needs no clock: a gap at the
//!   thinning floor means "as slow as the sampler can see" (heavy), [`FAST_GAP`] or more means
//!   a flick (light). [`speed_pressure`] turns the trail into that signal, smoothed;
//!   [`pressure_along`] then transfers it onto the smoothed centerline by ABSOLUTE arc length
//!   and it is STORED with the stroke — the resample throws the raw spacing away, and a bake
//!   must render exactly what the screen showed.
//! * **Curvature** — you bear down through a loop. Local
//!   [Menger curvature](https://en.wikipedia.org/wiki/Menger_curvature), normalized against the
//!   pen's own [`CURVE_REF_W`] reference radius, is recomputed from the geometry every time
//!   (so a resize re-inks coherently, and a stroke with no stored speed signal still has
//!   character).
//!
//! The two blend into one 0..1 pressure around [`NEUTRAL_PRESSURE`], smooth over a window so
//! the ribbon swells organically instead of jittering, and map to a width multiplier bounded by
//! `1 - `[`PRESSURE_LIGHT_DROP`] … `1 + `[`PRESSURE_HEAVY_GAIN`] — the stroke never balloons and
//! never collapses mid-stroke, and the selected 2/4/6px preset is always the nominal weight
//! (neutral pressure on a straight run renders EXACTLY the preset). On top of that, both tips
//! ramp through [`taper_factor`] to [`TIP_WIDTH_FRAC`] of the preset, which is the pressure
//! building as the nib lands and releasing as it lifts. A TAP is a single-point stroke: a firm
//! press, [`dot_width`] (a touch heavier than the preset, matching a heavy stroke's body).
//! Everything is deterministic — a pure function of the stored geometry and the stored
//! pressure, no randomness, no wall-clock.
//!
//! # Live == committed (the parity contract)
//! The stored `AnnotKind::Pen` points ARE the smoothed centerline and the stored pressure IS the
//! speed signal, at ALL times: the drag keeps the raw trail on `EditState::pen_raw` and re-fits
//! [`smooth_path`] + [`pressure_along`] into the model on every sample, so the beautified stroke
//! is what you watch being drawn — releasing the pointer changes NO geometry (there is no
//! commit-time re-shape to pop). Hit-testing, the eraser, merge-connectivity, bounds/resize and
//! the bake therefore all read the one centerline the canvas draws; [`stroke_fill_polygons`]
//! itself does NOT smooth, it only widths-and-outlines what it is given.
//!
//! That only works because the pipeline is CAUSAL: every stage's output for a point depends on
//! a BOUNDED number of samples after it ([`SETTLE_TAIL_POINTS`]), so appending a sample can
//! only change the last stretch of the curve — the settled prefix is bit-identical from the
//! moment it settles through the commit. (The classic global Ramer-Douglas-Peucker fit is
//! prettier per-point but re-decimates the WHOLE stroke every time the far end moves, which
//! reads as the ink wriggling behind the cursor.) Two deliberate exceptions, both bounded and
//! both invisible in practice: [`taper_len`] shrinks with a stroke's own length, so a stroke
//! still shorter than ~8 pen widths re-profiles its width as it grows, and a stroke long
//! enough to exceed [`MAX_POINTS`] resamples at a proportionally coarser step.
//!
//! Display and bake differ ONLY in the mapping handed to [`stroke_fill_polygons`]: the canvas
//! passes its image→screen map and zoom scale, the bake passes `p * scale` and `scale`. Same
//! profile, same polygons — the bake is the display at another resolution.

/// Binomial (1-2-1) low-pass passes run before decimation. Two is enough to bury pointer
/// tremor at the [`crate::app::preview::annotate::PEN_MIN_STEP`] sampling scale without
/// rounding off intentional corners (the decimation + spline handle shape, not this).
const SMOOTH_PASSES: usize = 2;

/// The decimation tolerance (SOURCE px): a point whose removal moves the polyline by less than
/// this carries no intent. Sits just under the 1.5px pointer sampling step, so jitter-scale
/// detail is dropped while a deliberate wiggle survives.
const SIMPLIFY_EPS: f32 = 0.6;

/// How many trailing RAW samples of a live stroke are still in flux — the knob the whole causal
/// fit is tuned by. Everything before them is FINAL: identical during the drag and after the
/// commit. At the 1.5px sampling step this is roughly 20 source px of "tail" behind the cursor,
/// short enough that the settling is invisible and long enough for the decimation to see a
/// span's shape before committing to it.
pub const SETTLE_TAIL_POINTS: usize = 15;

/// How far ahead the decimation may look for the end of one kept span: the settling budget less
/// the low-pass's ±1 and the spline's one-control lookahead. This is what makes the fit CAUSAL —
/// a span's end is decided from at most this many following samples, so the curve behind the
/// cursor stops moving instead of being re-fitted end-to-end on every sample (and the cost
/// stays linear, ~w/2 distance tests per point).
const DECIMATE_WINDOW: usize = SETTLE_TAIL_POINTS - 3;

/// The resample step (SOURCE px) along the spline, as a FRACTION of the base stroke width, and
/// its floor. Width varies over the stroke, so the ribbon is only as smooth as its sampling —
/// but a 6px pen needs less detail than a 2px one. `max(RESAMPLE_MIN, base_w * RESAMPLE_FRAC)`.
const RESAMPLE_FRAC: f32 = 0.5;
const RESAMPLE_MIN: f32 = 1.5;

/// The most sub-samples one spline span may contribute — a guard against a single enormous
/// span (a fast flick between two samples) exploding the point count.
const MAX_SPAN_STEPS: usize = 24;

/// The soft ceiling on a resampled stroke's point count. A stroke long enough to hit it is
/// resampled at a proportionally coarser step instead of being truncated (never lose ink).
const MAX_POINTS: usize = 4096;

/// How thin a stroke gets at its very tips, as a fraction of the nominal width. NOT zero: a
/// literal point leaves anti-aliasing crumbs and a tapped dot must stay a dot. 18% reads as
/// pressure building as the nib lands and releasing as it lifts.
pub const TIP_WIDTH_FRAC: f32 = 0.18;

/// The taper RAMP length, in multiples of the base width — how far in from each tip the stroke
/// reaches full weight.
const TAPER_SPAN_W: f32 = 2.5;

/// …but never more than this fraction of the stroke's own length, per end. A short flick still
/// shows full weight in the middle instead of being taper all the way through.
const TAPER_SPAN_FRAC: f32 = 0.3;

/// The pressure a stroke has with NO speed signal and no curvature — the value that renders
/// EXACTLY the selected preset width. Both proxies swing around it.
pub const NEUTRAL_PRESSURE: f32 = 0.5;

/// How much a fully-heavy pass swells the stroke, as a fraction of the preset width…
pub const PRESSURE_HEAVY_GAIN: f32 = 0.40;

/// …and how much a fully-light (fast flick) pass thins it. Deliberately the smaller of the two:
/// a light stroke must stay a confident mark, never a scratch.
pub const PRESSURE_LIGHT_DROP: f32 = 0.25;

/// How far the SPEED proxy alone can push pressure off neutral (±), and how far CURVATURE can
/// push it up. Both stay inside the 0..1 pressure range after the blend is clamped; together
/// they can saturate, which is exactly what a slow pass through a tight loop should do.
const SPEED_SWING: f32 = 0.7;
const CURVE_SWING: f32 = 0.4;

/// The raw-sample GAP (SOURCE px) that reads as "as slow as the sampler can resolve" — the
/// pointer thinning floor, [`crate::app::preview::annotate::PEN_MIN_STEP`]…
const SLOW_GAP: f32 = 1.5;

/// …and the gap that reads as a full-speed flick. Between them the speed proxy eases.
pub const FAST_GAP: f32 = 14.0;

/// The curvature REFERENCE radius, in multiples of the base width: a curve whose radius is this
/// or tighter reads as fully "bearing down", easing in smoothly from straight. Tying it to the
/// pen's own width keeps a 6px pen from treating its own nib-scale wiggles as loops.
const CURVE_REF_W: f32 = 6.0;

/// Half-window (in samples) the pressure signals are averaged over. Width must change
/// GRADUALLY along the ribbon — a per-sample spike would read as a lump, not as pressure.
const PRESSURE_SMOOTH: usize = 3;

/// How heavy a TAP is, as a multiple of the preset width: a firm press that leaves a small ink
/// pool, sized to match the body of a heavy stroke rather than a hairline.
const DOT_GAIN: f32 = 1.25;

/// The smallest half-width (TARGET px) any ribbon sample renders at, so a tapered tip stays a
/// visible mark at any zoom instead of collapsing to nothing.
const MIN_HALF: f32 = 0.25;

/// How far (TARGET px) a joint's outer wedge may sag before a round disc is inserted to fill
/// it. Sub-pixel wedges cost nothing to leave open; anything coarser shows as a nick.
const JOINT_WEDGE_TOL: f32 = 0.3;

/// The widest any sample of a stroke with base width `base_w` can render — the ceiling the
/// pressure swell is bounded by. Hit-testing, eraser reach, merge slack and the "keep the drawn
/// extent inside the image" margin all size themselves off THIS, not the preset width, so a
/// heavy loop is still fully grabbable/erasable and never spills past the picture edge.
pub fn max_width(base_w: f32) -> f32 {
    base_w * (1.0 + PRESSURE_HEAVY_GAIN)
}

/// The diameter a TAP (a single-point stroke) inks at: a firm press, [`DOT_GAIN`] × the preset.
/// Never wider than [`max_width`], so every reach/margin sized off that still covers a dot.
pub fn dot_width(base_w: f32) -> f32 {
    base_w * DOT_GAIN
}

// ── stage 1: smoothing ────────────────────────────────────────────────────────────────

/// `x*x*(3-2x)` on a value clamped to 0..1 — the ease used by every ramp here (C¹ at both
/// ends, so nothing in the width profile shows a crease).
fn smoothstep01(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

/// Drop consecutive duplicates (within a hair) and any non-finite sample, keeping the first and
/// last real points. A raw trail can hold coincident samples; every later stage divides by
/// segment lengths.
fn dedup(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::with_capacity(points.len());
    for &p in points {
        if !p.0.is_finite() || !p.1.is_finite() {
            continue;
        }
        if out.last().is_none_or(|&l| dist(l, p) > 1e-4) {
            out.push(p);
        }
    }
    out
}

/// One or more binomial (¼, ½, ¼) passes over the interior points; the two ENDPOINTS are
/// pinned, so where the stroke starts and stops is exactly where the hand started and stopped.
fn binomial_smooth(points: &[(f32, f32)], passes: usize) -> Vec<(f32, f32)> {
    let mut cur = points.to_vec();
    if cur.len() < 3 {
        return cur;
    }
    let mut next = cur.clone();
    for _ in 0..passes {
        for i in 1..cur.len() - 1 {
            next[i] = (
                0.25 * cur[i - 1].0 + 0.5 * cur[i].0 + 0.25 * cur[i + 1].0,
                0.25 * cur[i - 1].1 + 0.5 * cur[i].1 + 0.25 * cur[i + 1].1,
            );
        }
        std::mem::swap(&mut cur, &mut next);
    }
    cur
}

/// A moving average of half-width `radius` over `v`, clamped at both ends. The shared smoother
/// for every scalar signal that becomes width.
fn box_average(v: &[f32], radius: usize) -> Vec<f32> {
    let n = v.len();
    (0..n)
        .map(|i| {
            let lo = i.saturating_sub(radius);
            let hi = (i + radius + 1).min(n);
            let win = &v[lo..hi];
            win.iter().sum::<f32>() / win.len().max(1) as f32
        })
        .collect()
}

/// Cumulative arc length along a polyline (same length as `points`, starting at 0).
fn arc_lengths(points: &[(f32, f32)]) -> Vec<f32> {
    let mut s = Vec::with_capacity(points.len());
    let mut acc = 0.0;
    for (i, p) in points.iter().enumerate() {
        if i > 0 {
            acc += dist(points[i - 1], *p);
        }
        s.push(acc);
    }
    s
}

/// Perpendicular distance from `p` to the INFINITE line through `a`–`b` (a degenerate baseline
/// falls back to the point distance).
fn line_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx.hypot(dy);
    if len <= 1e-6 {
        return dist(p, a);
    }
    ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / len
}

/// CAUSAL opening-window decimation with tolerance `eps` and at most `window` samples of
/// lookahead: from each anchor, extend the kept span as far as every skipped point stays
/// within `eps` of the chord, then anchor there. Both endpoints are always kept and the output
/// is always a SUBSEQUENCE of the input, so it can never grow.
///
/// Unlike a global Ramer-Douglas-Peucker fit, a point's fate depends only on the ≤ `window`
/// samples after it — which is exactly what lets the live stroke's settled prefix stop moving
/// while the hand keeps drawing. Pure — unit-tested.
pub fn decimate(points: &[(f32, f32)], eps: f32, window: usize) -> Vec<(f32, f32)> {
    let n = points.len();
    if n < 3 {
        return points.to_vec();
    }
    let window = window.max(1);
    let mut out = Vec::with_capacity(n);
    out.push(points[0]);
    let mut anchor = 0usize;
    while anchor + 1 < n - 1 {
        let limit = (anchor + window).min(n - 1);
        // The farthest end whose skipped interior all stays within `eps` of the chord.
        let mut best = anchor + 1;
        for end in anchor + 2..=limit {
            let fits = (anchor + 1..end)
                .all(|k| line_distance(points[k], points[anchor], points[end]) <= eps);
            if fits {
                best = end;
            } else {
                break;
            }
        }
        out.push(points[best]);
        anchor = best;
    }
    if out.last().copied() != Some(points[n - 1]) {
        out.push(points[n - 1]);
    }
    out
}

/// One CENTRIPETAL Catmull-Rom sample: the point at parameter `u` ∈ 0..1 of the span `p1`→`p2`,
/// shaped by the neighbours `p0`/`p3`. Barry-Goldman pyramidal form over the centripetal knot
/// sequence (`Δt = √chord`), which is what keeps the curve free of the cusps and overshoot
/// uniform Catmull-Rom produces where a hand-drawn stroke doubles back.
fn catmull_rom(p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32), u: f32) -> (f32, f32) {
    // Knots, each advanced by √chord; a zero chord would divide by zero, so every step has a
    // floor (the spans are deduped, but the phantom end controls can coincide with an anchor).
    let knot = |a: (f32, f32), b: (f32, f32)| dist(a, b).sqrt().max(1e-4);
    let t0 = 0.0;
    let t1 = t0 + knot(p0, p1);
    let t2 = t1 + knot(p1, p2);
    let t3 = t2 + knot(p2, p3);
    let t = t1 + (t2 - t1) * u.clamp(0.0, 1.0);
    let lerp = |a: (f32, f32), b: (f32, f32), ta: f32, tb: f32| {
        let d = tb - ta;
        if d.abs() <= 1e-6 {
            return a;
        }
        let w = (t - ta) / d;
        (a.0 + (b.0 - a.0) * w, a.1 + (b.1 - a.1) * w)
    };
    let a1 = lerp(p0, p1, t0, t1);
    let a2 = lerp(p1, p2, t1, t2);
    let a3 = lerp(p2, p3, t2, t3);
    let b1 = lerp(a1, a2, t0, t2);
    let b2 = lerp(a2, a3, t1, t3);
    lerp(b1, b2, t1, t2)
}

/// Resample a control polyline as a centripetal Catmull-Rom curve at roughly `step` spacing.
/// Both endpoints are emitted EXACTLY (the spline interpolates its controls, and each span's
/// last sub-sample IS the span's end control); the end spans use a reflected phantom control so
/// the curve leaves/enters straight rather than hooking.
fn resample_spline(ctrl: &[(f32, f32)], step: f32) -> Vec<(f32, f32)> {
    if ctrl.len() < 3 {
        return ctrl.to_vec();
    }
    let step = step.max(0.25);
    // Coarsen the step (never truncate) if this stroke would blow the point budget.
    let total: f32 = ctrl.windows(2).map(|w| dist(w[0], w[1])).sum();
    let step = step.max(total / MAX_POINTS as f32);
    let n = ctrl.len();
    // A reflected phantom keeps the first/last span's tangent aligned with the stroke.
    let phantom = |anchor: (f32, f32), inner: (f32, f32)| (2.0 * anchor.0 - inner.0, 2.0 * anchor.1 - inner.1);
    let mut out = Vec::with_capacity(n * 4);
    out.push(ctrl[0]);
    for i in 0..n - 1 {
        let p0 = if i == 0 { phantom(ctrl[0], ctrl[1]) } else { ctrl[i - 1] };
        let (p1, p2) = (ctrl[i], ctrl[i + 1]);
        let p3 = if i + 2 < n { ctrl[i + 2] } else { phantom(ctrl[n - 1], ctrl[n - 2]) };
        let steps = ((dist(p1, p2) / step).ceil() as usize).clamp(1, MAX_SPAN_STEPS);
        for s in 1..=steps {
            let u = s as f32 / steps as f32;
            // The last sub-sample IS the span's end control — emit it exactly, so the final
            // point of the stroke is bit-for-bit where the hand lifted.
            out.push(if s == steps { p2 } else { catmull_rom(p0, p1, p2, p3, u) });
        }
    }
    out
}

/// Turn a raw pointer trail into the smoothed centerline the pen actually draws: low-pass →
/// causal decimation → centripetal Catmull-Rom resample at a width-aware step. FIRST and LAST
/// points are preserved exactly; a 0/1/2-point input comes back unchanged (deduped). Appending
/// samples only changes the last [`SETTLE_TAIL_POINTS`] samples' worth of curve, which is what
/// makes the live stroke and the committed one the same geometry. Pure — unit-tested. `base_w`
/// only sizes the resample step (a fat pen needs less detail).
pub fn smooth_path(points: &[(f32, f32)], base_w: f32) -> Vec<(f32, f32)> {
    let pts = dedup(points);
    if pts.len() < 3 {
        return pts;
    }
    let low = binomial_smooth(&pts, SMOOTH_PASSES);
    let ctrl = decimate(&low, SIMPLIFY_EPS, DECIMATE_WINDOW);
    let step = (base_w * RESAMPLE_FRAC).max(RESAMPLE_MIN);
    resample_spline(&ctrl, step)
}

// ── stage 2: pseudo-pressure ──────────────────────────────────────────────────────────

/// The SPEED half of the pseudo-pressure, per RAW sample: `1` where the hand crawled (a gap at
/// the pointer thinning floor — pressing deliberately), easing to `0` at a [`FAST_GAP`] flick.
/// Smoothed over [`PRESSURE_SMOOTH`] samples so it swells rather than flickers. The first
/// sample inherits the second's (a stroke's very start has no gap to measure, and the entry
/// taper owns that stretch anyway). Pure — unit-tested.
pub fn speed_pressure(raw: &[(f32, f32)]) -> Vec<f32> {
    let n = raw.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![1.0]; // a tap: a firm press
    }
    let mut p = vec![0.0f32; n];
    for i in 1..n {
        let gap = dist(raw[i - 1], raw[i]);
        p[i] = 1.0 - smoothstep01((gap - SLOW_GAP) / (FAST_GAP - SLOW_GAP));
    }
    p[0] = p[1];
    box_average(&p, PRESSURE_SMOOTH)
}

/// Transfer the raw trail's [`speed_pressure`] onto the smoothed centerline `out`, by ABSOLUTE
/// arc length (never by fraction — a fraction would re-map the whole stroke every time the far
/// end grows, and the settled prefix must not move). Returns one value per point of `out`;
/// an empty/mismatched input yields an empty vector, which every consumer reads as
/// [`NEUTRAL_PRESSURE`]. Pure — unit-tested.
pub fn pressure_along(raw: &[(f32, f32)], out: &[(f32, f32)]) -> Vec<f32> {
    if out.is_empty() {
        return Vec::new();
    }
    let src = speed_pressure(raw);
    if src.len() < 2 {
        return vec![src.first().copied().unwrap_or(NEUTRAL_PRESSURE); out.len()];
    }
    let s_raw = arc_lengths(raw);
    let s_out = arc_lengths(out);
    let mut j = 0usize;
    out.iter()
        .enumerate()
        .map(|(i, _)| {
            let s = s_out[i];
            while j + 1 < s_raw.len() - 1 && s_raw[j + 1] < s {
                j += 1;
            }
            let (a, b) = (s_raw[j], s_raw[j + 1]);
            let t = if b > a { ((s - a) / (b - a)).clamp(0.0, 1.0) } else { 0.0 };
            src[j] + (src[j + 1] - src[j]) * t
        })
        .collect()
}

/// How far in from each tip (SOURCE px) the taper ramps to full weight, for a stroke of base
/// width `base_w` and total arc length `total_len`: [`TAPER_SPAN_W`] widths, capped at
/// [`TAPER_SPAN_FRAC`] of the stroke so a short flick still reaches full weight mid-stroke.
/// Pure — unit-tested.
pub fn taper_len(base_w: f32, total_len: f32) -> f32 {
    (base_w.max(0.0) * TAPER_SPAN_W).min(total_len.max(0.0) * TAPER_SPAN_FRAC)
}

/// The taper RAMP at `d` arc-px from the nearer tip: `0` exactly at the tip, `1` at and beyond
/// `len`, eased in between — the pressure building as the nib lands and releasing as it lifts.
/// The width it drives never actually reaches zero: a tip renders at [`TIP_WIDTH_FRAC`] of the
/// nominal width, so a stroke pinches without vanishing. A non-positive `len` (a zero-length
/// stroke) is "already full weight". Pure — unit-tested.
pub fn taper_factor(d: f32, len: f32) -> f32 {
    if len <= 0.0 {
        return 1.0;
    }
    smoothstep01(d / len)
}

/// The width MULTIPLIER a 0..1 pseudo-pressure inks at: [`NEUTRAL_PRESSURE`] is exactly `1`
/// (the selected preset), full pressure is `1 + `[`PRESSURE_HEAVY_GAIN`], zero pressure is
/// `1 - `[`PRESSURE_LIGHT_DROP`]. Linear either side of neutral, so the two swings stay
/// independently tunable. Pure — unit-tested.
pub fn pressure_multiplier(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    if p >= NEUTRAL_PRESSURE {
        let up = (p - NEUTRAL_PRESSURE) / (1.0 - NEUTRAL_PRESSURE);
        1.0 + PRESSURE_HEAVY_GAIN * up
    } else {
        let down = (NEUTRAL_PRESSURE - p) / NEUTRAL_PRESSURE;
        1.0 - PRESSURE_LIGHT_DROP * down
    }
}

/// Menger curvature (1/radius of the circle through the three points) at `b`. `0` for collinear
/// or coincident points. Pure.
fn menger_curvature(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    let (ab, bc, ca) = (dist(a, b), dist(b, c), dist(c, a));
    let denom = ab * bc * ca;
    if denom <= 1e-6 {
        return 0.0;
    }
    let area2 = ((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)).abs();
    (2.0 * area2 / denom).max(0.0)
}

/// The blended pseudo-pressure per point (0..1): [`NEUTRAL_PRESSURE`], pushed by the STORED
/// speed signal (± [`SPEED_SWING`]) and by the curvature recomputed from the geometry (up to
/// [`CURVE_SWING`]), then smoothed over [`PRESSURE_SMOOTH`] samples. `speed` may be empty or
/// the wrong length (an older/plain stroke, or one built without a trail) — it then reads as
/// neutral throughout and the curvature alone gives the stroke its character. Pure —
/// unit-tested.
pub fn blended_pressure(points: &[(f32, f32)], base_w: f32, speed: &[f32]) -> Vec<f32> {
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }
    let base_w = base_w.max(0.1);
    let use_speed = speed.len() == n;
    let r_ref = base_w * CURVE_REF_W;
    let mut k = vec![0.0f32; n];
    for i in 1..n.saturating_sub(1) {
        k[i] = smoothstep01(menger_curvature(points[i - 1], points[i], points[i + 1]) * r_ref);
    }
    if n >= 3 {
        k[0] = k[1];
        k[n - 1] = k[n - 2];
    }
    let raw: Vec<f32> = (0..n)
        .map(|i| {
            let sp = if use_speed { speed[i].clamp(0.0, 1.0) } else { NEUTRAL_PRESSURE };
            (NEUTRAL_PRESSURE + SPEED_SWING * (sp - NEUTRAL_PRESSURE) + CURVE_SWING * k[i])
                .clamp(0.0, 1.0)
        })
        .collect();
    box_average(&raw, PRESSURE_SMOOTH)
}

/// The per-point WIDTH (SOURCE px) of a stroke: the selected preset scaled by the pseudo-
/// pressure ([`blended_pressure`] → [`pressure_multiplier`]) and by the end taper. A straight
/// neutral-pressure sample is EXACTLY `base_w`; the tips ease down to `base_w × `
/// [`TIP_WIDTH_FRAC`]; nothing ever exceeds [`max_width`] or drops below
/// `base_w × (1 - `[`PRESSURE_LIGHT_DROP`]`)` between the tapers. A single point is a TAP —
/// [`dot_width`], a firm press. Pure and deterministic (stored geometry + stored speed signal
/// only) — unit-tested.
pub fn width_profile(points: &[(f32, f32)], base_w: f32, speed: &[f32]) -> Vec<f32> {
    let n = points.len();
    let base_w = base_w.max(0.1);
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![dot_width(base_w)];
    }
    let s = arc_lengths(points);
    let total = s[n - 1];
    let pressure = blended_pressure(points, base_w, speed);
    let tl = taper_len(base_w, total);
    (0..n)
        .map(|i| {
            let d = s[i].min(total - s[i]);
            let taper = TIP_WIDTH_FRAC + (1.0 - TIP_WIDTH_FRAC) * taper_factor(d, tl);
            base_w * pressure_multiplier(pressure[i]) * taper
        })
        .collect()
}

// ── stage 3: the fill geometry ────────────────────────────────────────────────────────

/// Split any segment longer than `max_step` into equal parts (interpolating the parallel speed
/// signal with it), so the width profile has samples to RAMP over. A live stroke arrives
/// already resampled at that spacing and passes through untouched; this is what keeps a
/// two-point path, a fast flick's single long span, or a stroke blown up by a resize from
/// rendering as one flat quad with no taper. The total is bounded by [`MAX_POINTS`].
/// Pure — unit-tested.
fn densify(points: &[(f32, f32)], press: &[f32], max_step: f32) -> (Vec<(f32, f32)>, Vec<f32>) {
    let n = points.len();
    if n < 2 {
        return (points.to_vec(), press.to_vec());
    }
    let use_press = press.len() == n;
    let total: f32 = points.windows(2).map(|w| dist(w[0], w[1])).sum();
    let step = max_step.max(0.25).max(total / MAX_POINTS as f32);
    let mut pts = Vec::with_capacity(n);
    let mut pr = Vec::with_capacity(n);
    pts.push(points[0]);
    pr.push(if use_press { press[0] } else { NEUTRAL_PRESSURE });
    for i in 0..n - 1 {
        let (a, b) = (points[i], points[i + 1]);
        // Bounded by the point budget rather than by [`MAX_SPAN_STEPS`]: the `step` floor above
        // already caps the TOTAL, and a single long span still deserves a smooth ramp.
        let parts = ((dist(a, b) / step).ceil() as usize).clamp(1, MAX_POINTS);
        let (pa, pb) = if use_press {
            (press[i], press[i + 1])
        } else {
            (NEUTRAL_PRESSURE, NEUTRAL_PRESSURE)
        };
        for s in 1..=parts {
            let t = s as f32 / parts as f32;
            pts.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
            pr.push(pa + (pb - pa) * t);
        }
    }
    (pts, pr)
}

/// A closed disc polygon (`segs`-gon) at `c`, wound to match the segment quads below — every
/// piece of a stroke must share one winding direction for the non-zero fill to UNION them.
fn disc(c: (f32, f32), r: f32, out: &mut Vec<Vec<(f32, f32)>>) {
    let r = r.max(MIN_HALF);
    let segs = ((r * 2.0) as usize).clamp(8, 24);
    let mut poly = Vec::with_capacity(segs);
    for i in 0..segs {
        // DEcreasing angle: the same (negative) signed area as the quads.
        let a = -std::f32::consts::TAU * (i as f32) / (segs as f32);
        poly.push((c.0 + r * a.cos(), c.1 + r * a.sin()));
    }
    out.push(poly);
}

/// The FILLED polygons of one freehand stroke, in TARGET space — the geometry BOTH the live
/// canvas and the bake draw, differing only in the `map`/`scale` they hand in.
///
/// `points` is the stroke's centerline in SOURCE px (already smoothed — the model stores the
/// smoothed curve, live and committed alike), `base_w` the selected preset width in SOURCE px,
/// `speed` the stored per-point speed signal (empty ⇒ neutral pressure); `map` takes a SOURCE
/// point to target space and `scale` is that mapping's uniform scale factor (target px per
/// source px), which the half-widths ride.
///
/// The result is one quad per segment (offset by the local half-width at each end) plus discs
/// at both caps and at any joint whose turn would leave a visible wedge. Every polygon is
/// closed and wound the SAME way: fill them as ONE path with the NON-ZERO rule and overlapping
/// passes union cleanly — no cancellation holes where a scribble crosses itself, and no
/// double-composited seams when the group draws at partial alpha. Pure — unit-tested.
pub fn stroke_fill_polygons(
    points: &[(f32, f32)],
    base_w: f32,
    speed: &[f32],
    map: impl Fn((f32, f32)) -> (f32, f32),
    scale: f32,
) -> Vec<Vec<(f32, f32)>> {
    // Dedup must not silently desync the parallel speed signal: only drop it if it shifted.
    let deduped = dedup(points);
    let speed = if speed.len() == points.len() && deduped.len() == points.len() { speed } else { &[] };
    if deduped.is_empty() {
        return Vec::new();
    }
    // Give the profile something to ramp over: a stroke whose samples are coarser than the
    // resample step (a two-point path, one long flick span, a resize that blew it up) would
    // otherwise render as one flat quad with no taper at all.
    let (pts, speed) = densify(&deduped, speed, (base_w * RESAMPLE_FRAC).max(RESAMPLE_MIN));
    let widths = width_profile(&pts, base_w, &speed);
    let tp: Vec<(f32, f32)> = pts.iter().map(|p| map(*p)).collect();
    let half: Vec<f32> = widths.iter().map(|w| (w * 0.5 * scale.abs()).max(MIN_HALF)).collect();
    let mut out: Vec<Vec<(f32, f32)>> = Vec::with_capacity(tp.len() * 2);
    if tp.len() == 1 {
        // A tap: a firm-press dot (the profile never tapers a single point away).
        disc(tp[0], half[0], &mut out);
        return out;
    }
    // Unit direction per segment (skipping degenerate ones), and the quad it sweeps.
    let mut dirs: Vec<Option<(f32, f32)>> = Vec::with_capacity(tp.len() - 1);
    for i in 0..tp.len() - 1 {
        let (a, b) = (tp[i], tp[i + 1]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = dx.hypot(dy);
        if len <= 1e-5 {
            dirs.push(None);
            continue;
        }
        let d = (dx / len, dy / len);
        dirs.push(Some(d));
        let n = (-d.1, d.0);
        let (ha, hb) = (half[i], half[i + 1]);
        out.push(vec![
            (a.0 + n.0 * ha, a.1 + n.1 * ha),
            (b.0 + n.0 * hb, b.1 + n.1 * hb),
            (b.0 - n.0 * hb, b.1 - n.1 * hb),
            (a.0 - n.0 * ha, a.1 - n.1 * ha),
        ]);
    }
    if out.is_empty() {
        // Every segment was degenerate (a stroke that never moved): still a dot.
        disc(tp[0], half[0], &mut out);
        return out;
    }
    // Round CAPS at both ends — with the taper they are tiny, and they are what makes the
    // pinch read as a nib landing and lifting rather than a chopped-off rectangle.
    disc(tp[0], half[0], &mut out);
    disc(tp[tp.len() - 1], half[tp.len() - 1], &mut out);
    // Round JOINS only where the turn would actually show a wedge (sag ≈ h·(1 − cos(θ/2))).
    // On a smoothed stroke most joints turn a degree or two and cost nothing to leave open;
    // this keeps the polygon count near one per segment instead of two.
    for i in 1..tp.len() - 1 {
        let (Some(d0), Some(d1)) = (dirs[i - 1], dirs[i]) else { continue };
        let cos_t = (d0.0 * d1.0 + d0.1 * d1.1).clamp(-1.0, 1.0);
        // cos(θ/2) from the half-angle identity, guarding the reversal case.
        let half_cos = ((1.0 + cos_t) * 0.5).max(0.0).sqrt();
        if half[i] * (1.0 - half_cos) > JOINT_WEDGE_TOL {
            disc(tp[i], half[i], &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(p: (f32, f32)) -> (f32, f32) {
        p
    }

    /// Non-zero winding number of a closed polygon around `p` — used to verify the ribbon
    /// actually covers the centerline it was built from.
    fn winding(poly: &[(f32, f32)], p: (f32, f32)) -> i32 {
        let mut w = 0;
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            let side = (b.0 - a.0) * (p.1 - a.1) - (p.0 - a.0) * (b.1 - a.1);
            if a.1 <= p.1 {
                if b.1 > p.1 && side > 0.0 {
                    w += 1;
                }
            } else if b.1 <= p.1 && side < 0.0 {
                w -= 1;
            }
        }
        w
    }

    fn covered(polys: &[Vec<(f32, f32)>], p: (f32, f32)) -> bool {
        polys.iter().any(|poly| winding(poly, p) != 0)
    }

    fn line(n: usize, len: f32) -> Vec<(f32, f32)> {
        (0..n).map(|i| (len * i as f32 / (n - 1) as f32, 50.0)).collect()
    }

    /// A jittery hand-drawn arc, sampled at roughly the pen's 1.5px step.
    fn hand_drawn(n: usize) -> Vec<(f32, f32)> {
        (0..n)
            .map(|i| {
                let t = i as f32 * 0.05;
                let jitter = if i % 2 == 0 { 0.35 } else { -0.35 };
                (20.0 + t * 40.0, 60.0 + (t * 1.3).sin() * 25.0 + jitter)
            })
            .collect()
    }

    #[test]
    fn smoothing_keeps_the_endpoints_and_never_nans_on_tiny_inputs() {
        assert!(smooth_path(&[], 4.0).is_empty());
        assert_eq!(smooth_path(&[(3.0, 4.0)], 4.0), vec![(3.0, 4.0)]);
        assert_eq!(smooth_path(&[(0.0, 0.0), (9.0, 0.0)], 4.0), vec![(0.0, 0.0), (9.0, 0.0)]);
        // Coincident samples collapse instead of dividing by zero downstream.
        assert_eq!(smooth_path(&[(2.0, 2.0), (2.0, 2.0), (2.0, 2.0)], 4.0), vec![(2.0, 2.0)]);
        // A real stroke: first and last survive exactly, nothing is NaN.
        let raw = hand_drawn(60);
        let out = smooth_path(&raw, 4.0);
        assert_eq!(out.first().copied(), raw.first().copied(), "start is pinned");
        assert_eq!(out.last().copied(), raw.last().copied(), "end is pinned");
        assert!(out.iter().all(|p| p.0.is_finite() && p.1.is_finite()));
    }

    #[test]
    fn smoothing_removes_jitter_but_keeps_the_shape() {
        // A straight run with ±0.45px tremor: the smoothed curve rides the true line far
        // closer than the raw trail did.
        let raw: Vec<(f32, f32)> =
            (0..60).map(|i| (i as f32 * 2.0, 10.0 + if i % 2 == 0 { 0.45 } else { -0.45 })).collect();
        let out = smooth_path(&raw, 4.0);
        // Measured over the BODY of the stroke (x ∈ 30..90 of a 0..118 run): the two endpoints
        // are pinned to exactly where the hand pressed and lifted, tremor and all, so the curve
        // easing off those anchors is by design — it is the middle that must be clean.
        let worst = out
            .iter()
            .filter(|p| (30.0..=90.0).contains(&p.0))
            .map(|p| (p.1 - 10.0).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.05, "tremor survived: {worst}");
        // A deliberate 90° corner is still a corner (the spline rounds it, it does not erase it).
        let mut corner: Vec<(f32, f32)> = (0..20).map(|i| (i as f32 * 5.0, 0.0)).collect();
        corner.extend((1..20).map(|i| (95.0, i as f32 * 5.0)));
        let out = smooth_path(&corner, 4.0);
        assert!(out.iter().any(|p| p.0 > 90.0 && p.1 > 80.0), "the vertical leg survives");
    }

    #[test]
    fn decimation_is_monotone_causal_and_keeps_the_ends() {
        let pts: Vec<(f32, f32)> = (0..50).map(|i| (i as f32, (i as f32 * 0.05).sin() * 8.0)).collect();
        let out = decimate(&pts, 0.6, DECIMATE_WINDOW);
        assert!(out.len() <= pts.len(), "decimation never grows a path");
        assert_eq!(out.first().copied(), pts.first().copied());
        assert_eq!(out.last().copied(), pts.last().copied());
        // Every kept point is one of the input points, in order.
        let mut it = pts.iter();
        assert!(out.iter().all(|p| it.any(|q| q == p)), "the output is a subsequence");
        // A straight run collapses as far as the lookahead window allows.
        let straight: Vec<(f32, f32)> = (0..25).map(|i| (i as f32 * 4.0, 7.0)).collect();
        assert!(decimate(&straight, 0.6, DECIMATE_WINDOW).len() <= 4);
        // Coarser tolerance can only keep fewer points; tiny inputs pass through.
        assert!(decimate(&pts, 4.0, DECIMATE_WINDOW).len() <= out.len());
        assert_eq!(decimate(&pts[..2], 0.6, DECIMATE_WINDOW), pts[..2].to_vec());
    }

    #[test]
    fn the_settled_prefix_is_final_while_the_hand_keeps_drawing() {
        // THE live-vs-committed contract: appending samples must not re-shape the ink already
        // behind the cursor. Only the last SETTLE_TAIL_POINTS raw samples may still move.
        let raw = hand_drawn(120);
        let partial = smooth_path(&raw[..90], 4.0);
        let full = smooth_path(&raw, 4.0);
        // How much of the partial fit survives verbatim in the longer one?
        let same = partial
            .iter()
            .zip(&full)
            .take_while(|(a, b)| a.0.to_bits() == b.0.to_bits() && a.1.to_bits() == b.1.to_bits())
            .count();
        assert!(same > 0, "nothing settled at all");
        // The stretch that DID move is bounded by the lookahead, measured in source px.
        let moved: f32 = partial[same.saturating_sub(1)..].windows(2).map(|w| dist(w[0], w[1])).sum();
        let budget = SETTLE_TAIL_POINTS as f32 * 1.5 * 2.0; // samples × step, with slack
        assert!(moved <= budget, "the unsettled tail is {moved}px, budget {budget}px");
        // And committing (no further samples) changes NOTHING at all.
        assert_eq!(smooth_path(&raw, 4.0), full, "the fit is deterministic");
        // The pressure signal settles with it — same prefix, same values.
        let pp = pressure_along(&raw[..90], &partial);
        let pf = pressure_along(&raw, &full);
        for i in 0..same.min(pp.len()).min(pf.len()).saturating_sub(SETTLE_TAIL_POINTS) {
            assert!((pp[i] - pf[i]).abs() < 1e-3, "pressure moved at {i}: {} vs {}", pp[i], pf[i]);
        }
    }

    #[test]
    fn speed_pressure_reads_slow_as_heavy_and_flicks_as_light() {
        // Crawling (samples at the thinning floor) is a full press…
        let slow: Vec<(f32, f32)> = (0..20).map(|i| (i as f32 * SLOW_GAP, 0.0)).collect();
        let sp = speed_pressure(&slow);
        assert_eq!(sp.len(), slow.len());
        assert!(sp.iter().all(|p| *p > 0.95), "a crawl is a heavy press: {sp:?}");
        // …a flick is light.
        let fast: Vec<(f32, f32)> = (0..20).map(|i| (i as f32 * (FAST_GAP + 4.0), 0.0)).collect();
        let fp = speed_pressure(&fast);
        assert!(fp.iter().all(|p| *p < 0.05), "a flick is light: {fp:?}");
        // Degeneracies.
        assert!(speed_pressure(&[]).is_empty());
        assert_eq!(speed_pressure(&[(1.0, 1.0)]), vec![1.0], "a tap is a firm press");
        assert!(speed_pressure(&slow).iter().all(|p| p.is_finite()));
        // Transferred onto a resampled centerline: one value per point, in range.
        let out = smooth_path(&slow, 4.0);
        let along = pressure_along(&slow, &out);
        assert_eq!(along.len(), out.len());
        assert!(along.iter().all(|p| (0.0..=1.0).contains(p)));
        assert!(pressure_along(&slow, &[]).is_empty());
    }

    #[test]
    fn pressure_maps_to_a_bounded_multiplier_anchored_on_the_preset() {
        assert_eq!(pressure_multiplier(NEUTRAL_PRESSURE), 1.0, "neutral IS the preset width");
        assert!((pressure_multiplier(1.0) - (1.0 + PRESSURE_HEAVY_GAIN)).abs() < 1e-6);
        assert!((pressure_multiplier(0.0) - (1.0 - PRESSURE_LIGHT_DROP)).abs() < 1e-6);
        // Monotone and bounded, including out-of-range inputs.
        assert!(pressure_multiplier(0.25) < 1.0 && pressure_multiplier(0.75) > 1.0);
        assert_eq!(pressure_multiplier(9.0), pressure_multiplier(1.0));
        assert_eq!(pressure_multiplier(-9.0), pressure_multiplier(0.0));
    }

    #[test]
    fn taper_is_zero_at_the_tips_and_full_in_the_middle() {
        assert_eq!(taper_factor(0.0, 10.0), 0.0, "exactly at the tip the ramp is 0");
        assert_eq!(taper_factor(10.0, 10.0), 1.0);
        assert_eq!(taper_factor(50.0, 10.0), 1.0, "past the ramp it stays full");
        assert!(taper_factor(5.0, 10.0) > 0.0 && taper_factor(5.0, 10.0) < 1.0);
        assert!(taper_factor(3.0, 10.0) < taper_factor(7.0, 10.0), "monotone in between");
        // A zero-length stroke has no ramp to walk — full weight, never a divide by zero.
        assert_eq!(taper_factor(0.0, 0.0), 1.0);
        // The ramp is width-scaled but never eats more than its share of a short stroke.
        assert_eq!(taper_len(4.0, 1000.0), 10.0);
        assert!((taper_len(4.0, 20.0) - 6.0).abs() < 1e-4, "short stroke: capped at 30%");
    }

    #[test]
    fn width_is_the_preset_in_a_straight_neutral_middle_and_pinched_at_both_ends() {
        let pts = line(40, 200.0);
        let w = width_profile(&pts, 4.0, &[]);
        assert_eq!(w.len(), pts.len());
        let mid = w[w.len() / 2];
        assert!((mid - 4.0).abs() < 1e-3, "straight + neutral is exactly the preset: {mid}");
        assert!((w[0] - 4.0 * TIP_WIDTH_FRAC).abs() < 1e-3, "the tip is the pinch fraction");
        assert!((*w.last().expect("width") - 4.0 * TIP_WIDTH_FRAC).abs() < 1e-3);
        assert!(w[1] > w[0] && w[2] > w[1], "monotone ramp out of the tip");
        // Degeneracies stay well-defined.
        assert!(width_profile(&[], 4.0, &[]).is_empty());
        assert_eq!(width_profile(&[(1.0, 1.0)], 4.0, &[]), vec![dot_width(4.0)], "a tap is a firm dot");
        assert!(width_profile(&pts, 0.0, &[]).iter().all(|w| *w > 0.0), "zero base still has width");
        // A mismatched speed signal is ignored (neutral), never a panic.
        assert_eq!(width_profile(&pts, 4.0, &[0.9, 0.1]), w);
    }

    #[test]
    fn speed_makes_a_slow_pass_heavier_than_a_flick_at_the_same_shape() {
        let pts = line(40, 200.0);
        let heavy = width_profile(&pts, 4.0, &vec![1.0; pts.len()]);
        let light = width_profile(&pts, 4.0, &vec![0.0; pts.len()]);
        let m = pts.len() / 2;
        assert!(heavy[m] > 4.0 * 1.15, "a slow pass inks heavier: {}", heavy[m]);
        assert!(light[m] < 4.0 * 0.9, "a flick inks lighter: {}", light[m]);
        assert!(light[m] > 4.0 * 0.7, "…but never collapses: {}", light[m]);
        assert!(heavy.iter().all(|w| *w <= max_width(4.0) + 1e-3), "bounded by max_width");
    }

    #[test]
    fn a_tight_loop_swells_and_the_swell_is_bounded() {
        // A radius-12 circle under a 4px pen (reference radius 24) is well inside the "bearing
        // down" regime: it must be visibly heavier than the same pen drawn straight.
        let circle: Vec<(f32, f32)> = (0..=72)
            .map(|i| {
                let a = std::f32::consts::TAU * i as f32 / 72.0;
                (60.0 + 12.0 * a.cos(), 60.0 + 12.0 * a.sin())
            })
            .collect();
        let w = width_profile(&circle, 4.0, &[]);
        let mid = w[w.len() / 2];
        assert!(mid > 4.0 * 1.15, "a tight loop inks heavier: {mid}");
        assert!(w.iter().all(|x| *x <= max_width(4.0) + 1e-3), "the swell is bounded by max_width");
        // A gentle 300px-radius arc is effectively straight — no swell worth seeing.
        let arc: Vec<(f32, f32)> = (0..=60)
            .map(|i| {
                let a = 0.5 * i as f32 / 60.0;
                (300.0 * a.sin(), 300.0 * (1.0 - a.cos()))
            })
            .collect();
        assert!(width_profile(&arc, 4.0, &[])[30] < 4.0 * 1.05, "a gentle arc stays near the preset");
        // Blended pressure stays in range whatever it is fed.
        let p = blended_pressure(&circle, 4.0, &vec![1.0; circle.len()]);
        assert!(p.iter().all(|x| (0.0..=1.0).contains(x)));
    }

    #[test]
    fn fill_polygons_cover_the_centerline_and_pinch_at_the_tips() {
        let pts = line(30, 300.0);
        let polys = stroke_fill_polygons(&pts, 6.0, &[], ident, 1.0);
        assert!(!polys.is_empty());
        assert!(polys.iter().all(|p| p.len() >= 4), "every piece is a real closed polygon");
        assert!(polys.iter().flatten().all(|p| p.0.is_finite() && p.1.is_finite()));
        // The ribbon covers the centerline it was built from.
        for t in [0.1f32, 0.3, 0.5, 0.7, 0.9] {
            assert!(covered(&polys, (300.0 * t, 50.0)), "centerline uncovered at {t}");
        }
        // …and is thinner at the tip than mid-stroke: 2px off the centerline is inside the
        // body mid-stroke but outside the pinched tip.
        assert!(covered(&polys, (150.0, 52.0)), "mid-stroke is at least 2px half-width");
        assert!(!covered(&polys, (0.5, 52.0)), "the tip is pinched well under 2px");
        // Nothing spills past the width ceiling.
        let ceiling = max_width(6.0) * 0.5;
        assert!(
            polys.iter().flatten().all(|p| (p.1 - 50.0).abs() <= ceiling + 1e-2),
            "the ribbon stays inside max_width"
        );
    }

    #[test]
    fn fill_polygons_survive_every_degeneracy() {
        assert!(stroke_fill_polygons(&[], 4.0, &[], ident, 1.0).is_empty());
        // A tap is a firm-press dot, not a vanished taper.
        let dot = stroke_fill_polygons(&[(10.0, 10.0)], 4.0, &[], ident, 1.0);
        assert_eq!(dot.len(), 1);
        assert!(covered(&dot, (10.0, 10.0)));
        assert!(covered(&dot, (11.9, 10.0)), "a 4px tap pools a little wider than the preset");
        assert!(!covered(&dot, (10.0 + max_width(4.0), 10.0)), "…but stays inside max_width");
        // A "stroke" that never moved is still a dot, not an empty fill.
        let still = stroke_fill_polygons(&[(5.0, 5.0), (5.0, 5.0), (5.0, 5.0)], 4.0, &[], ident, 1.0);
        assert_eq!(still.len(), 1);
        assert!(covered(&still, (5.0, 5.0)));
        // A two-point hairline stroke still produces a ribbon around its centre.
        let tiny = stroke_fill_polygons(&[(0.0, 0.0), (3.0, 0.0)], 2.0, &[], ident, 1.0);
        assert!(!tiny.is_empty());
        assert!(covered(&tiny, (1.5, 0.0)));
        // A zero scale collapses to the minimum half-width instead of producing NaNs.
        let flat = stroke_fill_polygons(&line(10, 50.0), 4.0, &[], ident, 0.0);
        assert!(flat.iter().flatten().all(|p| p.0.is_finite() && p.1.is_finite()));
        // A speed signal of the wrong length is ignored rather than mis-indexed.
        let bad = stroke_fill_polygons(&line(10, 50.0), 4.0, &[0.3], ident, 1.0);
        assert_eq!(bad.len(), stroke_fill_polygons(&line(10, 50.0), 4.0, &[], ident, 1.0).len());
    }

    #[test]
    fn coarse_paths_are_densified_so_the_taper_has_somewhere_to_ramp() {
        // A two-point path (a test fixture, a resized stroke, one flick span) has no interior
        // samples — without densifying, BOTH its points are tips and the whole thing would ink
        // at the pinch width. It must still show a full-weight body.
        let (pts, pr) = densify(&[(0.0, 0.0), (60.0, 0.0)], &[], 2.0);
        assert!(pts.len() >= 30, "a 60px span at a 2px step: {}", pts.len());
        assert_eq!(pts.first().copied(), Some((0.0, 0.0)), "endpoints are exact");
        assert_eq!(pts.last().copied(), Some((60.0, 0.0)));
        assert!(pr.iter().all(|p| (*p - NEUTRAL_PRESSURE).abs() < 1e-6), "absent ⇒ neutral");
        // An already-fine path passes through unchanged.
        let fine = line(30, 58.0);
        assert_eq!(densify(&fine, &[], 2.0).0.len(), fine.len());
        // The speed signal interpolates ALONG the split, staying in step point-for-point.
        let (pts, pr) = densify(&[(0.0, 0.0), (20.0, 0.0)], &[1.0, 0.0], 5.0);
        assert_eq!(pts.len(), pr.len());
        assert!(pr.windows(2).all(|w| w[1] <= w[0]), "monotone from 1 down to 0: {pr:?}");
        // …and the ribbon it feeds really does reach full weight in the middle.
        let polys = stroke_fill_polygons(&[(0.0, 50.0), (60.0, 50.0)], 8.0, &[], ident, 1.0);
        assert!(covered(&polys, (30.0, 53.0)), "the body of a two-point stroke is full weight");
        assert!(!covered(&polys, (0.5, 53.0)), "…and its tips still pinch");
    }

    #[test]
    fn every_polygon_shares_one_winding_so_overlaps_union() {
        // The non-zero fill only unions a self-crossing scribble if every piece is wound the
        // same way — a reversed piece would punch a hole where the stroke crosses itself.
        let mut pts: Vec<(f32, f32)> = (0..40).map(|i| (i as f32 * 4.0, 100.0)).collect();
        pts.extend((0..40).map(|i| (156.0 - i as f32 * 4.0, 104.0))); // doubles straight back
        let polys = stroke_fill_polygons(&pts, 6.0, &[], ident, 1.0);
        let area = |poly: &Vec<(f32, f32)>| {
            let mut a = 0.0;
            for i in 0..poly.len() {
                let p = poly[i];
                let q = poly[(i + 1) % poly.len()];
                a += p.0 * q.1 - q.0 * p.1;
            }
            a * 0.5
        };
        assert!(polys.iter().all(|p| area(p) < 0.0), "all pieces share one winding sign");
        // The overlap region is covered (a union), never cancelled into a hole.
        assert!(covered(&polys, (80.0, 102.0)));
    }

    #[test]
    fn the_map_and_scale_are_the_only_difference_between_display_and_bake() {
        // Display (image→screen map + zoom) and bake (p*scale) must be the SAME geometry at
        // another resolution: scaling the source-space polygons by hand reproduces the target.
        let raw = hand_drawn(60);
        let pts = smooth_path(&raw, 4.0);
        let speed = pressure_along(&raw, &pts);
        let src = stroke_fill_polygons(&pts, 4.0, &speed, ident, 1.0);
        let k = 2.5;
        let scaled = stroke_fill_polygons(&pts, 4.0, &speed, |p| (p.0 * k, p.1 * k), k);
        assert_eq!(src.len(), scaled.len());
        for (a, b) in src.iter().zip(&scaled) {
            assert_eq!(a.len(), b.len(), "the same pieces, at both resolutions");
            for (p, q) in a.iter().zip(b) {
                assert!(
                    (p.0 * k - q.0).abs() < 1e-2 && (p.1 * k - q.1).abs() < 1e-2,
                    "{p:?} scaled by {k} != {q:?}"
                );
            }
        }
    }
}

