//! The colour picker's pure decisions (DRAGON-582): which SOURCE PIXEL the cursor is
//! over, what the magnifier disc looks like, where the hex label goes, how big the
//! result window is, and what may write the recent-colours list.
//!
//! Everything here is a plain function over plain data. No `App`, no iced widget, no
//! platform: the picker's correctness lives in this file and the Linux gate proves all
//! of it on any host. The overlay and the window only feed these numbers in and apply
//! the answers.
//!
//! **Plain data really does mean plain data again** (DRAGON-680). One function here used
//! to MEASURE text: `widest_mode_label` sized the mode dropdown from its longest label
//! through `preview::text_annot`'s embedded faces, with an injectable seam
//! (`mode_chip_width_for`) so the decision could still be tested without it. The dropdown
//! is gone (a bare chevron pair replaced it), and the measurement went with it. If
//! something here ever has to measure text again, that pair is the pattern: measure
//! through the embedded face, and keep the DECISION injectable.

use crate::color::Srgb;

// ── The magnifier ────────────────────────────────────────────────────────────

/// How many SOURCE PIXELS the magnifier disc spans, edge to edge.
///
/// ODD on purpose: an even span has no middle pixel, so there would be no single pixel
/// the ring is centred on and no honest answer to "which one am I sampling". 13 shows
/// enough context to aim at a one-pixel border without the disc swallowing the screen.
pub const MAGNIFIER_SPAN: u32 = 13;

/// How many rendered pixels one source pixel occupies in the disc. The product with
/// [`MAGNIFIER_SPAN`] is both the buffer's edge length and the disc's on-screen
/// diameter in points, so a source pixel reads as a clear 12pt square.
pub const MAGNIFIER_CELL: u32 = 12;

/// The disc's diameter, in rendered pixels AND in logical points (the raster is built
/// at 1:1 with its on-screen size, so the image widget never resamples it).
pub const MAGNIFIER_DIAMETER: u32 = MAGNIFIER_SPAN * MAGNIFIER_CELL;

const _: () = assert!(
    MAGNIFIER_SPAN % 2 == 1,
    "DRAGON-582: an even magnifier span has no centre pixel, so the ring would sit \
     between two pixels and the picker could not say which one it sampled"
);

/// The MOST the magnifier will ever enlarge a source pixel: SIX source pixels edge to edge
/// (DRAGON-601, the owner's ceiling).
///
/// It used to BE [`MAGNIFIER_CELL`], which made the default and the ceiling the same number,
/// so the three zoom routes could only ever travel DOWN. The owner asked for headroom above
/// the shipped magnification, so the ceiling moved and the default did not: a fresh picker
/// still opens at [`MAGNIFIER_ZOOM_DEFAULT`] and looks exactly as it did.
///
/// **Twenty-six, specifically, because that is where the disc stops holding enough context to
/// aim with.** The useful limit at this end is legibility, not arithmetic. The disc is a fixed
/// [`MAGNIFIER_DIAMETER`] on screen, so the magnification decides how much of the screen it
/// holds: `156 / zoom` source pixels edge to edge. That is 52 at the floor, 13 at the default,
/// and 6 here. Six is the last step where the sampled pixel still has two or three neighbours
/// visible on every side, which is what tells you WHICH pixel you want: an antialiased edge, a
/// one-pixel border and the thing it borders are all still on screen together. Past it the
/// lens fills with two or three enormous squares, and it can tell you the colour under the
/// sample but no longer where the sample is, which is the "shows you less than your eyes"
/// complaint the floor guards against, arriving from the other direction.
///
/// It is stated in the same terms as [`MAGNIFIER_ZOOM_MIN`] on purpose, so the two ends of the
/// range can be read against each other rather than as two unrelated numbers.
pub const MAGNIFIER_ZOOM_MAX: u32 = 26;
/// The LEAST the magnifier will enlarge a source pixel: THREE rendered pixels per source
/// pixel, a little short of 1:1 (DRAGON-598, the owner's floor).
///
/// It used to be 1, and the owner's objection is the whole reason it moved: "lets not let
/// the color picker zoom out all the way to 1:1, a little before we get to 1:1 is fine."
/// At 1:1 the loupe is not a loupe. Every source pixel is one rendered pixel, so the disc
/// shows exactly what the eye already sees at exactly the size it already sees it, and the
/// only thing it adds is the marker.
///
/// **Three, specifically, because three is where the marker stops eating its neighbours.**
/// The sampled pixel is marked by a one-pixel OUTLINE drawn INSIDE its own cell, and a cell
/// narrower than three rendered pixels has no inside left. Below three the picker had to
/// mark the sample by painting the eight cells AROUND it instead, hiding eight real colours
/// to point at one, which is the "shows you less than your eyes" complaint one step along
/// rather than a different problem ([`magnifier_rgba`] carries that arm's tombstone). So the
/// floor is not a taste call: it is the smallest magnification at which the lens can still
/// say which pixel it means without covering another one.
///
/// The range stays worth having. At the floor the disc holds 52 source pixels edge to edge
/// (156 / 3) against the default's 13, so zooming out still shows four times the context.
pub const MAGNIFIER_ZOOM_MIN: u32 = 3;
/// What a fresh picker opens at: [`MAGNIFIER_CELL`], the magnification the picker was designed
/// around and has always shipped with.
///
/// It is pinned to the CELL rather than to either end of the range, and that is the point of
/// it being its own const (DRAGON-601). It used to be spelled `= MAGNIFIER_ZOOM_MAX` back when
/// the ceiling was the same number; now that the ceiling is above it, spelling it that way
/// would have silently moved every user's opening view when the ceiling rose.
pub const MAGNIFIER_ZOOM_DEFAULT: u32 = MAGNIFIER_CELL;

const _: () = assert!(
    MAGNIFIER_ZOOM_MIN >= 3 && MAGNIFIER_ZOOM_DEFAULT == MAGNIFIER_CELL,
    "DRAGON-587/598/601: the picker must OPEN at the magnification it was designed around \
     (the cell), or every existing user's first look changes. A floor below 3 leaves the \
     sampled cell too narrow to hold its own one-pixel outline, so the marker would have to \
     paint the eight cells around the sample and hide eight real colours to point at one (and \
     a floor under 1:1 would minify outright, reporting a colour the lens never showed)"
);

const _: () = assert!(
    MAGNIFIER_ZOOM_MIN < MAGNIFIER_ZOOM_DEFAULT && MAGNIFIER_ZOOM_DEFAULT < MAGNIFIER_ZOOM_MAX,
    "DRAGON-598/601: the default must sit STRICTLY between the two ends, or one of the three \
     zoom routes has nowhere to travel. Equal floor and default loses zooming out; equal \
     default and ceiling loses zooming in, which is exactly what DRAGON-601 was asked to fix"
);

/// Pure, unit-tested: a magnification clamped into the range the picker allows.
///
/// Signed input on purpose: every route into the zoom is a STEP away from the current value
/// ([`zoom_after_step`]), and a step down from 1 has to saturate rather than wrap through
/// zero into an enormous `u32`.
pub fn clamp_magnification(zoom: i32) -> u32 {
    zoom.clamp(MAGNIFIER_ZOOM_MIN as i32, MAGNIFIER_ZOOM_MAX as i32) as u32
}

/// Pure, unit-tested: a PERSISTED magnification made safe for the bounds THIS build allows
/// (DRAGON-615).
///
/// The stored value is never trusted, and the reason is concrete rather than defensive. The
/// ceiling has already moved once, 12 to 26 in DRAGON-601, so configs written either side of
/// that change are both in the wild; a future move would put a third shape out there. Applying
/// a stored number blind would hand the magnifier a magnification outside its own range, which
/// nothing downstream re-checks.
///
/// It delegates to [`clamp_magnification`] rather than repeating the range, so the persisted
/// route cannot drift from the three interactive ones. The only thing it adds is the widening:
/// the field is a `u32` (a magnification is never negative) while the shared clamp is signed,
/// and a value above `i32::MAX` saturates to the CEILING rather than wrapping to a negative and
/// landing on the floor, which is the answer a hand-edited "make it enormous" actually meant.
pub fn zoom_from_persisted(stored: u32) -> u32 {
    clamp_magnification(i32::try_from(stored).unwrap_or(i32::MAX))
}

/// Pure, unit-tested: the magnification after `steps` notches from `current`.
///
/// THE one arithmetic every zoom route shares (DRAGON-587): the trackpad, the mouse wheel and
/// the numpad `+` / `-` all reduce to a signed step count and land here, so no route can
/// escape the clamp or drift to a different step size. Positive steps zoom IN.
///
/// **Saturating, not wrapping** (DRAGON-601). The add used to be a plain `+`, which panics in
/// a debug build on a large positive step: a route hands this whatever notch count it
/// accumulated, and the clamp can only defend the RESULT, never the addition that produces it.
/// It went unnoticed while the ceiling equalled the default, because the test that pushed the
/// hardest pushed DOWN (`i32::MIN` is reachable by `3 + i32::MIN` without overflow) and there
/// was no ceiling above the default to push UP against. Raising the ceiling added that test,
/// and the test found this.
pub fn zoom_after_step(current: u32, steps: i32) -> u32 {
    clamp_magnification(clamp_magnification(current as i32).cast_signed().saturating_add(steps))
}

/// macOS: how much cumulative pinch magnification counts as one magnifier zoom notch, for
/// [`pinch_notches`].
///
/// `widgets::color_pick::WHEEL_ZOOM_STEP` is this same widget's equivalent for the
/// wheel/trackpad-SCROLL route: 48 screen pixels of two-finger swipe make one notch. A pinch's
/// magnification is a different unit entirely, an `NSMagnificationGestureRecognizer`'s value is
/// a proportion (1.0 means the content doubled), not a pixel count, so the two cannot share a
/// divisor. What should match is the FEEL, not the number: the magnifier's whole range
/// ([`MAGNIFIER_ZOOM_MIN`]..=[`MAGNIFIER_ZOOM_MAX`], 3..=26) is 23 notches over roughly an 8.7x
/// span, so each notch is already about a 10% relative change. 0.1 of magnification per notch
/// keeps a pinch's per-notch weight in that same ballpark instead of inventing an unrelated
/// scale, so a normal, deliberate pinch moves the lens about as far as a normal, deliberate
/// wheel/scroll gesture does.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PINCH_ZOOM_STEP: f32 = 0.1;

/// macOS, pure, unit-tested: whole zoom NOTCHES from a pinch's magnification `delta`, plus the
/// fractional remainder to carry into the next call.
///
/// Mirrors `widgets::color_pick`'s wheel-scroll accumulator (whole notches published, the
/// remainder kept, so a slow gesture accumulates into one step instead of rounding away), but
/// cannot literally reuse that accumulator: it lives in the widget's own `tree.state`, and a
/// pinch is drained by the app/subscription layer (`App::sub_color_picker_pinch`), never by the
/// widget's own `Event` handler. `accum` is [`crate::app::color_picker::ColorPickerState`]'s
/// small home for the same shape of state instead.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn pinch_notches(accum: f32, delta: f32) -> (i32, f32) {
    let accum = accum + delta / PINCH_ZOOM_STEP;
    let whole = accum.trunc();
    (whole as i32, accum - whole)
}

/// Pure, unit-tested: the SOURCE PIXEL under a cursor position.
///
/// `offset` is the fractional capture offset of the cursor from this output's top-left
/// ([`crate::geometry::OverlayUnits::capture_offset_f`]), `capture` the output's own
/// capture extent, and `image` the frozen snapshot's pixel dimensions. The two can
/// differ by any factor: a HiDPI Linux output reports logical units while its snapshot
/// is physical pixels, and Windows reports physical for both.
///
/// **The furthest edge is reachable**, which is the requirement that shapes the whole
/// function. The result is clamped to `0..=size-1` per axis, so a cursor at (or past)
/// the very last point of the surface answers the last pixel rather than falling off the
/// end, and a cursor at zero answers the first. Working from the FRACTIONAL offset is
/// what makes the last pixel of a scaled output reachable at all: a truncated capture
/// coordinate cannot address anything finer than one capture unit.
///
/// `None` only for a degenerate output or snapshot (a zero extent), where there is no
/// pixel to name. A picker that cannot name the pixel must say so, never guess.
pub fn source_pixel(
    offset: (f32, f32),
    capture: (i32, i32),
    image: (u32, u32),
) -> Option<(u32, u32)> {
    if capture.0 <= 0 || capture.1 <= 0 || image.0 == 0 || image.1 == 0 {
        return None;
    }
    let map = |o: f32, cap: i32, px: u32| -> u32 {
        // MULTIPLY, then divide (DRAGON-587). The other order, `(o / cap) * px`, computes a
        // ratio that is almost never a dyadic rational and then scales it back up, so the
        // round trip lands a hair BELOW a whole number and `floor` drops a pixel. On an
        // unscaled 1920-wide output that mis-read 22 of the 1920 whole-point positions, each by
        // one column to the left, which is a colour picker reporting the wrong pixel. This form
        // is exact whenever the true quotient is representable, and never worse.
        let scaled = (o as f64 * px as f64) / cap as f64;
        if !scaled.is_finite() || scaled < 0.0 {
            return 0;
        }
        (scaled.floor() as u32).min(px - 1)
    };
    Some((
        map(offset.0, capture.0, image.0),
        map(offset.1, capture.1, image.1),
    ))
}

// DRAGON-597 deleted `SAMPLE_OFFSET` and `sample_point`, and the tombstone is worth keeping
// because the rule looked like a general precision measure and was not.
//
// The picker used to read the pixel one surface POINT up and left of the pointer instead of the
// pixel under it. That existed for exactly ONE reason: on a Wayland layer surface the pointer
// sprite could not be hidden (libcosmic left `set_cursor_visible` an unimplemented TODO), so the
// surface asked for the default ARROW, whose hotspot is its TIP and whose body falls down and
// right of it. The tip covers its own hotspot pixel, so the first pixel the user could actually
// SEE was the one diagonally up and left, and reading that one is what let them judge a colour
// before taking it. Points rather than pixels because what was being escaped was a sprite, and a
// sprite is measured on screen.
//
// The shift also had to SHORTEN near the far walls, `min(1 point, extent - pointer)`, because a
// pointer can never leave the surface: a fixed shift made the last column and the last row
// unreachable (the owner reported that three times) and a shift that merely switched off at the
// wall only moved the hole along by one. The ramp kept the pointer-to-pixel map continuous and
// strictly increasing, so every pixel stayed landable.
//
// All of that is moot now. Our iced fork implements `set_cursor_visible` for layer surfaces
// (the iced `[patch]` block in `Cargo.toml`), so there is no sprite to escape on any surface,
// and
// the sample is the pointer's own point again. The edge guarantee did not depend on the shift:
// `source_pixel` clamps per axis from the FRACTIONAL offset, so a pointer a hair inside the far
// wall still answers the last pixel, and `edge_pixel_tests` pins that directly.
//
// If the fork ever has to be dropped, this comes back WITH the arrow, never on its own: an arrow
// with an unshifted sample is precisely the bug that was reported.

/// What just asked the sample to move (DRAGON-599). Two things can, and they compose
/// differently, which is the whole reason this is a named type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleMove {
    /// A real pointer MOTION event. The sample belongs to the pointer again.
    Pointer,
    /// A directional key, as a step of `(dx, dy)` SOURCE PIXELS.
    Keys(i32, i32),
}

/// **Pure**, unit-tested: the keyboard nudge offset, in SOURCE PIXELS, after `mv`
/// (DRAGON-599).
///
/// **A Wayland client cannot warp the pointer**, and that fact shapes this whole feature.
/// There is no call that moves the physical cursor, so an arrow key cannot make the pointer
/// follow it. What moves instead is the SAMPLE, held as a displacement from wherever the
/// pointer last really was, and this is the arithmetic of that displacement.
///
/// Keys ACCUMULATE and a pointer motion RESETS. The reset is the load-bearing half, and it is
/// stated here rather than left implicit at a call site because getting it wrong is the bug
/// this feature is most likely to ship with: an offset that survived pointer motion would add
/// to itself for the rest of the session, and the loupe would drift permanently away from the
/// cursor with no way back short of relaunching the picker. Resetting means the two are
/// re-married by the smallest real mouse movement, which is exactly the escape hatch a user
/// reaches for when the sample has wandered somewhere they did not intend.
///
/// Saturating, so a key held against a wall for a very long time cannot wrap the offset; the
/// clamp in [`nudged_sample`] is what actually keeps the sample on screen.
pub fn nudge_after(current: (i32, i32), mv: SampleMove) -> (i32, i32) {
    match mv {
        SampleMove::Pointer => (0, 0),
        SampleMove::Keys(dx, dy) => (current.0.saturating_add(dx), current.1.saturating_add(dy)),
    }
}

// ── Pacing the magnifier's raster against a moving pointer (DRAGON-TBD) ──────

/// Pointer speed, in surface POINTS per second, at or below which the magnifier's raster is
/// rebuilt on EVERY frame: the pointer is being aimed, so every picture is worth having.
///
/// At 120Hz this is 5 points per frame, and at the default magnification a point is about a
/// source pixel, so consecutive discs still share 8 of the 13 pixels they span
/// ([`MAGNIFIER_SPAN`]). The lens therefore reads as continuous motion, which is the whole
/// reason to keep rastering. It is comfortably above ordinary targeting; only a deliberate
/// flick clears it.
pub const DELIBERATE_SPEED: f32 = 600.0;

/// Pointer speed, in surface POINTS per second, at or above which the raster is paced as
/// slowly as it ever gets ([`RASTER_MAX_INTERVAL`]).
///
/// At 120Hz this is 50 points per frame, roughly FOUR TIMES the disc's own
/// [`MAGNIFIER_SPAN`], so two consecutive rasters would share not one source pixel: the
/// pictures in between are not a motion the eye can follow, they are unrelated stills. The
/// owner's own case, a flick across a 3456-point display in about 200ms, is ~17000, well past
/// this.
pub const FLICK_SPEED: f32 = 6000.0;

/// The longest the magnifier's CONTENT may go unrefreshed while the pointer is sweeping.
///
/// 40ms is 25 refreshes a second, which is deliberately not a freeze: the disc keeps showing
/// where it is, just less often, which is what the owner asked for ("fast movements should be
/// careful", not "fast movements show nothing"). It cuts the raster count during a flick by
/// about 80% at 120Hz.
pub const RASTER_MAX_INTERVAL: std::time::Duration = std::time::Duration::from_millis(40);

const _: () = assert!(
    DELIBERATE_SPEED < FLICK_SPEED,
    "the pacing ramp needs a below-which and an above-which, in that order, or \
     raster_min_interval divides by a non-positive width"
);

/// **Pure**, unit-tested: the pointer's speed in surface POINTS per second, from the two most
/// recent sample points and the time between them (DRAGON-TBD).
///
/// `dt` that is zero or negative answers `0.0` rather than infinity, and the direction of that
/// choice is the point: an unknown speed must fall toward ACCURACY (raster now), never toward
/// the throttle. Two samples in the same instant is not a fast pointer, it is a clock that
/// could not separate them.
pub fn sample_speed(prev: (f32, f32), now: (f32, f32), dt: std::time::Duration) -> f32 {
    let secs = dt.as_secs_f32();
    if secs <= 0.0 {
        return 0.0;
    }
    let (dx, dy) = (now.0 - prev.0, now.1 - prev.1);
    (dx * dx + dy * dy).sqrt() / secs
}

/// **Pure**, unit-tested: the shortest time that may separate two magnifier rasters at this
/// pointer speed (DRAGON-TBD). `Duration::ZERO` means "every frame".
///
/// # Why pace this at all
/// The raster is not expensive: measured at **57.5µs** for the whole 156x156 disc, flat across
/// the zoom range, which at 120Hz is 0.69% of one core plus a 97KB texture upload per frame.
/// So this is not rescuing a frame budget, and it must not behave as though it were. What it
/// removes is work whose RESULT nobody can see: during a flick across the screen the sample
/// lands on an unrelated source pixel every frame, and the user is not reading the lens, they
/// are travelling. The measurement is quoted so the next person can tell how little is at
/// stake before they make this more aggressive.
///
/// # The shape, and why it is a ramp rather than a switch
/// A hard on/off gate at one speed flickers for anyone moving near it: the content would
/// freeze and unfreeze as the hand wavers across the threshold. A ramp degrades instead, so
/// the same wobble only changes how often the picture refreshes, by a little.
///
/// Below [`DELIBERATE_SPEED`] the answer is ZERO, so the aiming case is byte-identical to
/// having no pacing at all. Above [`FLICK_SPEED`] it saturates at [`RASTER_MAX_INTERVAL`].
/// In between it is linear.
///
/// **A stopped pointer answers ZERO**, which is what makes the settle exact rather than
/// approximate: the frame after the hand stops is already below `DELIBERATE_SPEED` (it moved
/// nothing), so the lens catches up on that frame with no timer, no decay and nothing to
/// tune. See `App::color_picker_resample_with`, which re-arms itself for exactly one more
/// look whenever it declines to raster, so the settle cannot be missed.
pub fn raster_min_interval(speed: f32) -> std::time::Duration {
    // NaN is spelled out rather than left to fall through a negated comparison: it can only
    // mean the speed is unknown, and an unknown speed rasters, like every other unknown here.
    if speed.is_nan() || speed <= DELIBERATE_SPEED {
        return std::time::Duration::ZERO;
    }
    if speed >= FLICK_SPEED {
        return RASTER_MAX_INTERVAL;
    }
    let t = (speed - DELIBERATE_SPEED) / (FLICK_SPEED - DELIBERATE_SPEED);
    RASTER_MAX_INTERVAL.mul_f32(t)
}

/// **Pure**, unit-tested: may the magnifier's raster be rebuilt now, given how long it has
/// been since the last one and how fast the pointer is moving (DRAGON-TBD)?
///
/// `since_last` is `None` when nothing has been rastered yet, which is always due: a picker
/// with no lens at all must never be made to wait for one.
pub fn raster_due(since_last: Option<std::time::Duration>, speed: f32) -> bool {
    match since_last {
        None => true,
        Some(elapsed) => elapsed >= raster_min_interval(speed),
    }
}

/// **Pure**, unit-tested: how far one directional key moves the sample, in surface POINTS, so
/// that a press is exactly ONE SOURCE PIXEL on any display (DRAGON-599).
///
/// The owner asked for one pixel per tap. A fixed one-POINT step would be one pixel on an
/// unscaled display and TWO on a HiDPI one, where every other pixel would then be unreachable
/// from the keyboard, which is not a colour picker. So the step is the surface's own
/// points-per-source-pixel, `viewport / image`, on each axis independently (a display does not
/// have to be square, and neither does its snapshot).
///
/// This is exactly the inverse of what [`source_pixel`] does, which is why one step lands on
/// the next pixel and not a hair short of it: that map is `floor(offset * image / capture)`,
/// and the point-to-capture factor cancels, so adding `viewport / image` points adds exactly
/// `1` before the floor.
///
/// One point per axis is the fallback for a degenerate surface or snapshot, where there is no
/// ratio to compute and refusing to move at all would be worse than moving by something.
pub fn nudge_step(viewport: (f32, f32), image: (u32, u32)) -> (f32, f32) {
    let axis = |extent: f32, px: u32| -> f32 {
        if !extent.is_finite() || extent <= 0.0 || px == 0 {
            return 1.0;
        }
        extent / px as f32
    };
    (axis(viewport.0, image.0), axis(viewport.1, image.1))
}

/// **Pure**, unit-tested: the surface POINT the picker reads, once the keyboard has moved the
/// sample `nudge` source pixels away from the pointer's own answer `base` (DRAGON-599).
///
/// `base` is the pointer's own point (DRAGON-597 removed the offset seam that used to sit
/// between them, see the tombstone above), so
/// this composes on top of the pointer rule rather than replacing it: with `nudge` at
/// `(0, 0)` it returns `base` untouched, which is every pick that never touched a key.
///
/// The clamp is to `0 ..= extent`, which is the SAME interval the pointer path already
/// produces at its two ends, so the existing reachability guarantee carries over rather than
/// being restated: `0` maps to pixel `0` and `extent` maps to the last pixel (see
/// [`source_pixel`]'s `min(px - 1)`), and everything in between is reachable because the step
/// is one pixel wide. That is why this clamps instead of refusing at a wall the way the region
/// nudge does: a sample is a POINT, so stopping it at the boundary still leaves it on a real
/// pixel, where stopping a rectangle would have to resize it.
pub fn nudged_sample(
    base: (f32, f32),
    nudge: (i32, i32),
    viewport: (f32, f32),
    image: (u32, u32),
) -> (f32, f32) {
    if nudge == (0, 0) {
        return base;
    }
    let step = nudge_step(viewport, image);
    let axis = |b: f32, n: i32, s: f32, extent: f32| -> f32 {
        if !b.is_finite() || !extent.is_finite() || extent <= 0.0 {
            return 0.0;
        }
        (b + n as f32 * s).clamp(0.0, extent)
    };
    (
        axis(base.0, nudge.0, step.0, viewport.0),
        axis(base.1, nudge.1, step.1, viewport.1),
    )
}

/// Pure, unit-tested: the magnifier disc as a straight RGBA buffer, `(width, height, pixels)`.
///
/// Nearest-neighbour by construction: each source pixel becomes a `zoom`-sized square, so the
/// user sees the pixel GRID and can tell exactly which square is being read. Four things are
/// baked in, and each earns its place:
///
/// * A **circular alpha mask**. The corners outside the circle are fully transparent, so
///   the dimmed desktop shows through and the disc reads as a lens rather than a card.
/// * **Transparent out-of-bounds**, near the edge of the SOURCE image. Repeating the edge
///   pixel would draw colours that are not there, which is the one thing a colour picker may
///   never do; showing the dim instead says honestly that the world stops here. This is the
///   answer to "what is inside the lens when part of the magnified region is off the screen"
///   (DRAGON-587): the dimmed backdrop, so the boundary is unmistakable, never a clamped or
///   wrapped pixel that would read as real screen content.
/// * A **one-pixel outline on the sampled cell**, in black or white by that pixel's own
///   luminance, so the sampled pixel is identifiable whatever colour it is.
/// * The **accent RING** around the rim, `ring.0` points thick in `ring.1`.
///
/// The ring used to be a bordered container stacked OVER this image, so a theme change could
/// repaint it without rebuilding the raster. DRAGON-587 moved it in here, and the reason is
/// worth keeping: the disc is now CLIPPED at a screen edge ([`DiscView`]), and a clipped
/// image with an unclipped ring widget over it would draw a full circle around a half disc.
/// A widget cannot be cropped the way a buffer can. Nothing is lost in practice: the picker
/// is a one-shot process whose theme cannot change while it is up, and the raster is rebuilt
/// on every pointer move to a new pixel anyway.
///
/// `zoom` is the magnification in rendered pixels per source pixel (DRAGON-587), clamped to
/// [`MAGNIFIER_ZOOM_MIN`]..=[`MAGNIFIER_ZOOM_MAX`]. The disc's on-screen SIZE never changes;
/// what changes is how much of the screen it holds, from 13 source pixels at the default out
/// to 52 at the floor. The cell a rendered pixel belongs to is derived from its distance to
/// the disc's centre rather than from an integer division of its coordinate, which is what
/// keeps the SAMPLED pixel exactly centred at every zoom (an integer grid only centres when
/// the cell count is odd). At the default the two are identical arithmetic, so the shipped
/// picture is what it always was.
///
/// `view` is which PART of the disc to build, so a disc hanging off a screen edge is produced
/// already clipped rather than clamped or rescaled by the layout. [`DiscView::FULL`] is the
/// whole thing.
pub fn magnifier_rgba(
    src: &image::RgbaImage,
    center: (u32, u32),
    zoom: u32,
    ring: (f32, [u8; 4]),
    view: DiscView,
) -> (u32, u32, Vec<u8>) {
    let d = MAGNIFIER_DIAMETER;
    let cell = clamp_magnification(zoom as i32) as f32;
    let (out_w, out_h) = (view.size.0.min(d), view.size.1.min(d));
    let mut out = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
    let radius = d as f32 / 2.0;
    // The centre pixel's own square, in rendered pixels, is `cell` wide at every zoom.
    let (cx, cy) = (radius, radius);
    let (ring_w, ring_ink) = (ring.0.max(0.0), ring.1);
    // The centre pixel decides the outline's ink, so a dark pixel gets a light box and a
    // light pixel a dark one.
    let centre_ink: [u8; 4] = {
        let p = src.get_pixel(center.0.min(src.width() - 1), center.1.min(src.height() - 1)).0;
        if Srgb::new(p[0], p[1], p[2]).wants_dark_text() {
            [0, 0, 0, 255]
        } else {
            [255, 255, 255, 255]
        }
    };
    for oy in 0..out_h {
        for ox in 0..out_w {
            // Where this output pixel sits in the WHOLE disc. Cropping shifts the window,
            // never the circle: the maths below is unchanged by which part is being built.
            let (x, y) = (ox + view.crop.0, oy + view.crop.1);
            let idx = ((oy as usize) * (out_w as usize) + ox as usize) * 4;
            // Inside the circle? Measured from the pixel's own centre, so the rim is
            // even rather than biased a half pixel up and left.
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > radius {
                continue;
            }
            // On the rim: the accent ring, drawn whether or not there is a source pixel
            // under it, because it is the LENS's own edge rather than content.
            if ring_w > 0.0 && dist >= radius - ring_w {
                out[idx..idx + 4].copy_from_slice(&ring_ink);
                continue;
            }
            // Which source pixel this rendered pixel shows: the centre cell straddles the
            // disc's centre, so cell 0 spans [-cell/2, +cell/2) from it.
            let cell_x = ((dx + cell / 2.0) / cell).floor();
            let cell_y = ((dy + cell / 2.0) / cell).floor();
            let sx = center.0 as i64 + cell_x as i64;
            let sy = center.1 as i64 + cell_y as i64;
            // Off the edge of the world: leave it transparent (see the doc).
            if sx < 0 || sy < 0 || sx >= src.width() as i64 || sy >= src.height() as i64 {
                continue;
            }
            let px = src.get_pixel(sx as u32, sy as u32).0;
            // Position INSIDE the centre cell, so the outline is its own one-pixel border at
            // every magnification the picker allows. [`MAGNIFIER_ZOOM_MIN`] is set at exactly
            // the cell width where that still fits (3), which is why there is only one arm.
            //
            // DRAGON-598 removed the second one, and its reason is worth keeping. Below three
            // rendered pixels a cell has no inside left, so the marker had to move OUTSIDE it,
            // onto the eight cells around the sample: the picker hid eight real colours to
            // point at one. That is the same "shows you less than your eyes" failure that took
            // the floor off 1:1, so the fix was to raise the floor rather than to keep drawing
            // the compromise. If the floor is ever lowered again, that arm has to come back.
            let (lx, ly) = (dx + cell / 2.0 - cell_x * cell, dy + cell / 2.0 - cell_y * cell);
            let on_centre_outline = cell_x == 0.0
                && cell_y == 0.0
                && (lx < 1.0 || ly < 1.0 || lx >= cell - 1.0 || ly >= cell - 1.0);
            let rgba = if on_centre_outline {
                centre_ink
            } else {
                [px[0], px[1], px[2], 255]
            };
            out[idx..idx + 4].copy_from_slice(&rgba);
        }
    }
    (out_w, out_h, out)
}

// ── Where the disc actually lands ────────────────────────────────────────────

/// How much of the magnifier disc is on screen, and where that part goes (DRAGON-587).
///
/// **The disc is ALWAYS centred on the SAMPLE POINT, always exactly [`MAGNIFIER_DIAMETER`]
/// across, and always round.** The sample point is the pointer's own point (it was displaced by
/// one point under the old arrow fallback; see this file's DRAGON-597 tombstone), so the lens
/// sits ON the pointer rather than parked away from it. Near a screen edge it is
/// CLIPPED by the boundary and nothing else: not pushed back on screen, not shrunk, not
/// squashed, and never moved to the pointer's other side. The pointer is already stopped at the
/// edge by the compositor, so a second constraint on the lens buys nothing and costs the
/// truth: a disc that stops following the pointer lies about where the sample is, and a
/// squashed one distorts the very pixels the user is judging.
///
/// Both of those were happening. The overlay places the disc by padding a fill container
/// ([`super::view`]'s `absolute`), and a padding cannot be negative, so past the LEFT or TOP
/// edge the disc was clamped back on screen. Past the RIGHT or BOTTOM edge the padded
/// container leaves the image less room than it asked for, and an `Image` resolves its layout
/// against those limits and then CONTAIN-fits its content, so the disc was scaled down: the
/// owner's "stops or squishes".
///
/// The fix is to hand the view a disc that already IS the visible part: the raster is built
/// cropped ([`magnifier_rgba`] takes this), and the image is placed at a non-negative origin
/// at exactly the size it will occupy, so nothing clamps and nothing rescales.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DiscView {
    /// Where the visible part goes, in surface POINTS. Never negative, and never so far
    /// right or down that the image would not fit.
    pub origin: (i32, i32),
    /// How many points of the disc are cut off its LEFT and TOP by the surface edge.
    pub crop: (u32, u32),
    /// The visible size, in points. Equal to the full diameter away from any edge.
    pub size: (u32, u32),
}

impl DiscView {
    /// The whole disc, uncropped, at the origin. What the raster's own tests build against,
    /// and the shape every pointer position away from an edge produces.
    ///
    /// Dead outside the tests, and kept anyway: it is the identity value [`magnifier_rgba`]'s
    /// doc names, and a raster call with no view argument to point at would be harder to read
    /// than one that says FULL.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const FULL: DiscView = DiscView {
        origin: (0, 0),
        crop: (0, 0),
        size: (MAGNIFIER_DIAMETER, MAGNIFIER_DIAMETER),
    };
}

// DRAGON-587 deleted `DiscAnchor` and its four-quadrant preference ladder, and the reason is
// worth keeping so it is not reinvented. The ladder placed the whole disc a DIAMETER up and
// left of the pointer and demanded the entire circle fit on screen, falling through to another
// quadrant when it did not. Two things were wrong with it. The offset was measured to the
// bounding BOX, so the circle's nearest point was another `r * (sqrt(2) - 1)` further out and
// the pointer floated in empty space well away from the lens. And the fallthrough made the lens
// JUMP to the other side of the pointer at the left and top walls, when what the owner asked
// for, twice, was for the circle to be CUT OFF by the edge so the pointer can walk right up to
// the last pixel. The disc is now centred on the sample point, which since DRAGON-597 is simply
// the pointer's own point.

/// Pure, unit-tested: the part of the disc that is on screen for this sample position.
///
/// `None` when nothing of it is (a centre far enough outside the surface), where the overlay
/// draws no magnifier rather than an empty one.
///
/// Points are rounded to whole units because the crop is a rectangle of BUFFER pixels, and
/// the raster is built 1:1 with its on-screen size. The disc's centre keeps its fractional
/// position: only the boundary between "cut off" and "drawn" is rounded, which is at most a
/// half-point of the outermost rim.
pub fn disc_view(centre: (f32, f32), viewport: (f32, f32)) -> Option<DiscView> {
    let (left, top, _, _) = disc_rect(centre, MAGNIFIER_DIAMETER as f32 / 2.0);
    let cut = |ideal: f32, extent: f32| -> Option<(i32, u32, u32)> {
        let crop = (-ideal).max(0.0).round() as u32;
        if crop >= MAGNIFIER_DIAMETER {
            return None; // entirely off the leading edge
        }
        let origin = ideal.max(0.0).round() as i32;
        // What is left of the surface from there, and what is left of the disc.
        let room = (extent - origin as f32).max(0.0).floor() as u32;
        let size = (MAGNIFIER_DIAMETER - crop).min(room);
        (size > 0).then_some((origin, crop, size))
    };
    let (x, crop_x, w) = cut(left, viewport.0)?;
    let (y, crop_y, h) = cut(top, viewport.1)?;
    Some(DiscView { origin: (x, y), crop: (crop_x, crop_y), size: (w, h) })
}

/// **Pure**, unit-tested: where the view PLACES a rastered magnifier buffer whose identity
/// is `raster`, now that the sample point is `sample` (DRAGON-650).
///
/// The buffer's PLACEMENT and its CONTENTS are two different questions, and welding them
/// together was the reported defect. The pacing (see [`raster_min_interval`]) may leave the
/// CONTENTS up to [`RASTER_MAX_INTERVAL`] stale during a sweep, which is its whole point —
/// but the raster's own `origin` was also where the view drew the disc, so on every paced
/// frame the lens simply did not move, then jumped the accumulated distance when the next
/// raster landed. On a 60Hz panel that is up to 40ms of travel in one step, read as "the
/// lens skips around erratically", while the hex chip (placed from the live sample every
/// frame) glided on ahead of it. The lens must FOLLOW the sample on every frame; only its
/// picture may lag.
///
/// So: place the buffer where the disc centred on TODAY's sample would want the disc
/// pixels it actually holds. [`disc_view`] answers where disc pixel `crop` of a disc
/// centred on `sample` lands; the buffer's first pixel is disc pixel `raster.crop`, so it
/// goes at `origin + (raster.crop - crop)`, integer arithmetic on `disc_view`'s own
/// rounding. **With a fresh raster this is `raster.origin` exactly** (the current view IS
/// the raster's identity, so the correction is zero), which is what keeps every unpaced
/// path — and the settled lens — byte-identical to before this function existed.
///
/// The clamp keeps the buffer fully on the surface, and it is not decoration: the view
/// places absolutely by PADDING a fill container, and a padding cannot be negative, while a
/// buffer given less room than it asks for is contain-fitted — the exact clamp-and-squash
/// pair [`disc_view`] exists to prevent. A stale FULL buffer swept against a wall therefore
/// parks flush with the wall instead of squashing, at most half a diameter from the sample,
/// for at most one pacing interval: the next raster is built clipped for that position and
/// the correction term vanishes.
///
/// `None` from [`disc_view`] (no part of the disc on screen, which a pointer over its own
/// surface cannot produce) answers the raster's own origin: the one placement known to have
/// been valid, rather than a guess.
pub fn drawn_disc_origin(
    sample: (f32, f32),
    raster: DiscView,
    viewport: (f32, f32),
) -> (i32, i32) {
    let Some(current) = disc_view(sample, viewport) else {
        return raster.origin;
    };
    let axis = |origin: i32, crop_now: u32, crop_buf: u32, size_buf: u32, extent: f32| -> i32 {
        let max = ((extent - size_buf as f32).floor() as i32).max(0);
        (origin + crop_buf as i32 - crop_now as i32).clamp(0, max)
    };
    (
        axis(current.origin.0, current.crop.0, raster.crop.0, raster.size.0, viewport.0),
        axis(current.origin.1, current.crop.1, raster.crop.1, raster.size.1, viewport.1),
    )
}

/// The disc's TRUE box as `(left, top, right, bottom)`: full size, centred on `centre`, and
/// free to run off the surface.
///
/// This is what the hex label is placed against, and what [`disc_view`] clips for drawing. It
/// is deliberately the UNCLIPPED box: the label must clear the whole circle, including the part
/// hanging off the screen, or it would sit on the lens the moment the pointer moved back
/// inland.
///
/// ONE function for both modes (DRAGON-587), and it no longer branches at all: the disc is
/// centred on the SAMPLE POINT whether or not that point is offset from the pointer, so there
/// is one box and one rule. It stays a named function rather than being inlined because the
/// label placement, the clip and their tests must all read the same box.
fn disc_rect(centre: (f32, f32), disc_radius: f32) -> (f32, f32, f32, f32) {
    (
        centre.0 - disc_radius,
        centre.1 - disc_radius,
        centre.0 + disc_radius,
        centre.1 + disc_radius,
    )
}

// ── The hex label's placement ────────────────────────────────────────────────

/// Where the hex label sits relative to the magnifier disc.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LabelPlacement {
    /// Under the disc, horizontally centred on the cursor. The default.
    Below,
    /// Over the disc, horizontally centred on the cursor.
    Above,
    /// To the right of the disc, vertically centred on the cursor.
    Right,
    /// To the left of the disc, vertically centred on the cursor.
    Left,
}

/// Pure, unit-tested: which side the hex label goes on, given where the disc is.
///
/// `centre` is the SAMPLE POINT, which is what the disc is centred on ([`disc_rect`]); in the
/// arrow mode that is one point up and left of the pointer, which moves the label by the same
/// point and nothing more.
///
/// **This ladder stays, and it still flips**, unlike the disc's (DRAGON-587). The two are not
/// the same problem: a circle that runs off the edge is still a usable lens, and clipping it is
/// what lets the pointer reach the last pixel, but a hex string with its right half off the
/// screen cannot be read at all. So the label moves and the circle does not.
///
/// Every placement clears the magnifier's own box in every position, which is the DRAGON-587
/// correction: the sides are measured from [`disc_rect`], so the ladder's answer and the
/// drawn disc cannot disagree near a screen edge. That box is the TRUE, unclipped one, so a
/// label beside a half-clipped circle still sits clear of where the whole circle would be.
///
/// A strict preference LADDER, and the order is the ticket's: Below, then Above, then
/// Right, then Left. The first placement whose whole box fits inside the surface wins.
/// A ladder rather than a "pick the side with the most room" score because the label
/// must not shuffle around while the pointer moves through the middle of the screen:
/// with a ladder it stays Below everywhere except near an edge, which is the only place
/// it has any reason to move.
///
/// When no rung FITS outright (a screen corner, where the disc plus the label overruns two
/// walls at once) the ladder is walked a second time asking a weaker question: which rung
/// CLEARS THE DISC once [`label_origin`] has clamped it into view. That second pass is
/// DRAGON-587's other half. Without it a corner fell straight through to `Below`, whose clamp
/// slides the box back up over the circle, which is the overlap the first pass was fixed to
/// prevent.
///
/// When even that finds nothing (a surface barely larger than the disc, which a sane display
/// never is) the answer is [`LabelPlacement::Below`] and the origin is clamped into view.
/// Answering "nowhere" would mean not showing the user the colour they are pointing at.
pub fn label_placement(
    centre: (f32, f32),
    label: (f32, f32),
    disc_radius: f32,
    gap: f32,
    viewport: (f32, f32),
) -> LabelPlacement {
    const LADDER: [LabelPlacement; 4] = [
        LabelPlacement::Below,
        LabelPlacement::Above,
        LabelPlacement::Right,
        LabelPlacement::Left,
    ];
    for placement in LADDER {
        let (x, y) = raw_origin(placement, centre, label, disc_radius, gap);
        if x >= 0.0 && y >= 0.0 && x + label.0 <= viewport.0 && y + label.1 <= viewport.1 {
            return placement;
        }
    }
    for placement in LADDER {
        let origin = label_origin(placement, centre, label, disc_radius, gap, viewport);
        if !overlaps_disc(origin, label, centre, disc_radius) {
            return placement;
        }
    }
    LabelPlacement::Below
}

/// Whether a label box at `origin` intersects the disc's own box. The predicate the second
/// pass of [`label_placement`] and its tests both read.
fn overlaps_disc(
    origin: (f32, f32),
    label: (f32, f32),
    centre: (f32, f32),
    disc_radius: f32,
) -> bool {
    let (dl, dt, dr, db) = disc_rect(centre, disc_radius);
    origin.0 < dr && origin.0 + label.0 > dl && origin.1 < db && origin.1 + label.1 > dt
}

/// The label box's top-left for a placement, BEFORE any clamping.
///
/// Every side is measured from the DISC'S OWN BOX ([`disc_rect`]), never from the pointer plus
/// a radius (DRAGON-587). The two now differ by the one point the sample is offset by, which
/// is small, but reading the box is still the right way round: it is the box the user sees, and
/// it is the box the overlap test uses, so the two cannot disagree.
fn raw_origin(
    placement: LabelPlacement,
    centre: (f32, f32),
    label: (f32, f32),
    disc_radius: f32,
    gap: f32,
) -> (f32, f32) {
    let (left, top, right, bottom) = disc_rect(centre, disc_radius);
    let (mid_x, mid_y) = ((left + right) / 2.0, (top + bottom) / 2.0);
    match placement {
        LabelPlacement::Below => (mid_x - label.0 / 2.0, bottom + gap),
        LabelPlacement::Above => (mid_x - label.0 / 2.0, top - gap - label.1),
        LabelPlacement::Right => (right + gap, mid_y - label.1 / 2.0),
        LabelPlacement::Left => (left - gap - label.0, mid_y - label.1 / 2.0),
    }
}

/// Pure, unit-tested: the label box's top-left in surface POINTS, for the placement
/// [`label_placement`] chose, CLAMPED so the box is always fully on screen.
///
/// The clamp only ever bites on the cross axis (a Below label near the left edge slides
/// right), because the ladder already rejected any placement whose main axis ran off.
/// It is applied unconditionally anyway: the fallback branch of `label_placement` can
/// return a placement that does not fit, and a label half off the screen is worse than a
/// label a few points from where the geometry wanted it.
pub fn label_origin(
    placement: LabelPlacement,
    centre: (f32, f32),
    label: (f32, f32),
    disc_radius: f32,
    gap: f32,
    viewport: (f32, f32),
) -> (f32, f32) {
    let (x, y) = raw_origin(placement, centre, label, disc_radius, gap);
    (
        x.clamp(0.0, (viewport.0 - label.0).max(0.0)),
        y.clamp(0.0, (viewport.1 - label.1).max(0.0)),
    )
}

// ── The result window's size ─────────────────────────────────────────────────
//
// DRAGON-630 rebuilt the window around the reference layout the owner supplied (the
// WinUI-style picker): a saturation/value square on top, then a controls row (pipette,
// round current-colour swatch, stacked hue and alpha strips), then ONE value row of
// per-component boxes with a mode stepper, then the recents strip. The seven stacked
// notation rows are gone; the stepper cycles the same seven notations through the one
// row instead, and nothing scrolls: a scrollbar would be a way of not answering the
// sizing question.
//
// The tombstone worth keeping from the pre-630 layout: the width used to be derived
// from the widest of the seven value rows versus the recents row (each spelled out in
// its own constant), and the owner's original brief was "half the mac permission
// window's width, best judgement". The judgement now sizes the SQUARE: a gradient field
// much under 400pt is too coarse to aim in, and everything else fits comfortably inside
// that width.

/// Padding inside the window, per side.
pub const WINDOW_PADDING: f32 = 16.0;
/// The window's own frame, per side: the frosted outer container is padded by 1 so its
/// border draws around everything (`color_picker_window_view`).
///
/// It is COUNTED, and the reason is a bug it caused: the size arithmetic used to be
/// `padding + content`, which is 2pt more content width than the window actually has, and
/// every section here is laid out at exactly [`CONTENT_W`]. While the value row still had
/// a flexible spacer in it that shortfall was invisible, absorbed by the spacer; the
/// moment every part of the row became fixed-width, the 2pt landed on the LAST item and
/// clipped the copy button's right edge. The history grid and the gradient square were
/// losing the same 2pt all along, just less visibly.
pub const WINDOW_BORDER: f32 = 1.0;
/// The content width every section shares: the SV square's width, and the budget the
/// value row, the controls row and the recents grid are laid out inside.
///
/// 388, and the number is CHOSEN rather than arrived at, by two constraints at once:
///
/// * the VALUE ROW's boxes have to stay usable in the widest mode. The row now spends a
///   48pt copy button and two gaps and a 24pt mode activator before the boxes see a point
///   ([`value_boxes_total`]), and CMYK shows five boxes; 388 is what leaves those five at
///   exactly the 55pt they had before the copy button arrived, so nothing about typing a
///   value changed;
/// * the HISTORY grid has to land on whole points: `388 - 9 * 28` swatch points splits
///   into eight gaps of exactly 17 ([`recents_gap`]), which keeps the flush right edge the
///   owner asked of the history. 380 and 384 both fail that; 388 is the first width at or
///   above the value row's floor that passes it.
///
/// **It has moved twice for the same reason, in both directions.** It was 478 when the
/// value row carried the boxes AND the mode chip AND both icon buttons side by side, came
/// down to 332 as each of those left the row, and is back up to 388 now that DRAGON-680's
/// item 23 put the copy button back on it as a 48pt leader. What that width buys elsewhere
/// is real: the hue and alpha tracks are 268pt of travel ([`STRIPS_W`], against a 150 floor
/// and the 212 they had before the copy button ever joined the controls row).
///
/// The history's gaps grew with it, 10pt to 17pt, because the row still holds NINE
/// swatches. Eleven per row would gap at exactly 8 and raise the cap to 22; it is a real
/// option and deliberately not taken here, since the cap is the owner's call and this
/// ticket was not about the history's size.
pub const CONTENT_W: f32 = 388.0;
/// The saturation/value square's height.
pub const SV_H: f32 = 180.0;
/// The gap under the square, before the controls row, and the gap under the controls
/// row, before the value row. Both LARGER than the ordinary section gap, by the owner's
/// review: the square and the controls read as one instrument without air between them.
pub const GAP_SQUARE_CONTROLS: f32 = 20.0;
pub const GAP_CONTROLS_VALUE: f32 = 20.0;
/// One slider strip's THICKNESS (hue, alpha).
///
/// 16 since DRAGON-680, from 20 (the owner: "slightly narrower, with smaller circles to
/// match"). The four points each strip gives up are not reclaimed by the row: they go
/// into [`STRIP_GAP`], because the stack still has to start at the round swatch's top
/// edge and end at its bottom one, and what the owner wanted the narrowing FOR was air
/// between the two tracks.
pub const STRIP_H: f32 = 16.0;
/// The vertical gap between the two strips: 16 since DRAGON-680, which is exactly the
/// eight points the two strips gave up, so [`CONTROLS_H`] is unchanged and the stack
/// still aligns to the swatch at both ends (pinned by the assert below).
pub const STRIP_GAP: f32 = 16.0;
/// A strip's draggable thumb, four points proud of the track it rides so its rim is
/// grabbable above and below the strip (`widgets::color_field::within_marker`).
///
/// DERIVED from [`STRIP_H`] rather than written out, which is what makes "smaller circles
/// to match" automatic: the thumb followed the strip from 24 to 20 with no second edit,
/// and the next thickness change moves it again.
pub const STRIP_MARKER_D: f32 = STRIP_H + 4.0;
/// The round current-colour swatch's diameter, which is also the controls row's height:
/// the strips column (two strips plus their gap) comes out to the same 48, pinned below.
pub const SWATCH_CIRCLE: f32 = 48.0;
/// The gap between the round swatch and the strips: wider than [`ROW_SPACING`], the
/// owner's review ("slightly more space to the right of the current color swatch").
pub const GAP_SWATCH_TRACKS: f32 = 16.0;
/// The controls row's height.
pub const CONTROLS_H: f32 = SWATCH_CIRCLE;
/// One value box's height (the same control height the old rows used). Note this is the
/// BUDGETED height: a text input measures its own font's line height plus its padding,
/// which lands a shade over 34 at the default scale, so the view centres its neighbours
/// on the boxes rather than on this number (see `App::value_row`). The remainder is
/// absorbed by [`LAYOUT_SLACK_H`].
pub const VALUE_BOX_H: f32 = 34.0;
/// The caption band under the boxes ("R", "G", "B", "A"), and the gap around each band
/// of the value block.
pub const VALUE_LABEL_H: f32 = 16.0;
pub const VALUE_LABEL_GAP: f32 = 4.0;

// ── The mode ACTIVATOR and its menu (DRAGON-680) ─────────────────────────────
//
// The mode control is an up chevron over a down chevron, ONE hoverable unit, sitting to the
// RIGHT of the value boxes and vertically centred on them. Clicking it anywhere opens the
// upward menu of the seven notations.
//
// **Tombstone, because the control has been rebuilt three times and every change was the
// owner's.** DRAGON-630 shipped a chevron pair that STEPPED the mode; the owner's review
// replaced it with a dropdown selecting by index; DRAGON-676 rebuilt that dropdown as a
// hand-made chip (an icon-button's fills under the value boxes' own border and rounding,
// its name at the left and a `chevrons-up-down` caret at the right) because the stock
// widget could take none of the app's own hover wash, text span or caret. DRAGON-680 then
// stripped the chip back to the bare chevrons: "get rid of the dropdown styling and text,
// and just make it the up and down chevron".
//
// **What that ticket got wrong for one revision, and the correction that is now the
// design:** it read the chevrons as a two-button STEPPER (up = previous notation, down =
// next). The owner's answer was that "they were still supposed to together act as a single
// hoverable unit that triggers the dropdown menu", so the MENU is back and the pair is one
// button. What did NOT come back is the chip: no border, no fill, no mode NAME beside the
// chevrons. Nothing on the closed control says which notation is current; the caption band
// under the boxes does ("HEX", or "R G B A"), and the menu marks the current row in accent.
//
// So the arithmetic here is the panel's alone. The chip's own numbers (the caret gap, the
// caret's glyph box, the chip padding) are gone for good, but the LABEL MEASUREMENT is not:
// a fixed-width panel still has to be wide enough for the longest notation name it lists.

/// One chevron's glyph box. Small on purpose: the pair has to fit inside one value box's
/// height and still leave visible air between the two arrows.
pub const MODE_STEP_ICON: u16 = 12;
/// One chevron's ROW inside the activator, and the activator's width. The unit is WIDER
/// than its glyphs so the hover wash reads as a control and the click target is not a 12pt
/// square.
///
/// It named a whole BUTTON for one revision, when the pair was two buttons; the geometry is
/// unchanged, and only the number of controls it describes has come down to one.
pub const MODE_STEP_H: f32 = 16.0;
pub const MODE_STEP_W: f32 = 24.0;
/// The gap between the two chevron ROWS (the owner's "slightly more space between the up
/// and down icons than they have now", and then "the up and down chevrons are good
/// spacing", so this number is settled).
///
/// Two points, and the visible air is far more than that, which is the point of measuring
/// it here rather than eyeballing the drawn glyphs. A lucide chevron paints only the
/// middle quarter of its own box (its path runs y=9..15 of a 24 viewBox), so each 12pt
/// glyph carries about 4.5pt of empty box above and below its arrow, and each row adds 2pt
/// more. The pair therefore shows roughly 15pt between the two arrowheads, against the
/// 3.5pt of the single `chevrons-up-down` glyph it replaces, whose two arrows are baked
/// into one 14pt box.
pub const MODE_STEP_GAP: f32 = 2.0;

const _: () = assert!(
    2.0 * MODE_STEP_H + MODE_STEP_GAP == VALUE_BOX_H,
    "DRAGON-680: the chevron activator must be exactly one value box tall, or it is no \
     longer something the box row can centre against and the value row's height stops \
     being the boxes' own"
);

/// The mode MENU's label size, its row height, the gap between rows and the panel's own
/// inset. All FIXED so [`mode_menu_panel_h`] can be exact rather than an estimate.
///
/// The height matters more than it looks: the flyout places the panel's TOP exactly
/// `mode_menu_panel_h()` above the activator's top (`chrome::FlyoutDir`), so this sum
/// decides where the panel's BOTTOM lands. Under-counted, the menu slides down INTO the
/// control. It used to assume a "~27pt natural height" per row and the rows measured more,
/// which is what the owner saw as the menu bottoming out inside the old chip's lettering.
pub const MODE_LABEL_SIZE: f32 = 13.0;
pub const MODE_MENU_ITEM_H: f32 = 28.0;
pub const MODE_MENU_GAP: f32 = 2.0;
pub const MODE_MENU_PAD: u16 = 4;
/// A menu row's horizontal padding, per side. It was the CHIP's padding, shared with the
/// rows so the closed face and the open list lined their text up; with the chip gone it is
/// the rows' own.
pub const MODE_MENU_ROW_PAD: u16 = 8;

/// The allowance for the face we MEASURE not being the face that DRAWS.
///
/// [`widest_mode_label`] measures through the embedded Inter, the same faces
/// `preview::chrome::text_font_ctrl_w` size their own controls from, because that is the
/// one measurement this crate can take on any host, headlessly, and get the same answer
/// everywhere. What actually draws the label is `cosmic::font::default()`: the COSMIC
/// interface font where there is a COSMIC config to read, and whatever the system
/// substitutes for it on Windows and macOS. Close relatives, not the same metrics.
///
/// 15% is the slack between them, and it buys width ONLY. An over-wide panel is a few
/// points of air past the longest word; an under-wide one clips "OKLCH", and only one of
/// those is a defect. It is the same "measure the worst case, not the developer's machine"
/// move the hex chip's `view::label_metrics_tests::WIDEST_MONO_ADVANCE_EM` already makes,
/// from the other side.
const MODE_LABEL_HEADROOM: f32 = 1.15;

/// The widest of the seven notations' labels at [`MODE_LABEL_SIZE`], in points.
///
/// MEASURED over `ColorFormat::ALL` rather than assumed to be "OKLCH": the labels are
/// data, a notation added or renamed moves the answer, and a panel sized for a label the
/// list no longer carries is exactly the drift a hand-written constant invites.
///
/// This is the ONE place in this file that reads anything but plain numbers (see the module
/// doc). It is still deterministic: `text_annot::measure` sums advances out of an embedded
/// face's own tables, so it answers identically on every platform and in every test run.
fn widest_mode_label() -> f32 {
    crate::color::ColorFormat::ALL
        .into_iter()
        .map(|f| {
            crate::app::preview::text_annot::measure(
                crate::app::preview::text_annot::TextFont::Clean,
                MODE_LABEL_SIZE,
                f.label(),
            )
        })
        .fold(0.0f32, f32::max)
}

/// Pure, unit-tested: the mode menu panel's width.
///
/// As wide as its LONGEST option needs and not a point more, which is the same rule the old
/// chip was sized by; what it no longer includes is a caret and a closed-face label, since
/// the activator is bare chevrons. The panel is a FIXED width because the flyout is placed
/// by an exact offset, and because its rows FILL it so a hover highlight is as wide as the
/// row it highlights.
pub fn mode_menu_width() -> f32 {
    mode_menu_width_for(widest_mode_label())
}

/// Pure, unit-tested: [`mode_menu_width`]'s arithmetic with the measurement injected, so
/// the DECISION can be exercised at label widths no font on this machine produces.
///
/// Rounded UP: a fractional width puts the panel's edges on a half pixel, where its 1pt
/// outline stops agreeing with itself about where the panel is.
fn mode_menu_width_for(label_w: f32) -> f32 {
    (label_w * MODE_LABEL_HEADROOM
        + 2.0 * f32::from(MODE_MENU_ROW_PAD)
        + 2.0 * f32::from(MODE_MENU_PAD))
    .ceil()
}

/// Pure, unit-tested: the mode menu's on-screen HEIGHT, which is the upward flyout's exact
/// offset. Every part of the panel has a fixed size, so this sum is the panel exactly.
pub fn mode_menu_panel_h() -> f32 {
    let n = crate::color::ColorFormat::ALL.len() as f32;
    n * MODE_MENU_ITEM_H + (n - 1.0) * MODE_MENU_GAP + 2.0 * f32::from(MODE_MENU_PAD)
}

/// The two SWATCH-menu entries the panel offers (DRAGON-682), and the first of
/// them is offered by the history's menu too, so the two menus read as one vocabulary.
///
/// "Copy" carries no notation name on purpose: the spelling follows the REMEMBERED mode
/// (`ColorPickerState::swatch_copy_text`), and a label that named it would be a second
/// place the mode is written, out of date the moment the chevrons move.
pub const SET_ACTIVE_LABEL: &str = "Set as active color";
/// The harmony menu's middle entry (DRAGON-682 item 28). The SAME words the divider button
/// under the value row uses, because it is the same action on a different colour, and two
/// spellings of one verb would read as two features.
pub const ADD_TO_RECENTS_LABEL: &str = "Add to recents";
pub const COPY_COLOR_LABEL: &str = "Copy";

/// Pure, unit-tested: the harmony swatch menu's panel width, measured from its widest row
/// exactly as the notation menu is measured from its longest option.
pub fn harmony_menu_width() -> f32 {
    let widest = [SET_ACTIVE_LABEL, ADD_TO_RECENTS_LABEL, COPY_COLOR_LABEL]
        .into_iter()
        .map(|s| {
            crate::app::preview::text_annot::measure(
                crate::app::preview::text_annot::TextFont::Clean,
                MODE_LABEL_SIZE,
                s,
            )
        })
        .fold(0.0f32, f32::max);
    mode_menu_width_for(widest)
}

/// Pure, unit-tested: that menu's on-screen height, which is its upward flyout's offset.
/// THREE rows since DRAGON-682 item 28, their gaps, and the panel's own inset.
///
/// Test-only since DRAGON-687: the menus' row counts became DATA (whether the palette
/// submenu rows are offered), so the view asks [`menu_panel_h_for`] with the rows it
/// really built, and this fixed sum survives as the test that pins the generalised
/// arithmetic to the numbers the fixed menus always had.
#[cfg_attr(not(test), allow(dead_code))]
pub fn harmony_menu_panel_h() -> f32 {
    let n = HARMONY_MENU_ROWS as f32;
    n * MODE_MENU_ITEM_H + (n - 1.0) * MODE_MENU_GAP + 2.0 * f32::from(MODE_MENU_PAD)
}

/// How many rows the harmony swatch menu had before the palette submenu made the count
/// data (DRAGON-687): set active, add to recents, copy. Feeds the test-only pin above.
#[cfg_attr(not(test), allow(dead_code))]
pub const HARMONY_MENU_ROWS: usize = 3;

/// **Pure**, unit-tested: how far LEFT of a harmony swatch its menu starts, so the panel
/// lands inside the panel whichever column was right-clicked (DRAGON-682).
///
/// Same clamp as the history's ([`recents_menu_dx`]), against the CARD's own width and the
/// bar's own geometry: the segments start at the card's padding and march right by their own
/// widths ([`segment_x`]), which is not a constant stride since the bar hands its remainder
/// out a point at a time.
pub fn harmony_menu_dx(column: usize, segments: usize, panel_w: f32) -> f32 {
    let x = segment_x(column, segments);
    let overflow = (x + panel_w - card_w()).max(0.0);
    overflow.min(x)
}

/// Pure, unit-tested: the width the panel's own content occupies, inside the
/// panel's padding. What the cards and their menus are laid out against.
pub fn panel_content_w() -> f32 {
    panel_w() - 2.0 * WINDOW_PADDING
}

/// The colour history's context-menu wording. ONE constant, because the panel is sized
/// from the string it draws (DRAGON-680 item 24).
pub const REMOVE_RECENT_LABEL: &str = "Remove from recents";

/// Pure, unit-tested: the history context menu's panel width, measured from its own single
/// row exactly as the notation menu is measured from its longest.
pub fn recents_menu_width() -> f32 {
    let widest = [REMOVE_RECENT_LABEL, SET_ACTIVE_LABEL]
        .into_iter()
        .map(|s| {
            crate::app::preview::text_annot::measure(
                crate::app::preview::text_annot::TextFont::Clean,
                MODE_LABEL_SIZE,
                s,
            )
        })
        .fold(0.0f32, f32::max);
    mode_menu_width_for(widest)
}

/// Pure, unit-tested: that menu's on-screen height, which is its upward flyout's exact
/// offset. One row plus the panel's own inset.
///
/// Test-only since DRAGON-687, [`harmony_menu_panel_h`]'s own reason: the row count is
/// data now, the view asks [`menu_panel_h_for`], and this pins the generalised sum.
#[cfg_attr(not(test), allow(dead_code))]
pub fn recents_menu_panel_h() -> f32 {
    // TWO rows since DRAGON-682 item 7 added "Set as active color" above the remove entry.
    2.0 * MODE_MENU_ITEM_H + MODE_MENU_GAP + 2.0 * f32::from(MODE_MENU_PAD)
}

/// **Pure**, unit-tested: how far LEFT of a history swatch's own left edge its context menu
/// starts, so the panel lands inside the window whichever swatch was right-clicked
/// (DRAGON-680 item 24).
///
/// The anchor here is a 28pt swatch that can sit anywhere across the content, so neither of
/// the flyout's fixed alignments works alone: left-aligned runs off the right edge for the
/// last few swatches, right-aligned runs off the left edge for the first few. The rule is
/// therefore "left-aligned unless that would overflow, and then only as far left as it has
/// to be", which is one clamp:
///
/// * the panel's left edge is `x - dx` and must not go negative, so `dx <= x`;
/// * its right edge is `x - dx + panel` and must not pass [`CONTENT_W`], so
///   `dx >= x + panel - CONTENT_W`.
///
/// A panel wider than the content cannot satisfy both; the left edge wins there, because a
/// menu clipped at the window's right edge still shows its text while one clipped at the
/// left shows the end of a sentence.
pub fn recents_menu_dx(index: usize, panel_w: f32) -> f32 {
    let col = index % RECENTS_PER_ROW;
    let x = col as f32 * (RECENT_SWATCH + recents_gap());
    let overflow = (x + panel_w - CONTENT_W).max(0.0);
    overflow.min(x)
}

/// Pure, unit-tested: how far LEFT of the activator's own left edge the menu panel starts,
/// for the flyout's placement.
///
/// **The panel is RIGHT-aligned to the activator**, which is a change from every other
/// flyout in this app (they all hang left-aligned off a button). The reason is position:
/// this activator sits at the RIGHT edge of the window's content, with only the window
/// padding beyond it, so a panel left-aligned to it would run off the window and be clipped.
/// Aligning the two right edges puts the whole panel inside the content it belongs to.
pub fn mode_menu_dx() -> f32 {
    (mode_menu_width() - MODE_STEP_W).max(0.0)
}

/// The BOX BAND's height: the tallest thing on the row that carries the value boxes.
///
/// That is the COPY BUTTON since DRAGON-680's item 23, not a box: the button leads the row
/// at the pipette's 48pt square and the boxes are 34, so the band is the button's height
/// and the boxes centre in it. The band grew 14pt for it, and the window with it; there is
/// no honest way to have a 48pt control inside a 34pt row, and the alternatives were worse
/// than the height (a smaller button, which the owner ruled out with "still same size", or
/// letting the button overdraw its row into the caption band below it).
pub const BOX_BAND_H: f32 = CONTROLS_BUTTON;

const _: () = assert!(
    BOX_BAND_H >= VALUE_BOX_H,
    "DRAGON-680: the box band must be at least as tall as the boxes in it, or the row is \
     shorter than its own content"
);

/// The whole value row: the box band (the copy button, the boxes, the mode activator) and
/// the caption band under them.
///
/// The mode ROW is gone (DRAGON-680): the dropdown, the split toggle and the copy button
/// all left it, so the band that carried them left with them. The copy button then came
/// BACK to the value block on the owner's item 23, not as a row of its own but as the box
/// row's leader, which costs the row the difference between a 48pt button and a 34pt box
/// and costs the window nothing else.
pub const VALUE_ROW_H: f32 = BOX_BAND_H + VALUE_LABEL_GAP + VALUE_LABEL_H;

/// Gap between the value boxes and the mode stepper, and between the controls row's own
/// neighbours (the pipette, the copy button and the swatch).
pub const ROW_SPACING: f32 = 8.0;
/// Gap between neighbouring value boxes, and (the owner's ask) between the mode row and
/// the boxes under it, so the air around a box reads the same in both directions.
pub const BOX_GAP: f32 = 6.0;
/// The gap BELOW the divider, before the colour history.
///
/// 24, doubled from 12 on the owner's review: that hairline is the window's one
/// horizontal rule, and at 12 it read as a line crowded by its neighbours rather than as
/// the break between the colour you are working on and the colours you kept.
pub const SECTION_GAP: f32 = 24.0;
/// The gap ABOVE the divider, after the value block. Deliberately SMALLER than
/// [`SECTION_GAP`], and the difference is not a fudge: what sits above the line is CAPTION
/// TEXT, whose line box carries empty descender space under the letters, while what sits
/// below it is a swatch with a hard top edge. Equal numbers therefore do not read as equal
/// air, which is exactly what the owner reported; four points of it is the text's own
/// slack, given back.
pub const GAP_VALUE_DIVIDER: f32 = 20.0;
/// The horizontal divider between the value row and the colour history (the owner's
/// review): one hairline, [`SECTION_GAP`] of air on each side.
pub const DIVIDER_H: f32 = 1.0;
/// The BAND the divider occupies in the window column, which is the height of the "Add
/// color" button centred on the line rather than the line's own hairline.
///
/// The button is small on purpose (the owner's ask) and it is the reason this band
/// exists: a control sitting ON a rule has to be given room by the rule's row, or the
/// section under it is pushed down by however much the button overhangs. The air the
/// owner asked for around the divider is measured from the LINE, so half this height eats
/// into each [`SECTION_GAP`], which is what "centred on the line" looks like.
pub const DIVIDER_BAND_H: f32 = 24.0;
/// The plus glyph inside that button: smaller than the row icons, because it sits next to
/// 12pt text rather than standing alone.
pub const ADD_COLOR_ICON: u16 = 12;
/// One recent-colour swatch.
pub const RECENT_SWATCH: f32 = 28.0;
/// The width of the FOCUS FRAME a stop wears while the window's Tab ring is parked on it
/// (DRAGON-680): libcosmic's own focused-input border width, restated because the toolkit
/// hard-codes it in its theme (`theme::style::text_input`) and exposes no token.
///
/// It lives here rather than in `view` because [`HISTORY_FOCUS_OUTSET`] is measured
/// against it: the frame has to be thinner than the space it sits in, or there is no air
/// between it and what it frames.
pub const FOCUS_RING_W: f32 = 2.0;
/// How far OUTSIDE the colour history's own bounds its focus frame is drawn.
///
/// **The frame is outside the swatches, with air, and it costs the layout NOTHING**
/// (DRAGON-680, the owner's veto of the first attempt: "the border highlight should be
/// outside of the swatches not clipping their edges"). That first attempt drew the border
/// ON the block's bounds, where it overlapped the outer swatches' own 1pt rims, chosen
/// because every part of this window is a fixed size and reserving space for a frame
/// looked like it had to come out of the grid's width, where it would have broken the
/// flush edges the owner has asked for at every review.
///
/// It does not have to come from there. The history is the LAST section, and it is
/// surrounded by margins that are already blank: [`SECTION_GAP`] above it, the window's
/// own [`WINDOW_PADDING`] below and to both sides. The frame is paid for out of those, by
/// shrinking the gap above it and the padding around it by exactly this much and giving
/// the same amount back as the frame's own inset. Nothing moves, the grid keeps its full
/// [`CONTENT_W`] and its flush edges, and [`color_window_size`] is unchanged: pinned by
/// `the_history_frame_is_paid_for_by_the_margins_it_sits_in`.
///
/// SIX, which leaves 4pt of air between the frame and the swatch rims once the frame's own
/// [`FOCUS_RING_W`] is taken off. That is enough to read as a frame AROUND the block
/// rather than a border ON it, and it is comfortably inside every margin it spends
/// (asserted below), so raising it is a one-line change until it is not.
pub const HISTORY_FOCUS_OUTSET: f32 = 6.0;

const _: () = assert!(
    HISTORY_FOCUS_OUTSET > FOCUS_RING_W,
    "DRAGON-680: the history's focus frame must sit clear of the swatches, or it is the \
     border-on-the-bounds the owner rejected with extra steps"
);
const _: () = assert!(
    HISTORY_FOCUS_OUTSET <= SECTION_GAP && HISTORY_FOCUS_OUTSET <= WINDOW_PADDING,
    "DRAGON-680: the frame is paid for out of the gap above the history and the window \
     padding around it, so it cannot be wider than either without moving the layout"
);
/// The ROUND swatch's outline, drawn as an analytic quad ON TOP of its raster, and how far
/// inside that outline the raster's own disc stops (DRAGON-680).
///
/// **This is the third answer to "the swatch is blocky around the rim", and the first one
/// that reaches the screen.** The first two were raster-side and both failed for the same
/// reason, so the reason is written down here rather than in a commit nobody re-reads:
///
/// 1. The disc has always had an analytic ONE-PIXEL coverage ramp at its edge. At a 48pt
///    disc on a 1x display that ramp is one device pixel against a 24px radius, which
///    smooths a staircase whose steps are also about a pixel: it takes the edge from hard
///    to slightly-soft-and-still-stepped. That is what the owner reported.
/// 2. So the raster was built SUPERSAMPLED (3x, 144px) on the assumption that the drawn
///    image would be downsampled with a decent filter. It is not. iced draws it through
///    its image atlas, which carries NO MIPMAPS, so minification with the linear sampler
///    averages a 2x2 texel neighbourhood and nothing more: a 3x downscale therefore reads
///    roughly every third texel and throws the rest away, feathered pixels included, and
///    lands back on the same staircase. Supersampling into a mip-less sampler is not
///    anti-aliasing, it is decimation. (The screenshot after that change was identical to
///    the one before it, which is the whole lesson: verify the DRAW PATH, not the buffer.)
///
/// What does reach the screen is the renderer's own analytic anti-aliasing, which is what
/// makes the slider thumbs one row up look right: a QUAD with a corner radius is drawn from
/// a signed distance field at the display's real resolution, so its edge is smooth at any
/// scale and needs no buffer at all. The disc therefore keeps the raster for its INTERIOR
/// (the checkerboard under a translucent colour, which a quad cannot express) and gets its
/// SILHOUETTE from a quad ring stacked over it, in the same subdued ink the raster's own rim
/// band uses. The owner asked for exactly this: "overlap the outline on top of it".
///
/// [`SWATCH_EDGE_MASK`] is what makes the mask complete: the raster's disc stops that far
/// inside the ring's outer edge, so its stepped boundary AND its coverage ramp are both
/// under the ring's opaque band and only the ring's analytic edge is ever visible.
pub const SWATCH_RING_W: f32 = 2.0;
/// How far inside the analytic ring's outer edge the round swatch's RASTER stops.
///
/// One point, which is the width of the raster's own coverage ramp, so the ramp ends
/// exactly where the ring's opaque band begins. It must stay strictly less than
/// [`SWATCH_RING_W`] (asserted below) or the raster's edge would poke out past the ring
/// and the staircase would be visible again.
pub const SWATCH_EDGE_MASK: f32 = 1.0;

const _: () = assert!(
    SWATCH_EDGE_MASK < SWATCH_RING_W,
    "DRAGON-680: the analytic ring must fully cover the raster's own edge, or the disc is \
     blocky again in a band the ring does not reach"
);
/// One of the controls row's two ROUND-SWATCH-SIZED buttons: the pick-again pipette that
/// leads the row (DRAGON-630, the reference layout's eyedropper position) and, since
/// DRAGON-680, the copy button immediately after it.
///
/// Named for the pair rather than for the pipette (it was `PICK_AGAIN_W`), because the
/// owner's ask for DRAGON-680 was exactly that the copy button be "the same size as the
/// color picker button": one constant is what makes that true by construction instead of
/// by two numbers agreeing today.
pub const CONTROLS_BUTTON: f32 = SWATCH_CIRCLE;
/// The glyph inside one of those buttons.
///
/// 24 since DRAGON-680, from 32 (the owner: the icon "needs some padding inside of the
/// hoverable circle. Maybe a slightly reduced icon size while keeping the area the
/// same"). The BUTTON is untouched at [`CONTROLS_BUTTON`], so what changes is only the
/// inset: 8pt on each side becomes 12. At exactly half its button the glyph also lands on
/// the proportion the app's ordinary bare icon buttons carry (a 16pt glyph in a 34pt
/// box), so the two sizes of icon button here read as the same kind of control.
pub const CONTROLS_ICON: u16 = 24;
/// The slider strips' width: what the controls row leaves after the pipette, the round
/// swatch and the spacing between them.
///
/// **268, and the tracks are the whole reason this row is worth measuring.** The number has
/// moved three times inside DRAGON-680 and the trail is worth keeping, because each move
/// was a control arriving or leaving: 212 to start, 156 when the copy button joined this
/// row, 164 when the owner took the gap out from between the two buttons, and 268 now that
/// item 23 has moved the copy button off this row entirely and on to the value row. The
/// strips expand to take back every point of it (the owner's explicit confirmation), so
/// this row has one button, one swatch and the longest tracks the picker has ever had.
pub const STRIPS_W: f32 =
    CONTENT_W - CONTROLS_BUTTON - ROW_SPACING - SWATCH_CIRCLE - GAP_SWATCH_TRACKS;

const _: () = assert!(
    STRIP_H * 2.0 + STRIP_GAP == CONTROLS_H,
    "DRAGON-630: the stacked hue and alpha strips must fill the controls row exactly, \
     or the row centres them against a height that is not theirs and the swatch and \
     strips visibly misalign"
);

// The travel floor, re-derived by DRAGON-680 rather than merely lowered. It was 200 while
// the tracks had 212, and the number was never argued; this one is. A hue strip spends its
// width on 360 degrees, so 164pt is 2.2 degrees per point, and on the HiDPI displays this
// app is used on that is about 1.2 degrees per physical pixel: still a strip you can aim a
// hue in, and the STRIPS were never the fine control anyway (the square is, for S and V,
// and the value boxes are, for an exact number). The alpha track is the easier of the two,
// at 1.6 of its 256 levels per point. Under about 150 that stops being true, so that is
// where the floor sits. (It read 156 for one revision, before the owner's correction took
// the gap out from between the two buttons and handed those 8 points back to the tracks.)
const _: () = assert!(
    STRIPS_W >= 150.0,
    "the hue and alpha tracks no longer have enough travel to aim in; give the controls \
     row's width back, or widen CONTENT_W (and re-check the history grid's whole-point gap)"
);

/// How many recent colours the history holds. Beyond this the OLDEST is dropped.
///
/// EIGHTEEN, as two rows of [`RECENTS_PER_ROW`]. It was twenty (DRAGON-649 widened the
/// rows from eight to ten); the owner traded two entries back for window width when the
/// value row's controls moved up onto their own line, since the history row is what
/// holds the window's width up now. The two-row shape is the DRAGON-630 review's,
/// matching the reference layout, and before that it was one row of ten.
pub const RECENTS_CAP: usize = 18;
/// Swatches per history row (nine now; ten through DRAGON-649; eight before that).
pub const RECENTS_PER_ROW: usize = 9;
/// The client-side header bar's REAL height, from libcosmic's own arithmetic
/// (DRAGON-687's drag-scroll round, the overlay-alignment fix).
///
/// **`HEADER_H = 44.0` sat here from DRAGON-582 to that round, and it was a fiction.**
/// libcosmic's CSD header is `32 + vertical padding`, and its unmaximized padding table
/// has no 44 in it: `[7, 7, 8, 7]` at Standard density (47) and `[3, 7, 4, 7]` at
/// Compact (39). The 3pt shortfall at Standard pushed every REAL widget below the header
/// 3pt lower than this file's rects assumed, [`LAYOUT_SLACK_H`] quietly absorbed it at
/// the window's bottom, and the error stayed invisible until the palette groups' 28pt
/// bars gave it something small enough to read against: the owner's "the dropzone
/// overlay is 1 or 2 px too high". This fn replicates the exact CSD-unmaximized
/// branch of libcosmic's table (this window is never maximized, its size is pinned, and
/// it always draws CSD), keyed on the same `header_size` config the widget reads, so
/// the sum and the widget cannot disagree again.
///
/// The ONE live input this file reads beyond fonts (see the module doc's measurement
/// exception): the density is a config the user changes at most rarely, it is read
/// per call rather than cached, and every consumer in this file goes through here, so
/// the whole window re-derives together. Tests pin RELATIONS against this fn rather
/// than absolute heights, because the answer legitimately differs per density.
pub fn header_h() -> f32 {
    match cosmic::config::header_size() {
        cosmic::cosmic_theme::Density::Compact => 39.0,
        _ => 47.0,
    }
}
/// Vertical SLACK added to the window height (DRAGON-630 rev 4): the widgets carry
/// natural padding this arithmetic cannot measure headlessly (text inputs and captions
/// resolve their own line heights), and every under-count lands on whichever section is
/// LAST — the history rows, which the owner has twice photographed squished. The slack
/// is one named knob rather than quiet inflation of a real part, so the next person can
/// see exactly how much of the height is measurement humility. 24 overshot by about
/// 16 on the owner's screen (air under the last history row); 8 is the measured rest.
pub const LAYOUT_SLACK_H: f32 = 8.0;

// ── Which layout a mode's value row takes (DRAGON-680) ───────────────────────
//
// DRAGON-676's mode-chip arithmetic stood here: the measured widest label, a 15%
// measure-versus-draw headroom, the caret gap and the chip padding, plus the "Copied!"
// word's size and gap. All of it went with the dropdown and the mode row (see the
// tombstone beside `MODE_STEP_ICON`). What replaces it is one predicate, because the
// layout is no longer a remembered TOGGLE, it is a property of the mode.

/// **Pure**, unit-tested: whether `mode`'s value row shows SPLIT per-component boxes
/// (`true`) or ONE unified box holding the whole spelling (`false`).
///
/// **Hex is unified and everything else is split** (DRAGON-680, the owner's ask: "let's
/// make hex always one unified input, and the rest always split input, so we can get rid
/// of the toggle split button"). The rule earns its shape: a hex value is ONE token that
/// people read, type and paste whole (`#FF8800CC`), and splitting it into `FF` `88` `00`
/// `CC` makes the commonest paste in the tool impossible without four selections; every
/// other notation is genuinely several numbers, and a user reaching for `oklch` wants to
/// nudge the chroma without retyping the rest.
///
/// **Tombstone.** DRAGON-630 rev 3 made this a persisted toggle
/// (`state::schema::color_picker_split_inputs`, a list-chevrons button beside the copy
/// button) that switched EVERY mode between the two layouts. The owner reversed it: the
/// setting is gone from the schema, the button is gone from the row, and the answer is
/// derived. Do not reintroduce a stored flag here; if a mode ever wants the other layout,
/// it belongs in this match, where every mode's answer can be read at once.
pub fn splits_components(mode: crate::color::ColorFormat) -> bool {
    !matches!(mode, crate::color::ColorFormat::Hex)
}

/// **Pure**, unit-tested: the DRAFT index of the box at row POSITION `pos` in `mode`
/// (DRAGON-680).
///
/// The two numbering schemes exist because a box has two identities: its place in the row
/// (what Tab and the focus ids count) and what it EDITS (what
/// `ColorPickerState::draft`, `box_text` and `BoxEdited` count). They agree for every
/// split mode, and differ for hex, whose one box edits the whole spelling and is therefore
/// [`super::WHOLE_VALUE_BOX`] rather than component 0.
///
/// One function so the two never disagree: a mismatch would show as a draft that renders
/// in the wrong box, or as focus arriving somewhere the caret is not.
pub fn draft_index(mode: crate::color::ColorFormat, pos: usize) -> usize {
    if splits_components(mode) { pos } else { super::WHOLE_VALUE_BOX }
}

/// Where keyboard focus is in the colour picker WINDOW (DRAGON-680).
///
/// The window's Tab ring is a closed cycle of stops: every value box in order, then the
/// mode activator, then the colour history as ONE stop, then back to the first box. That is
/// the owner's model, and [`next_focus`] is its whole statement.
///
/// **Only the BOXES carry toolkit focus.** A value box is a real text input, so Tab must
/// give it the caret and its own accent outline. The other two stops are ours: the
/// activator and the history draw the SAME accent frame from this state, and the toolkit's
/// focus is cleared while either holds it, so no caret is left blinking in a box the user
/// has tabbed away from (`App::apply_picker_focus`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickerFocus {
    /// A value box, by ROW POSITION (0 is the first, whatever the mode).
    Box(usize),
    /// The mode activator. Up and down then step the notation.
    Mode,
    /// The colour history, as a single stop: the whole grid is framed, and the four arrows
    /// move a navigation CURSOR through it. The cursor does not change the colour;
    /// Space or Enter applies the swatch it is on (DRAGON-682 item 7).
    History,
    /// The PANEL's harmony swatches, as a single stop, and only while the window is
    /// expanded (DRAGON-682 item 9). The arrows move a cursor through the harmony cards in
    /// reading order; Space and Enter COPY the swatch the cursor is on (item 32, which
    /// amended item 9's do-nothing rule), which is the same thing that swatch's right-click
    /// Copy does. It does NOT apply the colour, unlike [`Self::History`]: see
    /// [`accept_action`] for why the two stops differ.
    Panel,
}

/// **Pure**, unit-tested: the focus stop a Tab (`forward`) or Shift+Tab lands on.
///
/// The ring, in the owner's words: "tabbing starts at the first input. after the last input
/// it can go to the up/down button. tabbing again should highlight the entire color history.
/// tabbing from that should take us back to the first input. we should be able to tab
/// forwards and backwards through this entire set."
///
/// `boxes` is the current mode's box count (one for hex, four or five for the rest), and
/// `has_history` is false while the history is EMPTY, which drops that stop from the ring
/// entirely: a frame around a grid of empty placeholder slots is a stop with nothing in it,
/// and its arrow keys would have nothing to move.
///
/// `None` (nothing focused: the window has just opened, or a click landed somewhere that is
/// not a stop) enters the ring at the first box going forward and at the LAST stop going
/// back, the same "enter from the end you pressed from" rule `keynav::step` states for a
/// plain list.
///
/// **Why this is ours rather than the toolkit's.** libcosmic drives Tab through
/// `keyboard_nav::subscription` into iced's `focus_next` / `focus_previous`, which visit
/// every widget implementing `operation.focusable` — and in libcosmic a BUTTON is one
/// (`widget/button/widget.rs`). This window is full of buttons: the pipette, the copy
/// button, the activator, Add to recents and up to eighteen history swatches. So the toolkit's
/// cycle wraps around ALL of them, which is what the owner saw as focus vanishing after the
/// last input instead of coming back to the first. The picker turns that blanket navigation
/// off for its own window and states the ring here instead.
pub fn next_focus(
    current: Option<PickerFocus>,
    forward: bool,
    boxes: usize,
    has_history: bool,
    has_panel: bool,
) -> PickerFocus {
    // The ring as a LIST of stops, built once so forward and backward are the same walk in
    // opposite directions and cannot disagree about the order.
    let mut stops: Vec<PickerFocus> = (0..boxes.max(1)).map(PickerFocus::Box).collect();
    stops.push(PickerFocus::Mode);
    if has_history {
        stops.push(PickerFocus::History);
    }
    // The panel comes LAST and only exists while the window is expanded (DRAGON-682 item
    // 9), gated exactly the way the history's own stop is: a stop the user cannot see is a
    // place Tab appears to lose focus.
    if has_panel {
        stops.push(PickerFocus::Panel);
    }
    // A stale box position (the mode changed to one with fewer boxes while it held focus)
    // is not in the ring; it enters from the pressed end like "nothing focused" does.
    let at = current.and_then(|c| stops.iter().position(|s| *s == c));
    let next = crate::keynav::step(at, if forward { 1 } else { -1 }, stops.len())
        .unwrap_or(0);
    stops[next]
}

/// The MOST boxes any mode's value row can hold, which is CMYK's four components plus the
/// shared alpha box.
///
/// It exists for the FOCUS ids (DRAGON-680 item 8): the window mints one stable
/// `widget::Id` per box position once, so Tab can move between them and the first can be
/// focused on open, and that list has to be long enough for every mode. Pinned against
/// `ColorFormat::ALL` by `value_layout_tests`, so a notation with more components fails a
/// test instead of silently sharing one id between two boxes.
pub const MAX_VALUE_BOXES: usize = 5;

/// Pure, unit-tested: the gap of the history GRID, both directions (the owner's rev-3
/// ask made the row gap equal the column gap). Chosen so a FULL row of
/// [`RECENTS_PER_ROW`] spans exactly [`CONTENT_W`]: the first swatch sits on the
/// content's left edge and the last lands flush with the tracks' right edge, which is
/// the owner's alignment ask. Whole points by [`CONTENT_W`]'s own construction.
pub fn recents_gap() -> f32 {
    (CONTENT_W - RECENTS_PER_ROW as f32 * RECENT_SWATCH) / (RECENTS_PER_ROW as f32 - 1.0)
}

/// The budget the value BOXES share: the content width, less what the row spends on the
/// two controls that flank them (DRAGON-680).
///
/// The row reads left to right: the COPY button, a gap, the boxes, a gap, the mode
/// ACTIVATOR. So the boxes give up [`CONTROLS_BUTTON`] and [`MODE_STEP_W`] and two
/// [`ROW_SPACING`]s, and [`CONTENT_W`] was chosen to leave what is left at exactly the
/// width the boxes had before the copy button arrived (300pt, so CMYK's five stay 55).
///
/// The copy button is the row's LEADER rather than a trailer beside the activator (the
/// owner's item 23: "the start of the input row"), which also keeps the two controls that
/// change the value's SPELLING at one end and the one that copies it at the other.
fn value_boxes_total() -> f32 {
    CONTENT_W - CONTROLS_BUTTON - MODE_STEP_W - 2.0 * ROW_SPACING
}

/// Pure, unit-tested: the BASE width of one value box when the row holds `boxes` of them
/// (DRAGON-630), floored to whole points so no box edge lands on a half pixel.
///
/// This is the floor; [`value_box_widths`] is what the view lays out, because the floor
/// leaves a remainder and that remainder has to go somewhere visible. The count is the
/// mode's components plus the alpha box: 4 everywhere except CMYK's 5, and every count
/// must leave a box a number is actually readable in (pinned by the tests).
pub fn value_box_width(boxes: usize) -> f32 {
    let boxes = boxes.max(1) as f32;
    ((value_boxes_total() - (boxes - 1.0) * BOX_GAP) / boxes).floor()
}

/// Pure, unit-tested: the width of EACH value box, left to right.
///
/// [`value_box_width`]'s floor leaves up to a few points over, and now that the boxes own
/// the whole content width that remainder is VISIBLE: dropped, it would end the box row
/// short of the right edge while the history grid and the slider tracks below it land
/// flush there, which is the alignment the owner has asked for at every review. So it is
/// handed out one point at a time from the left. No two boxes then differ by more than a
/// single point, which nobody can see, and the row is flush at both ends, which anybody
/// can.
pub fn value_box_widths(boxes: usize) -> Vec<f32> {
    let n = boxes.max(1);
    let base = value_box_width(n);
    let used = n as f32 * base + (n as f32 - 1.0) * BOX_GAP;
    // Whole points only: a fractional tail (CONTENT_W is whole, so there is none today)
    // stays as trailing slack rather than putting an edge back on a half pixel.
    let mut spare = (value_boxes_total() - used).floor().max(0.0) as usize;
    (0..n)
        .map(|_| {
            if spare > 0 {
                spare -= 1;
                base + 1.0
            } else {
                base
            }
        })
        .collect()
}

/// Pure, unit-tested: the UNIFIED box's width, which hex takes ([`splits_components`]):
/// the entire budget the split boxes share, so both layouts span the same width and the
/// mode stepper beside them never moves when the mode changes.
pub fn value_whole_width() -> f32 {
    value_boxes_total()
}

// ── The side PANEL (DRAGON-682) ────────────────────────────────

/// Pure, unit-tested: the width the PICKER's own column occupies inside the window frame,
/// which is what pins the left half when the panel opens (DRAGON-682).
///
/// **The existing UI must not move**, so the picker's column is laid out at this exact
/// width in BOTH states rather than at `Length::Fill`. In the collapsed window that is the
/// whole content area and nothing changes; in the expanded one it is what stops the column
/// stretching across the new space and dragging every row with it.
///
/// It is the content plus the window's own padding, i.e. everything inside the frosted
/// border on the picker's side.
pub fn picker_column_w() -> f32 {
    CONTENT_W + 2.0 * WINDOW_PADDING
}

// **Tombstone: the expand/collapse JITTER machinery** (`panel_mounted`,
// `PANEL_TOGGLE_DELAY`, `ColorPickerState::{window_w, panel_settled, toggle_seq}`, the
// `PanelSettled` / `ShrinkWindow` / `ResizeSettled` messages), all deleted by DRAGON-682
// item 42 at the owner's instruction: "remove the expand/collapse delay and whatever tricks
// we're using to prevent the expand/collapse jitter because none of it is working. ill deal
// with that later."
//
// The DEFECT, still open: expanding or collapsing the panel shows a few frames in which the
// picker's column jolts left and the panel's content is crushed, before the window settles
// at its new size. It is short and it looks broken.
//
// TWO designs were tried, and NEITHER fixed it on a real compositor:
//
// 1. **Item 31, the clipped partial reveal.** The picker column stayed a fixed width and the
//    panel took whatever width was LEFT OVER, its content laid out at full width and shown
//    through a clip, so every transitional frame was a correct panel partly revealed rather
//    than a whole one squeezed. It reads well in the pure layer, and the owner rebuilt it and
//    reported no improvement: an eye reads a 40%-wide panel as a broken panel, so a partial
//    panel is not worth drawing.
// 2. **Items 33 and 34, whole-or-nothing plus a delay.** The panel was mounted only once the
//    platform reported the surface really was at the expanded width AND a tuning delay
//    (100ms, then 250ms) had elapsed, with collapse the mirror: unmount, wait, then shrink. A
//    generation counter kept a second toggle from being completed by the first one's timers,
//    and a bounded fallback covered a resize event that never arrived. The owner rebuilt that
//    too, at both delays, and the jitter was still there: whatever moves is not the panel's
//    mounting.
//
// So the machinery is gone and the panel is now a plain function of the persisted flag:
// toggle, ask for the size, draw. **If you pick this up again, start by finding out what
// actually moves in those frames** (the surface, the compositor's own resize animation, the
// window server's, or our layout) rather than adding a third layer of application-level
// timing on top of two that were measured not to work.

/// The tab STRIP's height, which is libcosmic's own (`tab_bar::horizontal` sets
/// `.button_height(44)` and the widget takes exactly that).
///
/// Read here for HIT TESTING only (the panel's drop zone starts under the strip); the strip
/// itself is laid out by the toolkit and this constant does not size anything.
pub const PANEL_TAB_STRIP_H: f32 = 44.0;

/// The y of the DIVIDER band's top edge, in window coordinates: the row carrying the "Add
/// to recents" button.
///
/// It is THE boundary between the window's two drop zones (DRAGON-682 item 35, the owner:
/// "the main tool area ... everything above the horizontal row that has the add to recents
/// button"). Built by walking the same stack [`color_window_size`] sums, so the two cannot
/// disagree about where anything is; `drop_zone_matches_the_window_height` pins that.
pub fn divider_band_top() -> f32 {
    WINDOW_BORDER
        + header_h()
        + WINDOW_PADDING
        + SV_H
        + GAP_SQUARE_CONTROLS
        + CONTROLS_H
        + GAP_CONTROLS_VALUE
        + VALUE_ROW_H
        + GAP_VALUE_DIVIDER
}

/// Where a colour can be DROPPED (DRAGON-682 items 35 to 39, widened by DRAGON-687).
///
/// The zone is the HIGHLIGHT's identity as well as the hit test's answer: the dashed
/// raster is rebuilt when it changes, so a variant here has to name a region, not a pixel.
/// The insertion SLOT a reorder needs is deliberately not part of it (it moves per pixel;
/// [`palette_color_slot`] and [`palette_group_slot`] resolve it from the position when the
/// action is taken, and the view draws it as an analytic line, never a raster).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropZone {
    /// Everything above the divider row: the SV square, the controls row and the value row.
    /// The tools that show one colour, which is why dropping here means "make it this one".
    Main,
    /// The history grid, and the divider row itself. The row belongs to this zone because it
    /// carries the "Add to recents" button: a drop on the button that files a colour should
    /// file the colour.
    Recents,
    /// ONE saved palette's block (its heading and its bar row), while Saved Palettes is
    /// showing (DRAGON-687). Dropping a colour here APPENDS it to that group, and dropping
    /// a colour of this very group reorders it instead.
    PaletteGroup(usize),
    /// The Saved Palettes SCROLL AREA outside any group's own block: the gaps between
    /// groups and the run-out below the last one (DRAGON-687). Only a GROUP NAME drag means
    /// anything here (reordering the groups); a colour dropped in a gap is a cancel, so a
    /// sloppy drop can never pick a group the user did not aim at.
    PaletteStrip,
}

/// What is being DRAGGED (DRAGON-682 items 35 to 37; DRAGON-687 added the two palette
/// sources). One machine, one source enum: the sources differ only in what a drop MEANS,
/// which is [`drop_action`]'s table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DragSource {
    /// A harmony swatch in the panel, by `(group, swatch)`: the position IS the identity,
    /// because that is what the pressed segment knows about itself (DRAGON-682 item 41).
    Harmony(usize, usize),
    /// The window's own round swatch, i.e. the colour that is currently active.
    Active,
    /// A history entry, by index.
    Recent(usize),
    /// A saved palette's colour, by `(group, index)` (DRAGON-687).
    PaletteSwatch(usize, usize),
    /// A saved palette's NAME, by group (DRAGON-687): dragging it reorders the groups, and
    /// dragging it off the window asks to delete the group (with the confirmation).
    PaletteName(usize),
}

/// What a completed drag DOES (DRAGON-682 item 38's matrix, plus DRAGON-687's palette
/// rows and columns).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropAction {
    /// Load the dragged colour as the active one, with its alpha. The harmony menu's "Set as
    /// active color", including the recents write item 22 gave it.
    SetActive,
    /// Load the dragged colour as the active one WITHOUT filing it anywhere (DRAGON-687):
    /// a palette swatch dropped on the tools. It is a saved colour being taken back, like a
    /// recents click, so it must not reorder the history behind the user
    /// (`ColorSource::RecentClick` is the source the handler uses).
    SetActiveNoFile,
    /// File the dragged colour in the history. The menu's "Add to recents".
    AddToRecents,
    /// Load this history entry, exactly as clicking it does: colour and alpha, no write.
    LoadRecent(usize),
    /// Forget this history entry, the same removal its menu and the Delete key perform.
    RemoveRecent(usize),
    /// APPEND the dragged colour at the END of this palette (DRAGON-687, the owner's rule:
    /// "dropped colors also append to the end and not the middle"). A duplicate is a no-op
    /// ([`palette_append`]).
    AppendToPalette(usize),
    /// COPY a palette colour into another palette, appended at its end through the one
    /// duplicate-guarded admit; the source keeps its colour (DRAGON-687).
    ///
    /// **A drag COPIES between groups, by the owner's reversal** ("dragging and dropping
    /// from one palette to another should copy, not move"). The first cut had the drag
    /// MOVE, on the judgment that dragging a thing somewhere is what moving means
    /// everywhere else; the owner overruled it: copy is the SAFE default for a gesture,
    /// since a copy that was meant as a move costs one explicit removal, while a move
    /// that was meant as a copy silently vacates the source. Both explicit verbs live on
    /// in the swatch menu ("Move to palette ›" still moves and vacates, "Copy to
    /// palette ›" still copies), so the drag is the safe form, not the only form.
    CopyToPalette { from: (usize, usize), to: usize },
    /// Reorder a colour WITHIN its palette: the owner's "drag and drop sortable along
    /// their width". `to` is an insertion slot in the group's original order.
    ReorderPaletteColor { group: usize, from: usize, to: usize },
    /// Forget a palette colour: dragged off the window (DRAGON-687). No confirmation, the
    /// same contract the recents' own drag-off has; only GROUP deletion confirms.
    RemovePaletteColor { group: usize, index: usize },
    /// Reorder the GROUPS: a palette's name dropped back on the strip. `to` is an
    /// insertion slot in the original group order.
    ReorderGroup { from: usize, to: usize },
    /// A palette's name dragged off the window: ASK to delete that group (DRAGON-687).
    /// The action opens the confirmation; nothing is removed until the user answers it.
    DeleteGroupRequest(usize),
}

// **Tombstone: `DropAction::PaletteNotice`** (DRAGON-682 item 39, retired by DRAGON-687).
// While Saved Palettes was a placeholder, the whole panel was ONE coarse `DropZone::Palette`
// and every drop there raised a "coming soon" card naming the colour, so the gesture was
// answered rather than swallowed. Real palettes replaced both: the zone is per GROUP now,
// the notice's state (`ColorPickerState::palette_notice`) and its view went with it, and a
// drop that lands on no group is an ordinary cancel like a drop on the header.

/// How far the pointer must travel before a press becomes a DRAG (DRAGON-682 item 35).
///
/// The same 4pt `src/widgets/drag_area.rs` has used since the toolbar was made draggable,
/// and for the same reason: it is under a hand's natural wobble on a click, so a plain
/// press-release still reads as a click, and it is small enough that a deliberate drag feels
/// immediate.
pub const DRAG_THRESHOLD: f32 = 4.0;

/// **Pure**, unit-tested: has this press travelled far enough to be a drag?
///
/// `origin` is where the pointer was when the press was seen and `at` is where it is now.
/// Distance, not per-axis, so a diagonal drag needs no further travel than a straight one.
pub fn drag_is_live(origin: (f32, f32), at: (f32, f32)) -> bool {
    (at.0 - origin.0).hypot(at.1 - origin.1) > DRAG_THRESHOLD
}

// **Tombstone: `drag_source(hovered_harmony, hovered_recent, hovered_swatch)`**, deleted by
// DRAGON-682 item 41, and the reason is a toolkit trap worth stating once.
//
// It resolved WHICH source a press picked up by reading the window's hover bookkeeping,
// because a press event carries no target. That was wrong, and it produced all three of the
// bugs the owner reported in one go: a drag could be started anywhere in the window, the
// ghost showed a colour that was not the one under the pointer, and dropping appeared to do
// nothing (the stale source's own drop was a no-op: loading the recent that was already
// loaded, or filing the active colour the history already held).
//
// WHY the hover flags go stale: `iced::widget::mouse_area` CAPTURES the `CursorMoved` event
// on the frame the pointer enters it (`mouse_area.rs`, the `is_out_of_bounds` block:
// `capture_event()` then `return`), and every `mouse_area` runs
// `if shell.is_event_captured() { return; }` BEFORE its own hover logic. The `Shell` is
// shared by the whole widget tree for one event, so one mouse_area being entered silences
// every other one's `on_exit` for that frame. With more than one hover-tracking mouse_area in
// a window, "the pointer left me" is simply not reliable.
//
// The fix is structural: the press is published BY the widget that was pressed, carrying its
// own identity (`ColorPickerMsg::DragPressed(DragSource)`), so nothing has to be remembered
// between events and no press anywhere else in the window can arm anything. Do not
// reintroduce hover-derived arming.

/// **Pure**, unit-tested: is this release OUTSIDE the window entirely?
///
/// The pointer keeps reporting while a button is held (the platform's implicit grab), so a
/// release past the frame is knowable, and for a history entry it MEANS something (item 38:
/// drag one off the window to forget it). Everything else treats it as a cancel.
pub fn off_window(at: (f32, f32), window: (f32, f32)) -> bool {
    at.0 < 0.0 || at.1 < 0.0 || at.0 >= window.0 || at.1 >= window.1
}

/// What the drop machine has to know about the PANEL to hit-test it (DRAGON-687).
///
/// Plain data, handed in by the caller, so every decision below stays a function: the
/// panel's content SCROLLS, which is the one thing DRAGON-682's `palette: bool` could not
/// express, and a hit test that did not know the offset would name the wrong group the
/// moment the list scrolled at all.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct PanelShape {
    /// The panel is mounted AND showing Saved Palettes: the only state in which the panel
    /// half takes any drop.
    pub palettes: bool,
    /// The panel scrollable's current vertical offset, in points, as the window's own
    /// mirror of the widget's truth (`ColorPickerState::panel_scroll_y`).
    pub scroll: f32,
    /// How many colours each saved palette holds, in display order. The LENGTH is the
    /// group count; the per-group counts place the reorder slots.
    pub groups: Vec<usize>,
}

/// **Pure**, unit-tested: which zone a point is in, in WINDOW coordinates (DRAGON-682 items
/// 35 and 39, per-group since DRAGON-687).
///
/// `window` is the surface's real size and `shape` the panel's facts ([`PanelShape`]).
/// Everything this does not name, the header, the tab strip, the create row, the frosted
/// border and the panel while Harmonies is showing, answers `None`, which is a cancel.
///
/// The zones are stated in the layout's OWN terms rather than as numbers: the picker column
/// is [`picker_column_w`] wide inside the border, the divider row ([`divider_band_top`]) is
/// the line between its two halves, and a palette group's block is
/// [`palette_group_offset`] into the scrolled content.
pub fn drop_zone(at: (f32, f32), window: (f32, f32), shape: &PanelShape) -> Option<DropZone> {
    let (x, y) = at;
    let bottom = window.1 - WINDOW_BORDER;
    if y < WINDOW_BORDER + header_h() || y >= bottom {
        return None;
    }
    let column_right = WINDOW_BORDER + picker_column_w();
    if x >= WINDOW_BORDER && x < column_right {
        return Some(if y < divider_band_top() { DropZone::Main } else { DropZone::Recents });
    }
    // The PANEL half. Only the palettes tab's SCROLL AREA takes a drop: the tab strip and
    // the create row above it are not where anything lands (item 39 retired the
    // hover-activation that used to make the strip meaningful mid-drag).
    if !shape.palettes
        || x < column_right
        || x >= window.0 - WINDOW_BORDER
        || y < palettes_scroll_top()
        || y >= panel_scroll_bottom(window.1)
    {
        return None;
    }
    // Which group's BLOCK (heading plus bar) the point is in, through the scroll offset.
    // The gaps between blocks, and the run-out below the last one, are the strip: a group
    // is a target only when the pointer is really over it.
    let rel = y - palettes_scroll_top() + shape.scroll;
    let pitch = palette_group_h() + PANEL_GROUP_GAP;
    if rel >= 0.0 && !shape.groups.is_empty() {
        let g = (rel / pitch).floor() as usize;
        if g < shape.groups.len() && rel - g as f32 * pitch < palette_group_h() {
            return Some(DropZone::PaletteGroup(g));
        }
    }
    Some(DropZone::PaletteStrip)
}

/// **Pure**, unit-tested: THE drop matrix (DRAGON-682 item 38, the owner's own table, with
/// DRAGON-687's palette rows and columns).
///
/// | source | Main | Recents | PaletteGroup(g) | PaletteStrip | off the window |
/// |---|---|---|---|---|---|
/// | harmony swatch | set active | file it | append to g | cancel | cancel |
/// | the active swatch | cancel | file it | append to g | cancel | cancel |
/// | a history entry | load it | cancel | append to g | cancel | forget it |
/// | a palette colour | set active, no file | file it | own g: reorder; other g: COPY | cancel | forget it |
/// | a palette NAME | cancel | cancel | reorder groups | reorder groups | ask to delete |
///
/// The cancels in the table are not oversights: dropping the ACTIVE colour on the tools
/// that already show it would do nothing visible, dropping a history entry back on the
/// history would duplicate or reorder it, and a COLOUR dropped in the strip's gaps names
/// no group, so guessing one would file it somewhere the user did not aim. A `None` here
/// means the drag ends having changed nothing, which is also what a release over the
/// header, the tab strip or the Harmonies tab does.
///
/// `at` rides along because three of the palette answers depend on WHERE inside the zone
/// the release landed: the reorder slots ([`palette_color_slot`], [`palette_group_slot`])
/// are positions, not zones, and resolving them here is what keeps the highlight, the
/// insertion line and the action one decision.
pub fn drop_action(
    source: DragSource,
    zone: Option<DropZone>,
    at: (f32, f32),
    shape: &PanelShape,
    off_window: bool,
) -> Option<DropAction> {
    if off_window {
        // Off the window: forget a history entry or a palette colour, ask about a group,
        // cancel everything else.
        return match source {
            DragSource::Recent(i) => Some(DropAction::RemoveRecent(i)),
            DragSource::PaletteSwatch(g, i) => {
                Some(DropAction::RemovePaletteColor { group: g, index: i })
            }
            DragSource::PaletteName(g) => Some(DropAction::DeleteGroupRequest(g)),
            _ => None,
        };
    }
    match (source, zone?) {
        // The palette half, first the NAME drag, whose target is a SLOT anywhere on the
        // strip (a group's own block included: reordering has to be able to aim between
        // any two neighbours, and every gap is next to a block).
        (DragSource::PaletteName(from), DropZone::PaletteGroup(_) | DropZone::PaletteStrip) => {
            let to = palette_group_slot(at, shape);
            // A slot that puts the group back where it already is changes nothing.
            (to != from && to != from + 1).then_some(DropAction::ReorderGroup { from, to })
        }
        // A colour of this very group: reorder along the width (the owner's "drag and
        // drop sortable along their width"). Anything else that carries a colour appends
        // at the END, never the middle (the owner's rule).
        (DragSource::PaletteSwatch(g, i), DropZone::PaletteGroup(to_g)) => {
            if g == to_g {
                let n = shape.groups.get(g).copied().unwrap_or(0);
                let to = palette_color_slot(at, n);
                (to != i && to != i + 1)
                    .then_some(DropAction::ReorderPaletteColor { group: g, from: i, to })
            } else {
                // COPY, the owner's reversal (see the variant): the menu's explicit
                // "Move to palette" is the vacating form.
                Some(DropAction::CopyToPalette { from: (g, i), to: to_g })
            }
        }
        (
            DragSource::Harmony(..) | DragSource::Active | DragSource::Recent(_),
            DropZone::PaletteGroup(g),
        ) => Some(DropAction::AppendToPalette(g)),
        (_, DropZone::PaletteStrip) => None,
        // The picker column, unchanged from item 38 for the three original sources.
        (DragSource::Harmony(..), DropZone::Main) => Some(DropAction::SetActive),
        (DragSource::Harmony(..), DropZone::Recents) => Some(DropAction::AddToRecents),
        (DragSource::Active, DropZone::Recents) => Some(DropAction::AddToRecents),
        (DragSource::Active, DropZone::Main) => None,
        (DragSource::Recent(i), DropZone::Main) => Some(DropAction::LoadRecent(i)),
        (DragSource::Recent(_), DropZone::Recents) => None,
        // A palette colour on the picker column (DRAGON-687, the owner: "onto the main
        // tool window ... or the recents panel"). Taking a SAVED colour back is a load,
        // not a derivation, so it files nothing (see [`DropAction::SetActiveNoFile`]).
        (DragSource::PaletteSwatch(..), DropZone::Main) => Some(DropAction::SetActiveNoFile),
        (DragSource::PaletteSwatch(..), DropZone::Recents) => Some(DropAction::AddToRecents),
        (DragSource::PaletteName(_), DropZone::Main | DropZone::Recents) => None,
    }
}

/// **Pure**, unit-tested: is this a source the window really has right now (DRAGON-682 item
/// 41)?
///
/// The press carries its own identity, so this is not "what did the pointer find"; it is the
/// sanity check that the identity still names something. A history index past the end (an
/// entry removed between the press and its handling), or a harmony swatch while the panel is
/// not even mounted, cannot arm a drag.
///
/// **What is NOT here, deliberately**: any rule about where in the window the press landed.
/// Only three widgets publish a press ([`DragSource`]'s three), so a press on the SV square,
/// either strip, a value box, an empty history slot, the panel's background or the window's
/// own chrome produces no message at all and reaches nothing. That is a property of the view,
/// not a decision this function could make; `drag_arming_tests` states it in the one place a
/// test can.
pub fn arms_drag(
    source: DragSource,
    recents: usize,
    panel_mounted: bool,
    shape: &PanelShape,
) -> bool {
    match source {
        DragSource::Active => true,
        DragSource::Recent(i) => i < recents,
        DragSource::Harmony(..) => panel_mounted,
        // The palette sources exist only while the palettes tab is on screen
        // (`PanelShape::palettes` carries the mounted-and-showing pair), and only while
        // the identity still names something: a group or colour removed between the press
        // and its handling arms nothing (DRAGON-687, item 41's own rule).
        DragSource::PaletteSwatch(g, i) => {
            shape.palettes && shape.groups.get(g).is_some_and(|n| i < *n)
        }
        DragSource::PaletteName(g) => shape.palettes && g < shape.groups.len(),
    }
}

/// **Pure**, unit-tested: does this release complete a CLICK on `source` (DRAGON-682 item
/// 41)?
///
/// `drag` is the machine's state at the moment of the release: the source that was pressed
/// and whether it ever became a drag. A click is the same swatch pressed and released with no
/// travel in between, which is exactly what a button does, and it is what still loads a
/// history entry now that the entry is no longer a button (a button with an `on_press`
/// captures the press, and the drag has to see it).
///
/// The two `false` cases are the ones that matter: a release that ends a real DRAG is not a
/// click (or dropping a swatch back where it started would also load it), and a release over
/// a swatch whose press happened somewhere else is not a click either (the press belongs to
/// whatever the pointer was on).
pub fn completes_click(drag: Option<(DragSource, bool)>, source: DragSource) -> bool {
    matches!(drag, Some((pressed, live)) if !live && pressed == source)
}

/// **Pure**, unit-tested: does this widget-level release DISARM the machine (DRAGON-687,
/// the lost-release fix's belt-and-braces half)?
///
/// Yes for a machine that is ARMED BUT NOT LIVE, whatever was pressed and wherever the
/// release landed: the button is up, so a press that never became a drag is over, full
/// stop. No for a LIVE drag, whose release is the DROP and belongs to `DragReleased`
/// alone (a widget handler ending it would race the drop dispatch), and no for no
/// machine at all.
///
/// The ordering contract at every call site: the click decision ([`completes_click`])
/// is read FIRST, against the machine's state at the release, then this disarms, then
/// the click applies. One release, both effects, one order, so the disarm can never
/// eat the apply. The primary net is the always-on window-level release watcher
/// (`sub_picker_release_watch`); this half exists so even a release that somehow never
/// reaches that stream still cannot leave the machine armed under an idle pointer.
pub fn release_disarms(drag: Option<(DragSource, bool)>) -> bool {
    drag.is_some_and(|(_, live)| !live)
}

/// **Pure**, unit-tested: the rectangle a drop zone occupies, as `(x, y, w, h)` in window
/// coordinates (DRAGON-682 item 41; per-group and scroll-aware since DRAGON-687).
///
/// The same regions [`drop_zone`] hit-tests, stated once as rectangles so the HIGHLIGHT
/// and the hit-test cannot describe different shapes. `zone_round_trips_its_rect` pins
/// that: every rectangle's own middle hit-tests back to the zone it came from.
///
/// A palette GROUP's rectangle is its BAR row (the part a dropped colour actually joins),
/// clipped to the scroll viewport so a half-scrolled group's highlight ends at the fold
/// instead of overpainting the create row or the window padding.
pub fn zone_rect(zone: DropZone, window: (f32, f32), shape: &PanelShape) -> (f32, f32, f32, f32) {
    let top = WINDOW_BORDER + header_h();
    let bottom = window.1 - WINDOW_BORDER;
    let column_right = WINDOW_BORDER + picker_column_w();
    match zone {
        DropZone::Main => (
            WINDOW_BORDER,
            top,
            picker_column_w(),
            divider_band_top() - top,
        ),
        DropZone::Recents => (
            WINDOW_BORDER,
            divider_band_top(),
            picker_column_w(),
            bottom - divider_band_top(),
        ),
        DropZone::PaletteGroup(g) => {
            // The WHOLE group block, title row and bar as one rect (the owner's ask,
            // and the alignment fix's other half): the rect is the same
            // `palette_group_offset` + `palette_group_h` the layout and the hit test
            // are built from, with NO interior offset re-derived between them, so the
            // overlay and the group cannot drift again. Clipped to the scroll viewport
            // as every group rect is.
            let (view_top, view_bottom) = (palettes_scroll_top(), panel_scroll_bottom(window.1));
            let top = view_top - shape.scroll + palette_group_offset(g);
            let y = top.max(view_top);
            let h = (top + palette_group_h()).min(view_bottom) - y;
            (column_right + WINDOW_PADDING, y, card_w(), h.max(0.0))
        }
        DropZone::PaletteStrip => {
            let y = palettes_scroll_top();
            (
                column_right,
                y,
                (window.0 - WINDOW_BORDER - column_right).max(0.0),
                (panel_scroll_bottom(window.1) - y).max(0.0),
            )
        }
    }
}

/// **Pure**, unit-tested: the zone to LIGHT UP right now, or `None` (DRAGON-682 item 41).
///
/// One zone at a time, the one the pointer is in, and only if a drop there would really do
/// something for THIS source. It asks [`drop_action`] rather than restating the matrix, so
/// **the highlight and the action are the same decision**: a zone that lights and then does
/// nothing, or does something without lighting, is impossible by construction.
///
/// The one nuance DRAGON-687 adds: a REORDER whose slot happens to be the no-move slot
/// answers `None` from the action, so the light follows it off for those few pixels too,
/// which is honest (releasing there really would change nothing).
pub fn zone_highlight(
    source: DragSource,
    at: (f32, f32),
    window: (f32, f32),
    shape: &PanelShape,
) -> Option<DropZone> {
    let zone = drop_zone(at, window, shape)?;
    drop_action(source, Some(zone), at, shape, false).map(|_| zone)
}

/// The drop-zone highlight's corner radius and stroke (DRAGON-682 item 41).
///
/// Fixed numbers rather than theme tokens: this is a transient overlay drawn around a
/// REGION, not a control, so the rounding setting has nothing to say about it, and a stroke
/// thin enough to be tasteful on a swatch disappears around a 388pt block.
pub const ZONE_HIGHLIGHT_RADIUS: f32 = 8.0;
/// See [`ZONE_HIGHLIGHT_RADIUS`].
pub const ZONE_HIGHLIGHT_STROKE: f32 = 2.0;
/// How much of the accent the highlight's FILL carries. Half-transparent, the owner's word:
/// enough to name the region, not enough to hide what is in it.
pub const ZONE_HIGHLIGHT_FILL_ALPHA: f32 = 0.18;

/// The GHOST's size: a history swatch, because that is what the owner asked to see under the
/// pointer ("a small swatch that is the same shape as the swatches in the recent history
/// area"), whatever it was dragged from.
pub const DRAG_GHOST: f32 = RECENT_SWATCH;

/// **Pure**, unit-tested: where the ghost's top-left goes for a pointer at `at`.
///
/// Centred on the pointer, and NOT clamped to the window: the ghost is drawn inside our own
/// surface, so it clips at the frame on its own, and clamping it would make it lie about
/// where the drop is going to land as the pointer leaves.
pub fn ghost_origin(at: (f32, f32)) -> (f32, f32) {
    (at.0 - DRAG_GHOST / 2.0, at.1 - DRAG_GHOST / 2.0)
}

/// Pure, unit-tested: the width the PANEL gets when the window is expanded.
///
/// **The window DOUBLES** (the owner: "hitting the expand button should double the width
/// of the window"), so the panel is whatever the second half is: the base window's own
/// width, less the one window border the frosted container already spends on that side.
/// Doubling is only meaningful if the second half is the size of the first, and this is
/// that statement written as arithmetic rather than as a second constant to keep in step.
pub fn panel_w() -> f32 {
    color_window_size_expanded().0 - 2.0 * WINDOW_BORDER - picker_column_w()
}
/// The gap under the tab strip, before the scrolling content.
///
/// There is no strip HEIGHT here: `tab_bar` sizes itself, and the panel is the one part of
/// this window whose content scrolls, so nothing needs to predict it. (A budget stood here
/// while the strip was hand-built out of fixed-height buttons; the strip is the toolkit's
/// own since item 12, and it measures itself.)
pub const PANEL_TAB_GAP: f32 = 12.0;
// **Tombstone: `PANEL_GROUP_GAP = 16.0`** (DRAGON-682, retired by DRAGON-687's spacing
// round). The settings window's section gap was borrowed while the groups wore its card
// dress; with five uncarded harmony groups it left "a LOT of extra space after the
// monochromatic group" (the owner) as one dead block at the bottom. The gap CONSTANT was worked out from this fill exercise
// ([`PANEL_GROUP_GAP`]): the Harmonies tab's leftover height divided evenly into its
// four seams, and that value is the shared rhythm BOTH panels space by.

/// Pure, unit-tested: the height the panel's scrolling viewport really has on the
/// HARMONIES tab (no create row above it), in points: what [`PANEL_GROUP_GAP`] divides.
pub fn harmonies_viewport_h() -> f32 {
    panel_scroll_bottom(color_window_size().1) - panel_content_top()
}

/// THE inter-group gap both panels space by: one chosen NUMBER, not a runtime
/// derivation (the owner's correction to DRAGON-687's spacing round; the first cut
/// derived this live from the viewport each call, and that was wrong).
///
/// How the number was worked out, ONCE: the Harmonies tab's viewport height less its
/// five groups, split evenly over the four seams (36 whole points at the current
/// stack), less the owner's two-point tightening. The fill relationship the exercise
/// established is asserted in `the_gap_constant_fills_the_harmonies_viewport`, so a
/// future change to the stack or the window surfaces there instead of silently
/// reflowing. Harmonies fills its content area with no scroll and two trailing points
/// per seam of slack; Saved Palettes uses the same value as its fixed rhythm and
/// scrolls whenever its variable content overflows.
pub const PANEL_GROUP_GAP: f32 = 34.0;
const _: () = assert!(
    PANEL_GROUP_GAP >= PANEL_HEADING_GAP,
    "a group gap tighter than the heading gap would read as one run-on block (DRAGON-687)"
);

pub const PANEL_HEADING_GAP: f32 = 8.0;
/// The group heading's explainer icon, and the air between it and the name (DRAGON-682
/// item 23). Smaller than the heading's own text: it is a footnote mark, not a control.
pub const PANEL_HINT_ICON: u16 = 14;
pub const PANEL_HINT_GAP: f32 = 6.0;
/// The padding inside a harmony CARD.
pub const PANEL_CARD_PAD: f32 = 12.0;
/// The gap the panel's scrolled content leaves on its RIGHT for the scrollbar (DRAGON-682
/// item 16: the owner's rows ran underneath it).
///
/// The settings window reserves the same space the same way, by padding the SCROLLED
/// CONTENT rather than by shrinking the scrollable: its pages sit inside a 24pt inset on
/// both sides. This panel is much narrower than a settings page, so it spends 12 on the
/// scrollbar's side and nothing on the other, and the TAB STRIP takes the same right inset
/// so the strip and the cards share one right edge.
pub const PANEL_SCROLLBAR_GAP: f32 = 12.0;

/// Pure, unit-tested: the width a harmony CARD spans, which is the panel's content less the
/// scrollbar's reserved gap.
pub fn card_w() -> f32 {
    panel_content_w() - PANEL_SCROLLBAR_GAP
}

/// Pure, unit-tested: the width the SEGMENTS of one bar share.
///
/// The whole CARD since DRAGON-682 item 27, because a harmony group has no card padding to
/// give up any more: the owner took the settings-card dress off these groups, so the bar
/// spans from the panel's content edge to the scrollbar's lane and nothing insets it.
/// [`PANEL_CARD_PAD`] survives for a group that IS carded (see `view::harmony_group`'s
/// `carded` parameter, which Saved Palettes may want when it has content).
pub fn bar_w() -> f32 {
    card_w()
}

/// **Pure**, unit-tested: the width of EACH segment in a bar of `n`, left to right
/// (DRAGON-682 item 17).
///
/// **The bar is FULL WIDTH whatever subdivides it** (the owner: "it should be full width no
/// matter how many segments subdivide it"), so this is [`value_box_widths`]'s trick in a
/// second place and for the same reason: the floor leaves a remainder, and dropping it would
/// end the bar short of the card's right edge. It is handed out a point at a time from the
/// left, so no two segments differ by more than one point and the bar lands flush.
pub fn segment_widths(n: usize) -> Vec<f32> {
    segment_widths_in(bar_w(), n)
}

/// The arithmetic of [`segment_widths`] over an arbitrary total, so the Saved Palettes
/// bars (which give up room for their plus button, DRAGON-687) divide their own width by
/// the same rule and land flush the same way.
pub fn segment_widths_in(total: f32, n: usize) -> Vec<f32> {
    let n = n.max(1);
    let base = (total / n as f32).floor();
    let mut spare = (total - base * n as f32).floor().max(0.0) as usize;
    (0..n)
        .map(|_| {
            if spare > 0 {
                spare -= 1;
                base + 1.0
            } else {
                base
            }
        })
        .collect()
}

/// **Pure**, unit-tested: which of a segment's two OUTER corner pairs are rounded, as
/// `[first, last]` (DRAGON-682 item 17).
///
/// Only the bar's ends round: the first segment's left corners and the last segment's right
/// ones. A ONE-segment bar is both, which is the case that makes this a function rather than
/// two comparisons at the call site.
pub fn segment_corners(index: usize, n: usize) -> [bool; 2] {
    [index == 0, index + 1 >= n.max(1)]
}

/// The MOST segments any bar holds, which is the widest harmony card
/// ([`harmony_card_lengths`]'s maximum). It exists so a menu's placement can be clamped
/// without the caller carrying its own row length around.
pub const MAX_SEGMENTS: usize = 5;

/// Pure, unit-tested: the x of segment `index`'s left edge inside its CARD, for placing that
/// segment's context menu.
pub fn segment_x(index: usize, n: usize) -> f32 {
    segment_widths(n).into_iter().take(index).sum::<f32>()
}
/// One harmony swatch's height, which IS a history swatch's (DRAGON-682 item 21, the
/// owner: "the height of the color swatch rows should be the height of our recent history
/// swatch colors").
///
/// Read from [`RECENT_SWATCH`] rather than restated, so the two can never drift: they are
/// the same kind of thing in two places, and the window is small enough that a two-point
/// difference between them reads as a mistake.
pub const PANEL_SWATCH: f32 = RECENT_SWATCH;
/// A harmony group's HEADING line, budgeted at the settings window's own heading height.
///
/// Budgeted rather than measured, and it only feeds [`harmony_group_offset`]: the panel
/// scrolls, so nothing here has to be exact for the layout to be right, and a scroll that
/// lands a few points off still puts the card on screen.
pub const PANEL_HEADING_H: f32 = 21.0;

/// Pure, unit-tested: the y offset of harmony group `index` inside the scrolling panel,
/// for scrolling a keyboard cursor into view (DRAGON-682 item 9).
///
/// Every group is the same height because every card is one row of swatches: a heading, the
/// gap under it, and a card of one [`PANEL_SWATCH`] inside its padding. If a card ever grows
/// a second row this becomes a sum over the groups above rather than a multiplication, and
/// the scroll would land short until it does.
pub fn harmony_group_offset(index: usize) -> f32 {
    index as f32 * (harmony_group_h() + PANEL_GROUP_GAP)
}

/// Pure, unit-tested: ONE harmony group's height: its heading, the gap under it, and the
/// bar. No card padding since DRAGON-682 item 27 took the card dress off these groups.
pub fn harmony_group_h() -> f32 {
    PANEL_HEADING_H + PANEL_HEADING_GAP + PANEL_SWATCH
}

/// Pure, unit-tested: the whole Harmonies tab's content height, for checking it against the
/// panel's own viewport (DRAGON-682 item 27: "maybe that tab doesn't scroll as a result").
///
/// The groups plus the gaps between them. It is a FIT CHECK, not a layout input: the tab is
/// inside a scrollable either way, because this window is a fixed size and a sixth group
/// must be reachable rather than clipped. (It was test-only until the scroll memory made
/// [`harmonies_max_scroll`] a live reader.)
pub fn harmony_content_h() -> f32 {
    let n = crate::color::Harmony::ALL.len() as f32;
    n * harmony_group_h() + (n - 1.0) * PANEL_GROUP_GAP
}

// **Tombstone: `panel_viewport_h`**, the APPROXIMATE viewport (a generous 60pt tab-strip
// allowance), retired by DRAGON-687's spacing round. It existed to answer a yes/no fit
// question where erring small only made the test stricter; the derived group gap FILLS
// the viewport exactly, which an under-estimate reads as overflow, so the derivation and
// its tests use the exact [`harmonies_viewport_h`] instead (the strip's height is exact
// too: `tab_bar` takes precisely its configured `button_height`, [`PANEL_TAB_STRIP_H`]).

/// The expand/collapse toggle's button, on the picker column's right edge.
///
/// The settings window's nav toggle is the model (`settings::mod`'s `toggle_nav`), and
/// this is its mirror: same 16pt symbolic glyph, same "the icon names the action" rule,
/// sized here to the picker's own bare icon buttons rather than to the settings rail's
/// 44x36 column, which is a width this window has no equivalent of.
pub const PANEL_TOGGLE_W: f32 = 24.0;
pub const PANEL_TOGGLE_ICON: u16 = 16;

/// Windows-with-native-captions only (DRAGON-685): where the FLOATING toggle sits, in
/// LOGICAL points, both measured against the live DWM cluster at 100% scale and meant to
/// be re-measured, not derived, if the cluster ever moves.
///
/// The header's end region cannot reach this spot (its spacing chain parked the icon
/// ~25pt short of the cluster and ~8pt below its glyphs), so the view floats the button in
/// its own stack layer instead; `view.rs`'s layer block does the border arithmetic.
///
/// * The GAP keeps the button's box clear of `DWMWA_CAPTION_BUTTON_BOUNDS`: the caption
///   subclass hands every message to `DwmDefWindowProc` first, so a button overlapping the
///   bounds would have its clicks answered as caption clicks before iced ever saw them.
/// * The CENTERLINE is the native glyphs' own, from the window's top edge (the top frame
///   is flush, DRAGON-284), so the toggle reads as one row with minimize and close.
///
/// Both are read ONLY from `view.rs`'s `#[cfg(windows)]` floating-toggle layer, so both are
/// honestly dead off Windows. They stay in this file, unguarded by `cfg`, because this is
/// the window's measurement sheet and a number that only exists on one platform is still a
/// number the next person needs to find here. They were added by DRAGON-685 without the
/// attribute and warned on every Linux and macOS build until DRAGON-686: the Windows
/// cross-check runs clippy against the msvc target, where these are LIVE, so nothing in the
/// Windows workflow could have caught it.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WIN_TOGGLE_CLUSTER_GAP: f32 = 6.0;
#[cfg_attr(not(windows), allow(dead_code))]
pub const WIN_TOGGLE_CENTERLINE: f32 = 15.0;

/// Pure, unit-tested: the colour-picker window's size when the panel is OPEN.
///
/// Exactly twice the base WIDTH and the same height, which is the owner's spec in one
/// line. Stated as a function over [`color_window_size`] rather than as its own sum so the
/// two can never drift: every part the base window grows, the expanded one grows with.
pub fn color_window_size_expanded() -> (f32, f32) {
    let (w, h) = color_window_size();
    (w * 2.0, h)
}

/// Pure, unit-tested: the window's size for an expansion STATE. The one function every
/// caller asks, so no call site has to branch on the flag itself.
pub fn color_window_size_for(expanded: bool) -> (f32, f32) {
    if expanded { color_window_size_expanded() } else { color_window_size() }
}

/// Which tab the panel is showing (DRAGON-682). PERSISTED, like the value row's notation:
/// the picker is a one-shot process, so an in-memory tab would reset on every launch.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PanelTab {
    /// Colour relationships calculated from the current colour: the harmony cards.
    ///
    /// It was called Compare for one commit, which is the word the owner's first brief
    /// used; they renamed it to Harmonies before anything shipped, so there is no migration
    /// here and the persisted id moved with the name.
    #[default]
    Harmonies,
    /// Saved palettes. A placeholder for now, by the owner's staging.
    Palettes,
}

/// The tab strip's ICONS, one per tab (DRAGON-682 item 12, the owner's own picks): a
/// dashed circle with a dot for the relationships, and the swatch book the tray's palette
/// entry already wears.
const TAB_ICONS: [&str; 2] = ["color-harmonies-symbolic", "color-palettes-symbolic"];

impl PanelTab {
    pub const ALL: [Self; 2] = [Self::Harmonies, Self::Palettes];

    /// The tab's own label, as the strip prints it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Harmonies => "Harmonies",
            // The owner's wording (DRAGON-682 item 12). The id stays the short one: a
            // label is user copy and a persisted id is not, and this is exactly the split
            // that lets one move without the other.
            Self::Palettes => "Saved Palettes",
        }
    }

    /// The tab's ICON name, for [`crate::widgets::icons::handle`].
    pub fn icon(self) -> &'static str {
        TAB_ICONS[match self {
            Self::Harmonies => 0,
            Self::Palettes => 1,
        }]
    }

    /// A fresh `segmented_button` model carrying every tab, in panel order, each with its
    /// label, its icon and its own [`PanelTab`] as data (DRAGON-682 item 12).
    ///
    /// The DATA is what makes the widget's activation legible: `on_activate` hands back an
    /// `Entity`, and the handler reads the tab straight off it rather than mapping indices,
    /// so inserting a third tab later cannot silently renumber the other two.
    ///
    /// Built the way the settings window builds its own strips (`settings::mod`'s
    /// `general_tab` / `capture_tab`), down to `handle(name).icon()`.
    pub fn model() -> cosmic::widget::segmented_button::SingleSelectModel {
        let mut model = cosmic::widget::segmented_button::SingleSelectModel::default();
        for tab in Self::ALL {
            model
                .insert()
                .text(tab.label())
                .icon(crate::widgets::icons::handle(tab.icon()).icon())
                .data(tab);
        }
        let first = model.iter().next();
        if let Some(first) = first {
            model.activate(first);
        }
        model
    }

    /// This tab's position in [`Self::ALL`]: the index its session state (the per-tab
    /// scroll memory) lives at. A method rather than `position()` at the call sites, so
    /// a third tab cannot be added without this answering for it.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    /// A stable identifier, which is what the config stores. Never user-facing, so the
    /// label can be renamed without invalidating anyone's saved tab.
    pub fn id(self) -> &'static str {
        match self {
            Self::Harmonies => "harmonies",
            Self::Palettes => "palettes",
        }
    }

    /// **Pure**, unit-tested: the tab whose [`Self::id`] this is, for reading the persisted
    /// value back. Anything unrecognised is [`Self::Harmonies`], the tab that has content.
    pub fn from_id(id: &str) -> Self {
        Self::ALL.into_iter().find(|t| t.id() == id).unwrap_or_default()
    }
}

/// Pure, unit-tested: the colour-picker window's size in LOGICAL POINTS. Its ONLY size.
///
/// **The window is exactly this and cannot be resized** (`min_size == max_size`, see
/// [`super::open_color_picker_window`], DRAGON-587): whatever the layout needs, the
/// window is, and there is no user resize to absorb a mistake.
///
/// **Width**: [`WINDOW_BORDER`] + `16` padding, on each side, + [`CONTENT_W`] =
/// **422pt** (366 before DRAGON-680's item 23 put a 48pt copy button at the head of the
/// value row, and 510 back when that row was one line carrying the mode chip and both icon
/// buttons beside the boxes). Still under the original "about half the permission window,
/// best judgement" brief (the permission window is 629). What sets it is the VALUE ROW
/// again, for the first time since DRAGON-676: see [`CONTENT_W`] for the two constraints
/// that pick the exact number.
///
/// **Height** is the sum of the parts: the frame + header + padding + the square + the
/// wide gap + the controls row + the wide gap + the value row (TWO bands since DRAGON-680
/// deleted the mode row: the boxes, then their captions) + gap + the divider band + gap +
/// the two history rows with their grid gap between them + padding. The history gap is
/// [`recents_gap`] in BOTH directions (the owner's rev-3 ask), which is also why the
/// height must be a function of it: under-counting it is exactly how the second row
/// got clamped short once.
///
/// **582pt before DRAGON-680, 542 mid-ticket, 563 now.** The first 40 came off when the
/// mode row was deleted ([`VALUE_BOX_H`] + [`BOX_GAP`], given back as the dropdown became a
/// chevron unit BESIDE the boxes and the split toggle went). Item 23 then added 21 of it
/// back, in two unrelated pieces that are worth separating: 14 is the box band growing to
/// hold a 48pt copy button ([`BOX_BAND_H`]), and 7 is the history's own grid gap widening
/// with the window ([`recents_gap`], 10pt to 17pt at the new [`CONTENT_W`]). Nothing else
/// in the stack moved.
pub fn color_window_size() -> (f32, f32) {
    let w = 2.0 * (WINDOW_BORDER + WINDOW_PADDING) + CONTENT_W;
    let h = 2.0 * WINDOW_BORDER
        + header_h()
        + 2.0 * WINDOW_PADDING
        + SV_H
        + GAP_SQUARE_CONTROLS
        + CONTROLS_H
        + GAP_CONTROLS_VALUE
        + VALUE_ROW_H
        + GAP_VALUE_DIVIDER
        + DIVIDER_BAND_H
        + SECTION_GAP
        + 2.0 * RECENT_SWATCH
        + recents_gap()
        + LAYOUT_SLACK_H;
    (w, h)
}

// ── Saved palettes (DRAGON-687) ──────────────────────────────────────────────
//
// The Saved Palettes tab's whole pure layer: the group model, every mutation a gesture can
// make (append, reorder, move, copy, rename, delete, sort), the tab's own layout numbers,
// the drop machine's palette hit-testing, and the drag auto-scroll. The view and the
// handlers only feed these numbers in and apply the answers, which is this file's one rule.

/// One saved palette at runtime: a name and an ordered colour list, each colour with its
/// alpha ([`Recent`] is the same value a history entry is, on purpose: the two lists hold
/// the same kind of thing and must persist the same spelling).
#[derive(Clone, PartialEq, Debug)]
pub struct Palette {
    /// The user's own name for the group, edited inline. User content, never logged.
    pub name: String,
    /// The colours, in the user's own order. Every add APPENDS (the owner's rule), and
    /// only an intra-group drag reorders.
    pub colors: Vec<Recent>,
}

impl Palette {
    pub fn new(name: String) -> Self {
        Self { name, colors: Vec::new() }
    }
}

/// **Pure**, unit-tested: the name a freshly created palette gets: `Palette 1`,
/// `Palette 2`, …, the first number no existing group already wears (DRAGON-687).
///
/// Numbered from the EXISTING NAMES rather than from the count, so creating, deleting the
/// first and creating again does not mint a second "Palette 2". The comparison is exact:
/// a group the user renamed does not reserve the number it started with.
pub fn default_palette_name(existing: &[Palette]) -> String {
    let mut n = 1usize;
    loop {
        let name = format!("Palette {n}");
        if !existing.iter().any(|p| p.name == name) {
            return name;
        }
        n += 1;
    }
}

/// **Pure**, unit-tested: THE duplicate rule, in one predicate (DRAGON-687, made the
/// literal single definition by the duplicate-guard audit): may `entry` join `colors`?
///
/// Exact equality, colour AND alpha ([`Recent`]'s own derived equality over the bytes):
/// the same colour at another transparency is another value, the history's settled rule.
/// EVERY path that puts a colour into a palette consults this, through
/// [`palette_append`] (the plus button, the pipette delivery, every drop-append, the
/// Add/Copy-to-palette submenus, and [`palette_move_color`]'s insertion half) or through
/// [`palette_from_saved`] (the LOAD, which was the audit's finding: a duplicate already
/// in the file bypassed every interactive guard and was re-displayed and re-saved
/// forever). A reorder is deliberately NOT an insertion and never asks.
pub fn palette_admits(colors: &[Recent], entry: Recent) -> bool {
    !colors.contains(&entry)
}

/// **Pure**, unit-tested: `palettes` with a freshly created group PREPENDED at the TOP
/// (DRAGON-687's drag-scroll round; create appended until the owner's correction: "when
/// we add a new palette, it should be added at the top by default and not the bottom").
/// The name is [`default_palette_name`]'s first free number, unchanged; the caller
/// scrolls the tab to the top so the new group's pre-selected rename is visible, and the
/// persisted order is exactly this order.
pub fn palettes_with_new(palettes: &[Palette]) -> Vec<Palette> {
    let mut out = Vec::with_capacity(palettes.len() + 1);
    out.push(Palette::new(default_palette_name(palettes)));
    out.extend(palettes.iter().cloned());
    out
}

/// **Pure**, unit-tested: `palettes` with `entry` APPENDED at the END of group `group`
/// (DRAGON-687).
///
/// `None` when nothing changed, which is the no-save signal every mutation here shares:
/// a group index that names nothing, or a colour (and alpha) the group already holds
/// ([`palette_admits`], the one duplicate rule). The duplicate rule is the plus button's
/// spec ("add our current swatch color to this palette if it isn't already added")
/// applied to EVERY add path, drops included, so one gesture cannot fill a palette with
/// copies another gesture refuses to make.
pub fn palette_append(palettes: &[Palette], group: usize, entry: Recent) -> Option<Vec<Palette>> {
    let target = palettes.get(group)?;
    if !palette_admits(&target.colors, entry) {
        return None;
    }
    let mut out = palettes.to_vec();
    out[group].colors.push(entry);
    Some(out)
}

/// **Pure**, unit-tested: a palette built from PERSISTED entries, deduplicated under the
/// one rule (DRAGON-687's duplicate-guard audit).
///
/// The LOAD is an insertion path too, and it was the one that bypassed the guard: a
/// byte-equal pair already in `palettes.toml` (a hand edit, an interrupted write, any
/// artifact of the unreleased config era) loaded verbatim, showed as the duplicate every
/// interactive path refuses to create, and was faithfully re-saved on every mutation, so
/// no guard could ever heal it. This is the fix at the boundary: the FIRST occurrence
/// wins (it is where the user put the colour), every later equal entry drops, order is
/// otherwise untouched, and the next save rewrites the file clean.
pub fn palette_from_saved(name: String, entries: impl IntoIterator<Item = Recent>) -> Palette {
    let mut colors: Vec<Recent> = Vec::new();
    for entry in entries {
        if palette_admits(&colors, entry) {
            colors.push(entry);
        }
    }
    Palette { name, colors }
}

/// **Pure**, unit-tested: `palettes` with colour `index` of group `group` removed.
/// `None` for an identity that names nothing (a drop resolved after the list changed).
pub fn palette_remove_color(
    palettes: &[Palette],
    group: usize,
    index: usize,
) -> Option<Vec<Palette>> {
    palettes.get(group)?.colors.get(index)?;
    let mut out = palettes.to_vec();
    out[group].colors.remove(index);
    Some(out)
}

/// **Pure**, unit-tested: one list with the element at `from` re-inserted at slot `to`,
/// where `to` is an INSERTION slot in the ORIGINAL order (`0..=len`).
///
/// The shared arithmetic of both reorders (a colour along its bar, a group down the
/// strip), kept in one place so the two cannot disagree about what a slot means. `None`
/// for out-of-range input and for the two slots that put the element back where it was.
fn reorder<T: Clone>(list: &[T], from: usize, to: usize) -> Option<Vec<T>> {
    if from >= list.len() || to > list.len() || to == from || to == from + 1 {
        return None;
    }
    let mut out = list.to_vec();
    let item = out.remove(from);
    // Removing `from` shifts every later slot left by one.
    let at = if to > from { to - 1 } else { to };
    out.insert(at, item);
    Some(out)
}

/// **Pure**, unit-tested: reorder a colour WITHIN its group (the owner's "drag and drop
/// sortable along their width"). `to` is an insertion slot in the group's original order.
pub fn palette_reorder_color(
    palettes: &[Palette],
    group: usize,
    from: usize,
    to: usize,
) -> Option<Vec<Palette>> {
    let colors = reorder(&palettes.get(group)?.colors, from, to)?;
    let mut out = palettes.to_vec();
    out[group].colors = colors;
    Some(out)
}

/// **Pure**, unit-tested: MOVE a colour to another group, appended at its END
/// (DRAGON-687). The removal always happens; the append is skipped when the target
/// already holds the colour, because the user's ask ("move it there") is already true
/// then and a duplicate would say otherwise.
///
/// The insertion half is [`palette_append`]'s, not a second copy of the rule (the
/// duplicate-guard audit's consolidation): this function decides only the removal and
/// the still-a-change-when-the-append-declines semantics.
pub fn palette_move_color(
    palettes: &[Palette],
    from: (usize, usize),
    to_group: usize,
) -> Option<Vec<Palette>> {
    let (g, i) = from;
    if g == to_group {
        return None;
    }
    palettes.get(g)?.colors.get(i)?;
    palettes.get(to_group)?;
    let mut out = palettes.to_vec();
    let entry = out[g].colors.remove(i);
    // The ONE duplicate decision: a declined append is still a completed move (the
    // removal happened), which is why this unwraps to the removed-only list rather
    // than answering `None`.
    Some(palette_append(&out, to_group, entry).unwrap_or(out))
}

/// **Pure**, unit-tested: COPY a colour to another group, appended at its END. `None`
/// when the target already holds it (nothing would change, so nothing is saved).
pub fn palette_copy_color(
    palettes: &[Palette],
    from: (usize, usize),
    to_group: usize,
) -> Option<Vec<Palette>> {
    let (g, i) = from;
    if g == to_group {
        return None;
    }
    let entry = *palettes.get(g)?.colors.get(i)?;
    palette_append(palettes, to_group, entry)
}

/// **Pure**, unit-tested: rename group `group` to `name`, TRIMMED (DRAGON-687).
///
/// `None` when nothing changes: an index that names nothing, a name that is already the
/// group's, or a name that trims to NOTHING. The empty case keeps the old name on
/// purpose: an inline editor committed empty is far more often a slip than a wish for a
/// nameless group, and a group with no visible name has no handle left to click.
pub fn palette_rename(palettes: &[Palette], group: usize, name: &str) -> Option<Vec<Palette>> {
    let name = name.trim();
    if name.is_empty() || palettes.get(group)?.name == name {
        return None;
    }
    let mut out = palettes.to_vec();
    out[group].name = name.to_string();
    Some(out)
}

/// **Pure**, unit-tested: `palettes` without group `group`. The CONFIRMED half of the
/// two delete gestures; the confirmation itself is the window's dialog, not this.
pub fn palette_delete(palettes: &[Palette], group: usize) -> Option<Vec<Palette>> {
    if group >= palettes.len() {
        return None;
    }
    let mut out = palettes.to_vec();
    out.remove(group);
    Some(out)
}

/// **Pure**, unit-tested: reorder the GROUPS: the name-drag drop. `to` is an insertion
/// slot in the original group order, from [`palette_group_slot`].
pub fn palette_reorder_group(
    palettes: &[Palette],
    from: usize,
    to: usize,
) -> Option<Vec<Palette>> {
    reorder(palettes, from, to)
}

/// The six sorts the group menu's "Sort palettes" submenu offers (DRAGON-687). The
/// variants carry the FORMULAS' names (ascending in the named quantity, reverse
/// descending); the user-facing labels and the menu order live in [`Self::label`] and
/// [`Self::ALL`], reworked by the owner's intuitive-language follow-up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteSort {
    Alphabetical,
    AlphabeticalReverse,
    Luminance,
    LuminanceReverse,
    CoolToWarm,
    WarmToCool,
}

impl PaletteSort {
    /// MENU order, not declaration order (DRAGON-687 follow-up, the owner: make the
    /// submenu "intuitive in language and behavior"): three pairs, each pair together,
    /// most-used first (name, then brightness, then temperature), and within a pair the
    /// end a user pictures first leads (A first, lightest first, warmest first).
    pub const ALL: [Self; 6] = [
        Self::Alphabetical,
        Self::AlphabeticalReverse,
        Self::LuminanceReverse,
        Self::Luminance,
        Self::WarmToCool,
        Self::CoolToWarm,
    ];

    /// The menu row's label: what the sort DOES, in the words a user would say, so nobody
    /// needs the spec to predict the outcome (the same follow-up; the first cut said
    /// "Luminance (reverse)", which is the formula's name, not the result's). Each label
    /// states the resulting ORDER outright, which also says the action is a one-shot
    /// rearrangement rather than a sticky mode. The variant names keep the formulas'
    /// vocabulary; only the user-facing words moved.
    pub fn label(self) -> &'static str {
        match self {
            Self::Alphabetical => "Name (A to Z)",
            Self::AlphabeticalReverse => "Name (Z to A)",
            Self::Luminance => "Darkest first",
            Self::LuminanceReverse => "Lightest first",
            Self::CoolToWarm => "Coolest first",
            Self::WarmToCool => "Warmest first",
        }
    }
}

/// **Pure**, unit-tested: a group's brightness for the luminance sorts: the MEAN of its
/// colours' WCAG relative luminance ([`Srgb::relative_luminance`], the colour model's own
/// measure, the one the picker already sorts text ink by). `None` for an empty group,
/// which has no brightness to speak of and sorts LAST (see [`sort_palettes`]).
///
/// The alpha deliberately does not weigh in: a palette entry's transparency is part of
/// the VALUE it copies out, not of how bright the swatch reads over the checkerboard, and
/// any compositing answer would have to invent a backdrop to composite against.
pub fn group_luminance(p: &Palette) -> Option<f64> {
    if p.colors.is_empty() {
        return None;
    }
    let sum: f64 = p.colors.iter().map(|c| c.color.relative_luminance()).sum();
    Some(sum / p.colors.len() as f64)
}

/// Where WARMTH peaks on the hue wheel, in degrees: 45 is orange, midway between red and
/// yellow, the textbook centre of the warm arc; its antipode 225 is azure, the centre of
/// the cool arc.
const WARM_PEAK_DEG: f64 = 45.0;

/// **Pure**, unit-tested: a group's warmth for the cool/warm sorts: the MEAN of each
/// colour's `saturation * cos(hue - 45°)`, hue and saturation in HSV
/// ([`crate::color::srgb_to_hsv`], the model this window already thinks in).
///
/// The formula, spelled out because "the average ... in the way that matters" is the
/// owner's requirement:
///
/// * the COSINE projects the circular hue onto one warm-to-cool axis, +1 at orange (45°)
///   through 0 at the two neutral crossings (135° spring green, 315° magenta-violet) to
///   -1 at azure (225°). Averaging raw hue DEGREES would be circular-mean nonsense: two
///   reds at 10° and 350° would average to cyan;
/// * the SATURATION weight is what keeps a near-grey from swinging the answer: an
///   achromatic colour has no hue, so it contributes zero warmth rather than a random
///   direction, and a vivid colour outweighs a washed one in proportion to how much hue
///   it actually shows. Value does not weigh in: a dark red is still warm.
///
/// `None` for an empty group, which sorts LAST like the luminance sorts' empty case.
pub fn group_warmth(p: &Palette) -> Option<f64> {
    if p.colors.is_empty() {
        return None;
    }
    let sum: f64 = p
        .colors
        .iter()
        .map(|c| {
            let hsv = crate::color::srgb_to_hsv(c.color);
            hsv[1] * (hsv[0] - WARM_PEAK_DEG).to_radians().cos()
        })
        .sum();
    Some(sum / p.colors.len() as f64)
}

/// **Pure**, unit-tested: the groups sorted by `sort` (DRAGON-687).
///
/// The rules, stated once:
///
/// * every sort is STABLE (groups that compare equal keep their current order), so
///   re-applying a sort is a no-op rather than a shuffle;
/// * "Alphabetical" is byte order on the name, ascending, which is the repo's prior art
///   for a key sort (`covermark_prefs`); the base direction of every sort is ASCENDING in
///   the named quantity ("Luminance" runs dark to light, "Cool to Warm" cool to warm),
///   and the reverse spellings descend;
/// * a group with NO colours has no luminance and no warmth, so the quantity sorts put
///   the empty groups LAST, in both directions, keeping their relative order. Reversing a
///   sort reverses the ANSWERED groups, not the unanswerable ones.
pub fn sort_palettes(palettes: &[Palette], sort: PaletteSort) -> Vec<Palette> {
    let mut out = palettes.to_vec();
    // A key sort over Option<f64>: None (an empty group) ranks after every Some in both
    // directions, and the direction only flips the answered values.
    let by_quantity = |out: &mut Vec<Palette>, key: fn(&Palette) -> Option<f64>, desc: bool| {
        out.sort_by(|a, b| match (key(a), key(b)) {
            (Some(x), Some(y)) => {
                let ord = x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
                if desc { ord.reverse() } else { ord }
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
    };
    match sort {
        PaletteSort::Alphabetical => out.sort_by(|a, b| a.name.cmp(&b.name)),
        PaletteSort::AlphabeticalReverse => out.sort_by(|a, b| b.name.cmp(&a.name)),
        PaletteSort::Luminance => by_quantity(&mut out, group_luminance, false),
        PaletteSort::LuminanceReverse => by_quantity(&mut out, group_luminance, true),
        PaletteSort::CoolToWarm => by_quantity(&mut out, group_warmth, false),
        PaletteSort::WarmToCool => by_quantity(&mut out, group_warmth, true),
    }
    out
}

// ── The Saved Palettes tab's layout ──────────────────────────────────────────

/// The create row's height: the "New Palette" button's own compact height, budgeted like
/// [`PANEL_HEADING_H`] is (the row is above the scrollable, so only
/// [`palettes_scroll_top`] reads it).
pub const PALETTE_CREATE_ROW_H: f32 = 32.0;
/// The gap between the title row's two icon buttons, and between them and the title.
pub const PALETTE_PLUS_GAP: f32 = 8.0;
/// The expanded search FIELD's width (item six): room for a real palette name beside
/// the sort icon without crowding the "New Palette" button off its top-right home
/// (`create_palette_row` composes sort icon + gap + this + fill + button inside
/// [`card_w`]-ish content).
pub const PALETTE_SEARCH_W: f32 = 180.0;

// **Tombstone: `palette_bar_w` / `palette_segment_widths` / `palette_segment_x` /
// `palette_menu_dx`** (DRAGON-687, retired by its UX round). The bars gave up width for
// the plus button, then for the pipette beside it ("shrink the palettes to make room if
// necessary"), and the owner then moved both icons up into the TITLE ROW so the palettes
// could breathe: a palette bar is FULL card width again, exactly a harmony bar, so the
// four palette-specific twins collapsed back into [`bar_w`], [`segment_widths`],
// [`segment_x`] and [`harmony_menu_dx`], and their flush-fit tests went with them (the
// shared fns carry their own).

/// **Pure**, unit-tested: which group a palette-destined pick's target snapshot names NOW
/// (DRAGON-687 follow-up), given where the group WAS (`index`) and what it was CALLED
/// (`name`) when the pipette launched the pick.
///
/// The pick is its own process and the list can move while it is out, so the snapshot is
/// resolved in two steps: the exact position first (nothing moved, the common case), then
/// the NAME anywhere in the list (the group was re-sorted or re-ordered; the first match
/// wins where two groups share a name, and the index fast path keeps the snapshot's own
/// group winning whenever it is still where it was). `None` when neither answers, which
/// is a group deleted, or renamed while the pick was out: the caller degrades to the
/// ordinary pick delivery rather than filing into whichever group drifted under the
/// index, and rename-while-picking losing the shortcut is the recorded compromise.
pub fn resolve_palette_target(palettes: &[Palette], index: usize, name: &str) -> Option<usize> {
    if palettes.get(index).is_some_and(|p| p.name == name) {
        return Some(index);
    }
    palettes.iter().position(|p| p.name == name)
}

/// A saved palette's TITLE ROW height (DRAGON-687's UX round): the pipette and plus sit
/// IN the title row now, right-aligned, and they are history-swatch squares, so the row
/// is a button tall and the title text centres against it.
pub const PALETTE_TITLE_ROW_H: f32 = RECENT_SWATCH;

/// Pure, unit-tested: ONE palette group's height: the title row (which carries the two
/// icon buttons since the UX round, so it is [`PALETTE_TITLE_ROW_H`] rather than the
/// harmony groups' text-only [`PANEL_HEADING_H`]), the gap under it, and the bar.
///
/// It matched [`harmony_group_h`] exactly until the icons moved up; the two tabs have
/// their OWN heights now and every consumer (the drop machine, the cursor's
/// scroll-into-view, the anchors) picks per tab rather than assuming parity.
pub fn palette_group_h() -> f32 {
    PALETTE_TITLE_ROW_H + PANEL_HEADING_GAP + PANEL_SWATCH
}

/// Pure, unit-tested: the width the title TEXT may occupy before it ellipsizes: the card
/// less the right-aligned icon pair and their gaps, less the hover pencil's own room, so
/// a long name truncates instead of pushing the buttons off the row.
pub fn palette_title_w() -> f32 {
    card_w()
        - 2.0 * (RECENT_SWATCH + PALETTE_PLUS_GAP)
        - (f32::from(PANEL_HINT_ICON) + PANEL_HINT_GAP)
}

/// The palette title's text size, as a BUDGET for [`palette_title_truncates`] (the view
/// draws `text::heading`, whose face this crate cannot measure headlessly; the mode
/// menu's measure-the-worst-case treatment applies, headroom included).
pub const PALETTE_TITLE_TEXT_SIZE: f32 = 14.0;

/// **Pure**, unit-tested: does this palette name TRUNCATE in its title row (DRAGON-687's
/// UX round)? Decides whether the title carries the full-name tooltip: a tooltip on
/// every short name would be noise, and one missing from a truncated name would leave
/// the ellipsis unanswerable. Measured through the embedded face with the mode labels'
/// own measure-versus-draw headroom, so the answer errs toward OFFERING the tooltip.
pub fn palette_title_truncates(name: &str) -> bool {
    crate::app::preview::text_annot::measure(
        crate::app::preview::text_annot::TextFont::Clean,
        PALETTE_TITLE_TEXT_SIZE,
        name,
    ) * MODE_LABEL_HEADROOM
        > palette_title_w()
}

/// Pure, unit-tested: the y offset of palette group `index` inside the scrolled content
/// ([`harmony_group_offset`]'s twin, and the drop machine's inverse map).
pub fn palette_group_offset(index: usize) -> f32 {
    index as f32 * (palette_group_h() + PANEL_GROUP_GAP)
}

/// Pure, unit-tested: the palettes tab's whole content height, for the auto-scroll clamp.
/// Zero for no groups.
///
/// The trailing [`PANEL_GROUP_GAP`] is the owner's symmetry ask (the drag-jump round's
/// item four): "padding below the last palette equal to the padding above it", the
/// existing constant reused rather than a new number. The view's matching term is the
/// palettes tab's bottom content padding in `side_panel`; every extent consumer (the
/// clamps, the spacer arithmetic, the pins) derives from HERE, item nine's one-source
/// rule, so the pad exists exactly twice: once as layout, once as arithmetic, both
/// spelled with the one constant.
pub fn palettes_content_h(groups: usize) -> f32 {
    if groups == 0 {
        return 0.0;
    }
    groups as f32 * palette_group_h() + groups as f32 * PANEL_GROUP_GAP
}

/// The y of the panel content's top edge, under the tab strip, in window coordinates:
/// where the HARMONIES scrollable starts, and the base the palettes tab builds on.
fn panel_content_top() -> f32 {
    WINDOW_BORDER + header_h() + WINDOW_PADDING + PANEL_TAB_STRIP_H + PANEL_TAB_GAP
}

/// Pure, unit-tested: the y of the SAVED PALETTES scrollable's top edge, in window
/// coordinates: the panel content's top plus the create row pinned above the scrollable
/// (the row scrolls with nothing, so "New Palette" is reachable however long the list).
pub fn palettes_scroll_top() -> f32 {
    panel_content_top() + PALETTE_CREATE_ROW_H + PANEL_TAB_GAP
}

/// Pure, unit-tested: the y of the panel scrollable's BOTTOM edge, in window coordinates:
/// the window's own padding is all that sits below it.
pub fn panel_scroll_bottom(window_h: f32) -> f32 {
    window_h - WINDOW_BORDER - WINDOW_PADDING
}

/// Pure, unit-tested: the harmonies tab's largest legal offset. ZERO since the spacing
/// round made the five groups fill their viewport exactly, and derived rather than
/// hard-coded so a sixth harmony would make the memory real instead of wrong.
pub fn harmonies_max_scroll() -> f32 {
    (harmony_content_h() - harmonies_viewport_h()).max(0.0)
}

/// Pure, unit-tested: a TAB's largest legal offset, for restoring its remembered scroll.
pub fn panel_max_scroll_for(tab: PanelTab, window_h: f32, groups: usize) -> f32 {
    match tab {
        PanelTab::Harmonies => harmonies_max_scroll(),
        PanelTab::Palettes => palettes_max_scroll(window_h, groups),
    }
}

/// **Pure**, unit-tested: which palette TITLE the pointer is on, from ONE panel-level
/// position in the scrolled content's OWN coordinates (DRAGON-687's UX round, the
/// stranded-pencil fix).
///
/// The pencil used to ride a per-title `on_enter`/`on_exit` pair, which is the exact
/// fragility this file already tombstoned at the old `drag_source`: `mouse_area`
/// captures the `CursorMoved` that enters it and every other area early-returns on a
/// captured event, so one area's enter can starve another's exit and strand its hover
/// flag forever, which is what the owner saw ("the edit pencil remains as if i never
/// stopped hovering"). Visibility is DERIVED now: one panel-level `on_move` reports the
/// position every frame it moves (level-triggered, so a starved frame heals on the
/// next), and this rect check answers from that one source of truth.
///
/// The rect is the TITLE side of the row: the title row's height at the group's own
/// offset, LEFT of the right-aligned icon pair (the pipette and plus say what they are
/// on their own hover; the pencil is the text's affordance). Content-local coordinates,
/// so the scroll offset is the widget's own problem and never this function's.
pub fn hovered_palette_title(pointer: Option<(f32, f32)>, groups: usize) -> Option<usize> {
    let (x, y) = pointer?;
    if x < 0.0 || x >= card_w() - 2.0 * (RECENT_SWATCH + PALETTE_PLUS_GAP) {
        return None;
    }
    let pitch = palette_group_h() + PANEL_GROUP_GAP;
    if y < 0.0 {
        return None;
    }
    let g = (y / pitch).floor() as usize;
    (g < groups && y - g as f32 * pitch < PALETTE_TITLE_ROW_H).then_some(g)
}

/// **Pure**, unit-tested: [`hovered_palette_title`] from the WINDOW-LEVEL pointer
/// (DRAGON-687, the pencil's second stranding).
///
/// The first fix derived the pencil from a position reported by the SCROLLED CONTENT's
/// own area, and the owner's follow-up repro names the hole in that design exactly:
/// moving UP off the FIRST title leaves that reporting region entirely (into the create
/// row, the tab strip, the header), so no further report ever arrives and the last
/// position, inside the title rect, stays current forever. A level-triggered source only
/// self-heals while reports keep coming; a REGION exit starves it exactly as the old
/// edge events did. So the source is the whole WINDOW now, where motion is reported
/// wherever the pointer is, and this function does the mapping the region used to do by
/// construction:
///
/// * anything not over the palettes tab at all (`palettes_showing` false) is no title;
/// * anything outside the scroll VIEWPORT is no title, and the check is against the
///   viewport's WINDOW edges before the scroll offset is applied, which is the pinned
///   failing case: a pointer in the create row above a SCROLLED list would otherwise
///   map to `-20 + scroll`, a positive content y that can land inside a title rect;
/// * inside it, the window point maps into the scrolled content's own space
///   (subtract the panel origin, add the scroll mirror) and the one existing rect check
///   answers, unchanged and already pinned.
pub fn hovered_palette_title_at(
    pointer: Option<(f32, f32)>,
    window: (f32, f32),
    scroll: f32,
    groups: usize,
    palettes_showing: bool,
) -> Option<usize> {
    if !palettes_showing {
        return None;
    }
    let (x, y) = pointer?;
    if y < palettes_scroll_top() || y >= panel_scroll_bottom(window.1) {
        return None;
    }
    hovered_palette_title(
        Some((x - panel_content_left(), y - palettes_scroll_top() + scroll)),
        groups,
    )
}

/// **Pure**, unit-tested: THE tab-switch scroll exchange (DRAGON-687's UX round, the
/// owner: "the palette tab should remember where we scrolled to when activating").
///
/// `mem` is the per-tab remembered offsets (indexed by [`PanelTab::index`]), `live` the
/// current tab's live offset (the drop machine's hit-test mirror). Switching STORES the
/// live offset into `from`'s slot and RESTORES `to`'s remembered one, CLAMPED into
/// `to_max` (groups can be created, deleted or re-sorted while the other tab was
/// showing, and a stale offset must land at the nearest valid position rather than
/// strand the mirror past the end). Returns the updated memory and the restored live
/// offset; the caller issues the real scroll command with exactly that value, so the
/// widget and the mirror move together and the desync the old reset-to-top guarded
/// against stays impossible.
///
/// Both DIRECTIONS of the transient drag switch ride this same exchange: switching to
/// Saved Palettes as a drag goes live restores the palettes offset (the auto-scroll then
/// moves the live value as the drag needs), and the revert stores wherever the drag left
/// it, so the next visit resumes there.
pub fn scroll_exchange(
    mut mem: [f32; 2],
    live: f32,
    from: PanelTab,
    to: PanelTab,
    to_max: f32,
) -> ([f32; 2], f32) {
    // A switch to the tab ALREADY showing is a STRUCTURAL no-op: no store, no restore,
    // and above all no clamp (DRAGON-687's drag-scroll round). The live offset is the
    // widget's own truth; "restoring" it through our clamp would move the tab by
    // whatever our max under-estimates the widget's real one, which is exactly the
    // "moves up some pixels" class of bug this tab must never show. The callers all
    // guard the same way; this is the guard that holds when a future one forgets.
    if from == to {
        return (mem, live);
    }
    mem[from.index()] = live;
    let restored = mem[to.index()].clamp(0.0, to_max.max(0.0));
    mem[to.index()] = restored;
    (mem, restored)
}

/// Pure, unit-tested: the panel scrollable's largest legal offset on the palettes tab:
/// content height less the viewport, floored at zero. The auto-scroll clamps against
/// this, so it stops AT the end instead of asking the widget for travel that does not
/// exist (which is what would jitter).
pub fn palettes_max_scroll(window_h: f32, groups: usize) -> f32 {
    (palettes_content_h(groups) - (panel_scroll_bottom(window_h) - palettes_scroll_top()))
        .max(0.0)
}

/// **Pure**, unit-tested: the insertion SLOT (`0..=n`) a drop at window-x `at_x` means,
/// within a palette bar of `n` colours (DRAGON-687).
///
/// The boundary between "before segment i" and "after it" is the segment's MIDDLE, so the
/// nearest seam wins, which is how every drag-to-reorder list feels. Left of the bar is
/// slot 0 and right of it slot `n`, so a drop that overshoots the bar's end still appends
/// rather than cancelling.
pub fn palette_color_slot(at: (f32, f32), n: usize) -> usize {
    let bar_left = WINDOW_BORDER + picker_column_w() + WINDOW_PADDING;
    let x = at.0 - bar_left;
    let widths = segment_widths(n);
    let mut edge = 0.0f32;
    for (i, w) in widths.iter().enumerate().take(n) {
        if x < edge + w / 2.0 {
            return i;
        }
        edge += w;
    }
    n
}

/// **Pure**, unit-tested: the insertion SLOT (`0..=groups`) a NAME drop at window-y means,
/// down the palettes strip. The boundary is each group block's MIDDLE, through the scroll
/// offset, so the nearest seam wins vertically exactly as [`palette_color_slot`]'s does
/// horizontally.
pub fn palette_group_slot(at: (f32, f32), shape: &PanelShape) -> usize {
    let rel = at.1 - palettes_scroll_top() + shape.scroll;
    let n = shape.groups.len();
    let pitch = palette_group_h() + PANEL_GROUP_GAP;
    for i in 0..n {
        if rel < i as f32 * pitch + palette_group_h() / 2.0 {
            return i;
        }
    }
    n
}

/// **Pure**, unit-tested: where the vertical insertion LINE for a colour reorder draws,
/// as an x inside the bar: the left edge of slot `slot` (the bar's own right edge for the
/// append slot).
pub fn palette_insert_line_x(slot: usize, n: usize) -> f32 {
    segment_x(slot.min(n), n.max(1))
}

/// **Pure**, unit-tested: where the horizontal insertion LINE for a group reorder draws,
/// as a y inside the scrolled content: the middle of the gap above slot `slot` (half a
/// group gap above the block it would land before; the content's own edges at the ends).
pub fn palette_group_line_y(slot: usize, groups: usize) -> f32 {
    if slot == 0 {
        return 0.0;
    }
    if slot >= groups {
        // Mid-way into the trailing pad (the item-four symmetry gave the content a
        // last gap), so the end slot's line sits mid-gap exactly as the interior ones.
        return palettes_content_h(groups) - PANEL_GROUP_GAP / 2.0;
    }
    palette_group_offset(slot) - PANEL_GROUP_GAP / 2.0
}

// ── Filtering and virtualizing the palette list (DRAGON-687 items six and eight) ──

/// **Pure**, unit-tested: the groups the search shows, as REAL indices in display order
/// (item six). Case-insensitive substring on the name; a blank or whitespace query shows
/// everything. The view, the drop zones, the keyboard grid and the scroll extents all
/// operate on THIS list's layout while a filter is active, so a hidden group is not a
/// drop target and not a cursor stop, which is the natural reading of a filtered list.
pub fn visible_palettes(palettes: &[Palette], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..palettes.len()).collect();
    }
    palettes
        .iter()
        .enumerate()
        .filter(|(_, p)| p.name.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

/// **Pure**, unit-tested: a name-drag's insertion SLOT over the visible rows, mapped
/// into the FULL list: inserting before the real group that anchors that visible slot,
/// or at the very end past the last visible row. Deterministic with hidden groups
/// interleaved, which is what makes group reordering meaningful under a filter at all.
pub fn visible_slot_to_real(visible: &[usize], slot: usize, total: usize) -> usize {
    visible.get(slot).copied().unwrap_or(total)
}

/// How many off-screen rows the virtualized list builds on EACH side of the viewport
/// (item eight). Three: one row absorbs the fractional row at each edge, and two more
/// cover a fast wheel flick's travel between the on_scroll report and the next view, so
/// ordinary scrolling never shows an unbuilt row. The cost of a too-big buffer is just
/// widgets, so this errs small; the correctness lives in the spacers, not the buffer.
pub const VIRTUAL_ROW_BUFFER: usize = 3;

/// The built slice of a virtualized palette list, plus the spacers standing in for
/// everything outside it (item eight).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RowWindow {
    /// The first visible ROW to build, inclusive.
    pub first: usize,
    /// One past the last row to build.
    pub last: usize,
    /// The TOP spacer's height, `None` when the window starts at row zero. The value
    /// accounts for the column's own inter-child spacing: the spacer plus the gap the
    /// column inserts after it lands the first built row at exactly `first * pitch`.
    pub top: Option<f32>,
    /// The BOTTOM spacer's height, `None` when the window reaches the end; same
    /// gap-accounting on its side.
    pub bottom: Option<f32>,
}

/// **Pure**, unit-tested: which rows of a `rows`-long palette list to BUILD for a
/// viewport of `viewport_h` at `scroll` (item eight, the owner's ten-thousand-palettes
/// case).
///
/// The DATA was never the problem (ten thousand name-and-hex groups are small and load
/// once); the WIDGET TREE was, since the panel rebuilds per frame and ten thousand rows
/// of bars and buttons would swamp layout. The pitch is fixed, so the intersecting range
/// is O(1) arithmetic, padded by [`VIRTUAL_ROW_BUFFER`] rows each side; everything else
/// in this file already computes from indices and pitch rather than from built widgets,
/// which is why the zones, the keyboard grid, the scroll-into-view and the auto-scroll
/// need no change beyond sharing the filtered index list.
///
/// The spacers keep the scrollbar honest: built or not, the content's total extent is
/// identical (`the_row_window_preserves_the_content_extent` pins it against
/// [`palettes_content_h`]), so the widget's own scroll range, and every offset this
/// file's mirror arithmetic produces, are unchanged by virtualization.
/// `keep` names one row that must be BUILT even when it is outside the viewport: the
/// open rename editor's. A widget that leaves the tree loses its focus and its caret,
/// so scrolling an open rename off screen would silently end the edit; keeping its row
/// built (one extra group, at worst) keeps the editor alive to scroll back to. The
/// window stays contiguous (the range stretches to reach the kept row), so the spacer
/// arithmetic is unchanged.
pub fn visible_row_window(
    rows: usize,
    scroll: f32,
    viewport_h: f32,
    keep: Option<usize>,
) -> RowWindow {
    if rows == 0 {
        return RowWindow { first: 0, last: 0, top: None, bottom: None };
    }
    let pitch = palette_group_h() + PANEL_GROUP_GAP;
    let scroll = scroll.max(0.0);
    let mut first = ((scroll / pitch).floor() as usize).saturating_sub(VIRTUAL_ROW_BUFFER);
    let mut last = ((((scroll + viewport_h.max(0.0)) / pitch).ceil() as usize)
        .saturating_add(VIRTUAL_ROW_BUFFER))
    .min(rows);
    if let Some(k) = keep
        && k < rows
    {
        first = first.min(k);
        last = last.max(k + 1);
    }
    let first = first.min(last);
    RowWindow {
        first,
        last,
        top: (first > 0).then_some(first as f32 * pitch - PANEL_GROUP_GAP),
        bottom: (last < rows).then_some((rows - last) as f32 * pitch - PANEL_GROUP_GAP),
    }
}

/// **Pure**, unit-tested: does this drop's ending COMMIT the transient Saved Palettes
/// activation (the drag-jump round's item five, the owner: "if we drop on a saved
/// palette from anywhere, we should stay on the saved palette tab instead of restoring
/// our tab")?
///
/// Exactly the two endings that put a colour INTO a saved palette group, from any
/// source: the append (harmony, recents, main swatch) and the cross-palette copy. The
/// user just filed something where they can now see it; snapping the tab away hides
/// the result of the gesture. Every other ending reverts as before: the loads and
/// recents drops land outside the panel, and the reorder/remove/delete endings can
/// only START from palette sources, which exist only while the palettes tab is already
/// showing, so there is no transient switch to commit there anyway. Committing also
/// PERSISTS the active tab, exactly as a real tab click would, so a relaunch agrees
/// with what the user sees (the handler's half).
pub fn drop_commits_palette_tab(action: DropAction) -> bool {
    matches!(action, DropAction::AppendToPalette(_) | DropAction::CopyToPalette { .. })
}

/// **Pure**, unit-tested: what size the dashed zone-outline raster must be for the live
/// zone rect, or `None` when the cached raster already has it (the drag-jump round's
/// item three).
///
/// The named stale key: the raster cache was keyed on the ZONE'S IDENTITY
/// (`DropZone::PaletteGroup(g)`), and [`zone_rect`] clips a group's rect to the scroll
/// viewport, so a group scrolling into view KEPT its identity while its rect grew from
/// a sliver to full height. The cached outline stayed sliver-sized while the accent
/// wash (an analytic quad) tracked the live rect, which is the owner's "short but wide
/// dashed line area that doesn't match the highlight bg". The rect IS the key: the
/// outline's pixels are a function of nothing but its size (and the accent, fixed for
/// a drag's duration), so equal sizes reuse the raster, across identities included,
/// and a stale-size draw is impossible by construction.
pub fn zone_raster_size(cached: Option<(u32, u32)>, w: f32, h: f32) -> Option<(u32, u32)> {
    let want = (w.round().max(1.0) as u32, h.round().max(1.0) as u32);
    (cached != Some(want)).then_some(want)
}

// ── Drag auto-scroll (DRAGON-687, the owner's addendum) ──────────────────────

/// How close to the scroll viewport's edge, in points, a live drag starts auto-scrolling.
///
/// A little over one group heading tall: deep enough that aiming at the topmost visible
/// bar does not brush it, shallow enough that "push toward the edge" is an obvious
/// gesture.
pub const AUTOSCROLL_BAND: f32 = 36.0;
/// The fastest the auto-scroll travels, in points per second, at the band's very edge.
///
/// "Slowly" is the owner's word: 260 pt/s crosses one group block in a bit over a quarter
/// second, fast enough to traverse a long list without waiting, slow enough to release
/// over the group you wanted as it arrives.
pub const AUTOSCROLL_MAX_SPEED: f32 = 260.0;
/// The auto-scroll drive's tick, while it is running: the same 16ms cadence the pinch
/// poll uses. It exists only while a live drag sits in a band, so it keeps nothing awake
/// otherwise.
pub const AUTOSCROLL_TICK: std::time::Duration = std::time::Duration::from_millis(16);

/// **Pure**, unit-tested: the auto-scroll VELOCITY for a live drag with the pointer at
/// `at`, in points per second: negative scrolls up (toward the list's start), positive
/// down, zero means "do not scroll" (DRAGON-687, the owner: "slowly scroll up if we're
/// near the top and slowly scroll down if we're near the bottom").
///
/// Zero unless the pointer is horizontally over the PANEL half and vertically inside the
/// scroll viewport: hovering the picker column, the header or the tab strip must never
/// move the list. Inside a band the speed RAMPS linearly from zero at the band's inner
/// edge to [`AUTOSCROLL_MAX_SPEED`] at the viewport's edge, so grazing a band nudges and
/// leaning into it travels; a hard switch would jerk exactly at the moment the user is
/// aiming.
pub fn drag_autoscroll_velocity(at: (f32, f32), window: (f32, f32), shape: &PanelShape) -> f32 {
    if !shape.palettes {
        return 0.0;
    }
    let (x, y) = at;
    let column_right = WINDOW_BORDER + picker_column_w();
    if x < column_right || x >= window.0 - WINDOW_BORDER {
        return 0.0;
    }
    let (top, bottom) = (palettes_scroll_top(), panel_scroll_bottom(window.1));
    if y < top || y >= bottom {
        return 0.0;
    }
    if y < top + AUTOSCROLL_BAND {
        let t = (top + AUTOSCROLL_BAND - y) / AUTOSCROLL_BAND;
        return -AUTOSCROLL_MAX_SPEED * t.clamp(0.0, 1.0);
    }
    if y >= bottom - AUTOSCROLL_BAND {
        let t = (y - (bottom - AUTOSCROLL_BAND)) / AUTOSCROLL_BAND;
        return AUTOSCROLL_MAX_SPEED * t.clamp(0.0, 1.0);
    }
    0.0
}

/// **Pure**, unit-tested: is the drag's auto-scroll ARMED after sampling the pointer at
/// `at` (DRAGON-687's drag-scroll round)?
///
/// The owner's bug: grabbing the topmost visible group to reorder it STARTED the drag
/// with the pointer already inside the top band (the first title sits wholly within the
/// band's [`AUTOSCROLL_BAND`] points), so the auto-scroll engaged at the grab itself,
/// scrolling to the top under a stationary pointer, or a few pixels on a brief drag.
/// The contract is "MOVES a drag TO the edge", so the band must be ENTERED: the
/// auto-scroll arms only once some live sample has landed where the ramp answers zero
/// (outside both bands, or outside the panel and its viewport entirely, which is where
/// every picker-column drag begins), and stays armed for the rest of the drag. The
/// predicate is the velocity ramp's own zero, so the two can never disagree about where
/// a band starts.
pub fn autoscroll_arms(
    armed: bool,
    at: (f32, f32),
    window: (f32, f32),
    shape: &PanelShape,
) -> bool {
    armed || drag_autoscroll_velocity(at, window, shape) == 0.0
}

const _: () = assert!(
    AUTOSCROLL_BAND * 2.0 < 100.0,
    "DRAGON-687: the two auto-scroll bands must leave a dead middle in the shortest \
     viewport this window can show, or every hover position scrolls and nothing can be \
     aimed at"
);

// ── Ctrl+Tab cycling (DRAGON-687, the owner's second addendum) ───────────────

/// **Pure**, unit-tested: the panel tab a Ctrl+Tab (`forward`) or Ctrl+Shift+Tab lands
/// on, or `None` while the panel is not mounted (a chord aimed at tabs nobody can see
/// must do nothing, the same rule the focus ring's panel stop follows).
///
/// The walk is [`crate::keynav::step`] over [`PanelTab::ALL`], the same wrap every other
/// keyboard-cycled list in this app takes, and the settings window's strip cycling rides
/// the same `step` from its own handler so the two chords cannot drift.
pub fn panel_tab_after_cycle(
    current: PanelTab,
    forward: bool,
    mounted: bool,
) -> Option<PanelTab> {
    if !mounted {
        return None;
    }
    let at = PanelTab::ALL.iter().position(|t| *t == current);
    let next = crate::keynav::step(at, if forward { 1 } else { -1 }, PanelTab::ALL.len())?;
    Some(PanelTab::ALL[next])
}

// ── The palette menus (DRAGON-687) ───────────────────────────────────────────

/// The submenu entries' labels, and the two group-menu rows. ONE constant each, because
/// the panels are sized from the strings they draw (the notation menu's own rule). The
/// submenu rows are DRAWN with a trailing `›` ([`SUBMENU_MARK`]); the constants stay
/// bare so the sort/target PAGES can reuse the words as headings if they ever need to.
pub const ADD_TO_PALETTE_LABEL: &str = "Add to palette";
pub const MOVE_TO_PALETTE_LABEL: &str = "Move to palette";
pub const COPY_TO_PALETTE_LABEL: &str = "Copy to palette";
pub const DELETE_PALETTE_LABEL: &str = "Delete palette";
/// The palette swatch menu's removal row (DRAGON-687 follow-up): the same removal the
/// drag-off performs, by name, with no confirmation (colours never confirm; groups do).
pub const REMOVE_FROM_PALETTE_LABEL: &str = "Remove from palette";
/// "Sort palettes" since the intuitive-language pass (the spec's working name was "Sort
/// item groups", which is nobody's word for the things the tab calls palettes). Item six
/// moved the words from the group menus' submenu row to the toolbar sort ICON's tooltip;
/// same label, one home at a time.
pub const SORT_GROUPS_LABEL: &str = "Sort palettes";
/// What a row that OPENS a page wears after its label: the owner's `>` spelled as the
/// typographic single guillemet, matching how every desktop menu marks a submenu.
pub const SUBMENU_MARK: &str = " ›";

/// Which PAGE a swatch or group context menu is showing (DRAGON-687): the root rows, or
/// one of the four second-level lists. ONE field for every menu in the window
/// (`ColorPickerState::menu_page`), reset to Root whenever any menu opens, because only
/// one menu is ever open and a page that outlived its menu would open the next one deep.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MenuPage {
    #[default]
    Root,
    /// The palette list "Add to palette ›" opens (harmony and recents swatches).
    AddTo,
    /// The palette list "Move to palette ›" opens (palette swatches).
    MoveTo,
    /// The palette list "Copy to palette ›" opens (palette swatches).
    CopyTo,
    // `Sort` sat here: the six sorts as a group-title submenu. DRAGON-687 item six moved
    // the sort popup to the create row's own toolbar icon (the owner's ask), so the
    // group menus lost the submenu, kept their delete entry, and the page went with it.
}

/// **Pure**, unit-tested: the page a menu shows the moment it OPENS: always
/// [`MenuPage::Root`], whatever page any earlier menu was left on (DRAGON-687 follow-up,
/// the owner's report: "sometimes the next time i right click will show only the sub item
/// option instead of the initial right click menu").
///
/// The page state is ONE field shared by every menu, and some close paths never touch it
/// (a flyout dismissed by clicking away only reports the menu closing; Escape clears the
/// menus in `keyboard.rs`). Resetting at OPEN is the invariant that covers every close
/// path at once, past and future, rather than a hunt for each path that forgets; the
/// close-side resets that exist stay as hygiene, not as the guarantee. Every menu-opening
/// arm consults this instead of writing the field directly, so a new menu cannot skip it.
pub fn menu_page_on_open(prior: MenuPage) -> MenuPage {
    let _ = prior;
    MenuPage::Root
}

/// **Pure**, unit-tested: does a harmony or recents swatch's menu offer "Add to
/// palette ›" (DRAGON-687)? Exactly when any saved palette exists, the owner's gate.
pub fn offers_add_to_palette(palette_count: usize) -> bool {
    palette_count > 0
}

/// **Pure**, unit-tested: does a PALETTE swatch's menu offer "Move to palette ›" and
/// "Copy to palette ›"? Exactly when more than one saved palette exists (the owner's
/// gate: with one palette there is nowhere else to put a colour).
pub fn offers_move_copy_to_palette(palette_count: usize) -> bool {
    palette_count > 1
}

/// **Pure**, unit-tested: which groups a move/copy page LISTS as targets: every group
/// but the one the colour is already in (`exclude`), in display order. Moving or copying
/// a colour to its own group is a no-op dressed as an action, so the row never exists.
pub fn palette_targets(count: usize, exclude: Option<usize>) -> Vec<usize> {
    (0..count).filter(|g| Some(*g) != exclude).collect()
}

/// Pure, unit-tested: a menu panel's on-screen height for `rows` rows: the fixed
/// arithmetic [`mode_menu_panel_h`] and its siblings share, generalised because the
/// palette menus' row counts are data (how many palettes exist) rather than constants.
pub fn menu_panel_h_for(rows: usize) -> f32 {
    let n = rows.max(1) as f32;
    n * MODE_MENU_ITEM_H + (n - 1.0) * MODE_MENU_GAP + 2.0 * f32::from(MODE_MENU_PAD)
}

/// Pure, unit-tested: a menu panel's width for whatever rows it draws, measured the way
/// every fixed menu here is measured, then CAPPED at the panel's own content width: a
/// palette NAME is user text of any length, and a menu wider than the panel would clip
/// at the window edge where one ellipsized row does not.
pub fn menu_width_for_labels<'a>(labels: impl Iterator<Item = &'a str>) -> f32 {
    let widest = labels
        .map(|s| {
            crate::app::preview::text_annot::measure(
                crate::app::preview::text_annot::TextFont::Clean,
                MODE_LABEL_SIZE,
                s,
            )
        })
        .fold(0.0f32, f32::max);
    mode_menu_width_for(widest).min(panel_content_w())
}

// ── Fitting a page-swapping menu inside the window (DRAGON-687 follow-up) ────

/// The least air a fitted menu panel keeps from the window's frame, beyond the frame's
/// own [`WINDOW_BORDER`]: enough that the panel's outline never kisses the window edge.
pub const MENU_FIT_MARGIN: f32 = 4.0;

/// **Pure**, unit-tested: the popover `Point` offsets, relative to the anchor's top-left,
/// that keep a menu panel of `panel` size fully inside the window (DRAGON-687 follow-up,
/// the owner: "the extended item menus in right clicks can sometimes clip outside the
/// window").
///
/// The offsets were computed once at OPEN for the ROOT page's fixed size, which was sound
/// while every page WAS the root; the submenu pages made a menu's size DATA (a target
/// list grows with the palettes, the sort page is three times its root), so the placement
/// has to be re-derived from the CURRENT page's measured size on every page swap. This is
/// that one decision, shared by every page-swapping menu (recents, harmony, palette
/// swatches, group names):
///
/// * VERTICALLY the preferred direction is UP, bottom flush with the anchor's top,
///   exactly the historical `FlyoutDir::UpRight` placement, so every page that fits
///   upward is byte-identical to before. A page too tall for the room above FLIPS DOWN
///   (top flush with the anchor's bottom); one too tall for either direction SLIDES to
///   fit, pinned at the top when it is taller than the whole window band, because a menu
///   whose tail is reachable by moving the pointer beats one clipped at either edge;
/// * HORIZONTALLY the caller's own column rule says where the panel WANTS its left edge
///   ([`recents_menu_dx`] and its siblings, already page-aware), and this clamps that
///   wish into the window as the final net, so a wide page near the right edge slides
///   left instead of clipping.
///
/// `anchor` is the anchored widget's top-left in WINDOW coordinates and `anchor_h` its
/// height; the answer is `(x, y)` for `Position::Point`, both relative to that top-left.
pub fn menu_fit(
    anchor: (f32, f32),
    anchor_h: f32,
    desired_left: f32,
    panel: (f32, f32),
    window: (f32, f32),
) -> (f32, f32) {
    let m = WINDOW_BORDER + MENU_FIT_MARGIN;
    let left = desired_left.clamp(m, (window.0 - m - panel.0).max(m));
    let (top_bound, bottom_bound) = (m, window.1 - m);
    let top = if anchor.1 - panel.1 >= top_bound {
        // Room above: the historical upward placement, untouched.
        anchor.1 - panel.1
    } else if anchor.1 + anchor_h + panel.1 <= bottom_bound {
        // No room above, room below: flip down.
        anchor.1 + anchor_h
    } else {
        // Neither direction fits whole: slide the upward wish into the band, pinned at
        // the top when the panel is taller than the band itself.
        (anchor.1 - panel.1).clamp(top_bound, (bottom_bound - panel.1).max(top_bound))
    };
    (left - anchor.0, top - anchor.1)
}

/// Pure, unit-tested: a HISTORY swatch's top-left in window coordinates, for
/// [`menu_fit`]. The grid's top is the same stack [`color_window_size`] sums (the
/// focus-outset shuffle moves no sums, its own tests pin that), and the swatches march by
/// the grid's shared gap.
pub fn history_swatch_anchor(index: usize) -> (f32, f32) {
    let col = index % RECENTS_PER_ROW;
    let row = index / RECENTS_PER_ROW;
    (
        WINDOW_BORDER + WINDOW_PADDING + col as f32 * (RECENT_SWATCH + recents_gap()),
        divider_band_top()
            + DIVIDER_BAND_H
            + SECTION_GAP
            + row as f32 * (RECENT_SWATCH + recents_gap()),
    )
}

/// The x of the panel content's left edge, in window coordinates: where the cards, the
/// bars and the group headings all start.
fn panel_content_left() -> f32 {
    WINDOW_BORDER + picker_column_w() + WINDOW_PADDING
}

/// Pure, unit-tested: a HARMONY segment's top-left in window coordinates, through the
/// panel's scroll offset.
pub fn harmony_swatch_anchor(group: usize, seg: usize, n: usize, scroll: f32) -> (f32, f32) {
    (
        panel_content_left() + segment_x(seg, n),
        panel_content_top() + harmony_group_offset(group) + PANEL_HEADING_H + PANEL_HEADING_GAP
            - scroll,
    )
}

/// Pure, unit-tested: a PALETTE segment's top-left in window coordinates, through the
/// scroll offset (the palettes tab's create row shifts its scroll top down).
pub fn palette_swatch_anchor(group: usize, seg: usize, n: usize, scroll: f32) -> (f32, f32) {
    (
        panel_content_left() + segment_x(seg, n),
        palettes_scroll_top()
            + palette_group_offset(group)
            + PALETTE_TITLE_ROW_H
            + PANEL_HEADING_GAP
            - scroll,
    )
}

/// Pure, unit-tested: the create row's SORT icon anchor in window coordinates (item
/// six): the row's left edge, the icon square centred on the row's height. What the
/// relocated sort flyout fits itself against.
pub fn sort_icon_anchor() -> (f32, f32) {
    (
        panel_content_left(),
        panel_content_top() + (PALETTE_CREATE_ROW_H - RECENT_SWATCH) / 2.0,
    )
}

/// Pure, unit-tested: the MAIN round swatch's top-left in window coordinates (item
/// seven), for its new context menu's fit: the controls row, one pipette and one gap in.
pub fn main_swatch_anchor() -> (f32, f32) {
    (
        WINDOW_BORDER + WINDOW_PADDING + CONTROLS_BUTTON + ROW_SPACING,
        WINDOW_BORDER + header_h() + WINDOW_PADDING + SV_H + GAP_SQUARE_CONTROLS,
    )
}

/// **Pure**, unit-tested: the MAIN swatch menu's root rows, by label, in the harmony
/// menu's own relative order (item seven). NO "Set as active color": the main swatch IS
/// the active colour, and an entry that sets a thing to itself is a no-op dressed as an
/// action. The palette row wears the harmony menu's own "Add to palette" words (the
/// owner said "copy to palette" naming the harmony entry loosely; one vocabulary beats
/// two spellings of one verb) and its exact any-palette gate.
pub fn main_swatch_menu_labels(palette_count: usize) -> Vec<&'static str> {
    let mut rows = vec![ADD_TO_RECENTS_LABEL];
    if offers_add_to_palette(palette_count) {
        rows.push(ADD_TO_PALETTE_LABEL);
    }
    rows.push(COPY_COLOR_LABEL);
    rows
}

/// Pure, unit-tested: a palette group HEADING's top-left in window coordinates, through
/// the scroll offset.
pub fn palette_heading_anchor(group: usize, scroll: f32) -> (f32, f32) {
    (panel_content_left(), palettes_scroll_top() + palette_group_offset(group) - scroll)
}

// ── The recent-colours list ──────────────────────────────────────────────────

/// What produced the colour the window is currently showing. The ONE input to
/// [`writes_recents`].
///
/// It is an enum rather than a bool at each handler because the three cases read
/// identically at the call site (they all end with "the window now shows this colour")
/// and only differ in whether the list is allowed to move. Naming them here is what
/// stops the rule being re-derived, differently, in three places.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorSource {
    /// The user sampled a pixel with the picker overlay. The only writer.
    Pick,
    /// The user clicked one of the recent swatches.
    RecentClick,
    /// The user typed into one of the value boxes.
    Edit,
    /// The user took a HARMONY swatch as the current colour by DROPPING it on the tools
    /// (DRAGON-682).
    ///
    /// It WRITES the recents (item 22, the owner: "when we set to active color from the
    /// harmonies panel, it should add to recents too"), which is what separates it from
    /// [`Self::RecentClick`]: a harmony swatch is a colour the user has just DERIVED and
    /// chosen, so it is new to the list in the same way a pick is, while a recents click is
    /// a colour that is already in it.
    ///
    /// **The drop is its ONE user since DRAGON-687's item five**: the harmony menu's
    /// set-active (and the plain click) go through `RecentClick` and file the PREVIOUS
    /// colour instead, item ten's [`files_outgoing`] bump, the owner's later model. A
    /// drop with THIS source files both: the clicked colour by item 22's rule here, and
    /// the outgoing one by the bump.
    Harmony,
}

/// Pure, unit-tested: may this event write the recent-colours list?
///
/// Only a PICK. The distinction is the whole point of the feature, so it is one
/// predicate rather than a condition repeated in each handler:
///
/// * Clicking a recent LOADS it. Click three in a row and the list is byte-identical to
///   before, so the row stays a stable place to look rather than reshuffling itself
///   under the pointer. Promoting on click would also make the list a record of what was
///   last CLICKED, which is not what "recent colours" means to anyone.
/// * Editing a value box changes the colour on screen, but the user is exploring, not
///   sampling. Recording every keystroke's intermediate colour would fill the row with
///   noise within one edit.
///
/// **It governs the WINDOW's own colour changes, and only picks that reach the window.**
/// A PALETTE-DESTINED pick (DRAGON-687 follow-up, the palette row's pipette) never gets
/// here at all: `PickDestination::files_pick_ordinarily` is the one place that exception
/// lives, and it means exactly that such a pick writes no recents, moves no active
/// colour and takes no clipboard, in the child or in the receiving window.
///
/// **It governs COLOUR CHANGES, not every write.** Two paths write the list without asking
/// it, and both are deliberate: the "Add to recents" button (and its primary+Enter chord)
/// files the shown colour by name, and [`remove_recent`] forgets one by name (DRAGON-680
/// item 24). What this rule protects the user from is the list reordering itself BEHIND
/// them as a side effect of looking at a colour; an action they asked for by name is the
/// opposite of that, and neither rule weakens the other.
pub fn writes_recents(source: ColorSource) -> bool {
    matches!(source, ColorSource::Pick | ColorSource::Harmony)
}

/// **Pure**, unit-tested: does this source bring its OWN alpha, or is the colour opaque by
/// nature (DRAGON-682 item 22)?
///
/// A PICK is a screen pixel and has no transparency, so it resets the window's alpha to
/// opaque. Everything else arrives WITH one: a typed edit spells its own, a history entry
/// carries the alpha it was filed at, and a harmony swatch is drawn at the window's current
/// alpha and taken at it.
///
/// It exists because the two rules used to disagree: the alpha was reset by the apply and
/// then put back by the caller, one line later, at two call sites. That worked until the
/// recents write moved INTO the apply (item 22), where it saw the reset value and filed an
/// opaque entry for a translucent colour.
pub fn keeps_alpha(source: ColorSource) -> bool {
    !matches!(source, ColorSource::Pick)
}

/// WHERE a clipboard write in this window came from (DRAGON-682 item 15). The ONE input to
/// [`copy_flashes`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CopySource {
    /// The window's own copy button, or its keyboard chord.
    CopyButton,
    /// The pick's own copy, performed as the window opens.
    ///
    /// Not EVERY pick copies: a palette-destined one (DRAGON-687 follow-up) skips the
    /// clipboard entirely and so never mints a copy source at all
    /// (`PickDestination::files_pick_ordinarily`, the one table for that exception).
    Pick,
    /// A SWATCH's context menu: a harmony segment, or a history entry.
    SwatchMenu,
}

/// **Pure**, unit-tested: does this copy raise the main copy button's success flash?
///
/// **No, for a swatch menu** (the owner: "right clicking a color in the harmonies and
/// copying should not make the main copy button activate in the main ui"). The flash is that
/// BUTTON's acknowledgement, and the button copies the window's own value; lighting it for a
/// colour the user copied from somewhere else says the wrong thing twice over, once about
/// what was copied and once about which control did it.
///
/// Yes for the other two, unchanged: the button flashing when you press the button is the
/// whole point of it, and the pick's open-time copy raises it because by the time the window
/// appears that copy has already happened and nothing else would say so.
pub fn copy_flashes(source: CopySource) -> bool {
    !matches!(source, CopySource::SwatchMenu)
}

/// What a harmony segment shows above itself right now (DRAGON-682 item 30).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SwatchTip {
    /// A transient "Copied!" card, because this segment's menu just copied it.
    Copied,
    /// Its hex, pinned open because the KEYBOARD cursor is on it.
    PinnedHex,
    /// Its hex, on hover, like every other swatch in the window.
    Hover,
    /// NOTHING, because a drag is in flight (DRAGON-682 item 35). A card under the ghost is
    /// a card about the swatch the pointer left, over the swatch it is heading for.
    Silent,
}

/// **Pure**, unit-tested: which of those a segment shows, given whether its copy flash is
/// live and whether the keyboard cursor is on it.
///
/// **"Copied!" WINS while it lasts** (the owner's ask), and the hex card comes back
/// afterwards, because the two can want the same swatch at once: the cursor is on a segment,
/// the menu opens from it, and the copy lands. Two cards over one swatch is two answers to
/// one question, and of the two answers the transient one is the one the user just caused.
/// A live DRAG silences all three, which is the honest answer while the pointer is carrying
/// something: hover cards would follow the ghost around the window announcing swatches
/// nobody is looking at.
pub fn swatch_tip(copied_here: bool, on_cursor: bool, dragging: bool) -> SwatchTip {
    match (dragging, copied_here, on_cursor) {
        (true, _, _) => SwatchTip::Silent,
        (false, true, _) => SwatchTip::Copied,
        (false, false, true) => SwatchTip::PinnedHex,
        (false, false, false) => SwatchTip::Hover,
    }
}

// `SwatchClick` / `swatch_click_outcome` sat here for item five of DRAGON-687's
// drag-scroll round: the click-specific "bump the previous colour into the recents".
// Item ten generalised the bump to EVERY discrete replacement of the active colour and
// moved it into the one apply path, so the click-specific decision folded into
// [`files_outgoing`] rather than surviving as a second mechanism beside it.

/// **Pure**, unit-tested: on a replacement of the ACTIVE colour, must the OUTGOING one
/// be filed into the recents first (DRAGON-687 item ten, the owner's closing rule: "any
/// time we set the active color, if the current color isnt in history we should run the
/// logic that adds it first. we dont want current colors going missing")?
///
/// The table, pinned by `files_outgoing_tests`:
///
/// * **Every DISCRETE source bumps**: swatch clicks and menu set-actives
///   (`RecentClick`), drops on the tools (`Harmony`), pick deliveries (`Pick`), and
///   recents loads (`RecentClick` again; loading a recent whose predecessor was unsaved
///   FILES the predecessor, the owner's intent, while the loaded entry itself still
///   neither rewrites nor reorders, `writes_recents` unchanged).
/// * **`Edit` never bumps**: the sliders, the SV square, the strips and the value boxes
///   are continuous exploration, and only the explicit Add action files there, exactly
///   as before.
/// * **A colour already in the history is not filed again** (the absent-check), and
///   replacing a colour with ITSELF (colour and alpha both) bumps nothing: nothing is
///   going missing.
/// * **`window_open` is the "there IS an outgoing colour" gate**: the window-open loads
///   (the viewer's, and a fresh pick's, which both run before the window is minted)
///   replace the state's DEFAULT, not a colour the user held, and filing that default
///   would seed every session's history with it.
///
/// **BUMP-ONLY (item five's stated choice, carried forward):** what is filed is the
/// PREVIOUS colour, never additionally the incoming one; whether the INCOMING colour
/// also files stays [`writes_recents`]'s answer (so a harmony drop files both: its own
/// colour by item 22, the outgoing one by this rule).
pub fn files_outgoing(
    window_open: bool,
    source: ColorSource,
    outgoing: Recent,
    incoming: Recent,
    recents: &[Recent],
) -> bool {
    window_open
        && source != ColorSource::Edit
        && outgoing != incoming
        && !recents.contains(&outgoing)
}

/// What SPACE or ENTER does, per focus stop (DRAGON-682 item 32).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AcceptAction {
    /// The history's cursor: LOAD that swatch, exactly as clicking it would (item 7). No
    /// clipboard, no history write.
    ApplyRecent,
    /// The panel's cursor: COPY that swatch, exactly as its right-click menu's Copy does
    /// (item 32). No change to the active colour.
    CopySwatch,
}

/// **Pure**, unit-tested: what the accept key does at the stop that currently holds the
/// focus ring (DRAGON-682 item 32).
///
/// **The two stops mean different things on purpose, and it is the owner's choice**:
/// the history APPLIES, the panel COPIES. It looks like an inconsistency and is not one.
/// A recent is a colour you already chose, so taking it back is the only thing "accept"
/// could mean; a harmony swatch is a suggestion the window computed, and the owner's whole
/// interaction with it is "give me that value", which is what its menu offers. Applying one
/// would also move the active colour, which recomputes every harmony under it, so the
/// keyboard would be walking a grid that changes under the cursor. **Do not unify these
/// later**: this reads like a rough edge from the outside and is a deliberate one.
///
/// Item 9 said the panel's Space and Enter did NOTHING; item 32 replaced that. This is
/// where the amendment lives, so there is one place to read the current rule.
///
/// `None` means the press is not ours: it falls through untouched, which is what leaves
/// Space to a focused text input and Enter to whatever else is listening.
pub fn accept_action(
    focus: Option<PickerFocus>,
    recent_cursor: bool,
    panel_cursor: bool,
) -> Option<AcceptAction> {
    match focus {
        // A cursor is REQUIRED, at both stops: with the ring on a grid that nothing is
        // pointing at, there is no "that one" to act on.
        Some(PickerFocus::History) if recent_cursor => Some(AcceptAction::ApplyRecent),
        Some(PickerFocus::Panel) if panel_cursor => Some(AcceptAction::CopySwatch),
        _ => None,
    }
}

/// **Pure**, unit-tested: how many swatches each HARMONY card holds, in panel order
/// (DRAGON-682 item 9).
///
/// The panel's cursor navigates a RAGGED grid (`keynav::ragged_step`), and this is the
/// shape it navigates: one row per harmony, each as long as that harmony's own card. It
/// lives here rather than being counted in the view because the keyboard handler needs it
/// without building any widgets.
pub fn harmony_card_lengths() -> Vec<usize> {
    crate::color::Harmony::ALL
        .into_iter()
        .map(|h| {
            if h == crate::color::Harmony::Monochromatic {
                // The one card whose length is a value ladder rather than a rotation count.
                h.swatches(Srgb::new(0, 0, 0)).len()
            } else {
                1 + h.offsets().len()
            }
        })
        .collect()
}

/// ONE entry of the colour history: a colour AND its alpha (DRAGON-680).
///
/// The list held a bare `Srgb` until the owner asked to "save to history with transparency
/// intact". Alpha is a real part of the value the window copies out (`#RRGGBBAA`,
/// `rgba(...)`), so a history that dropped it was handing back a different colour from the
/// one that was filed, and the split swatch that shows the transparency has nothing to draw
/// without it.
///
/// A STRUCT rather than a `(Srgb, u8)` pair because it is stored, persisted, compared for
/// selection and de-duplicated: every one of those reads better with the two fields named,
/// and a bare tuple invites an argument order mistake at the four call sites that build one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Recent {
    pub color: Srgb,
    /// `255` = fully opaque, which is what every entry written before DRAGON-680 is, and
    /// what a config with no alpha digits loads as.
    pub alpha: u8,
}

impl Recent {
    pub fn new(color: Srgb, alpha: u8) -> Self {
        Self { color, alpha }
    }

    /// A fully opaque entry: what a PICK files (a screen pixel has no transparency) and
    /// what a legacy `#RRGGBB` config entry becomes.
    pub fn opaque(color: Srgb) -> Self {
        Self::new(color, u8::MAX)
    }

    /// The `#RRGGBB` / `#RRGGBBAA` spelling this entry is PERSISTED as, and the one its
    /// tooltip shows (`ColorFormat::format_with_alpha`, so it is the same spelling the
    /// value row and the clipboard use).
    ///
    /// An opaque entry spells with no alpha digits, which is what keeps a config written
    /// before DRAGON-680 byte-identical after this ticket: nothing about an existing
    /// history is rewritten just because alpha became expressible.
    pub fn hex(self) -> String {
        crate::color::ColorFormat::Hex.format_with_alpha(self.color, self.alpha)
    }

    /// **Pure**, unit-tested: an entry from a persisted spelling, `None` for junk.
    ///
    /// Accepts both shapes deliberately: `#RRGGBBAA` is what this ticket writes for a
    /// translucent entry, and `#RRGGBB` is every entry any older build wrote. The
    /// alpha-less form loads OPAQUE (`parse_with_alpha`'s own rule), so an existing
    /// history survives the upgrade unchanged rather than being dropped as unparseable.
    pub fn parse(s: &str) -> Option<Self> {
        crate::color::ColorFormat::Hex
            .parse_with_alpha(s)
            .map(|(color, alpha)| Self::new(color, alpha))
    }
}

/// **Pure**, unit-tested: `list` with the entry at `index` REMOVED (DRAGON-680 item 24).
///
/// Out of range answers the list unchanged rather than panicking: the index comes from a
/// menu that was opened over a swatch, and a pick delivered from another process can
/// reorder the history between the open and the click.
///
/// **Removal is a WRITE of the recents, and it is allowed** where a load or an edit is not
/// (see [`writes_recents`], whose rule is about which COLOUR CHANGES may reorder the list
/// behind the user's back). This is not a colour change at all: it is an explicit,
/// deliberate "forget this one", asked for by name from a context menu or a delete key, so
/// there is nothing to protect the user from. The two rules do not overlap and neither
/// weakens the other.
/// **Pure**, unit-tested: the recents after an explicit ADD of `entry` (DRAGON-682 item
/// 28), which is [`push_recent`] under the name of the action that uses it.
///
/// **The ACTIVE colour is not a parameter, and that is the point**: this action files a
/// colour the user pointed at, and the owner's ask was that it do so "without messing up the
/// active color". A function that cannot see the active colour cannot move it, which is a
/// stronger statement than a handler that merely does not, and it is why the add path is
/// shaped this way rather than as a variant of the apply.
///
/// One write path for every explicit add (the divider button, its primary+Enter chord, and
/// the harmony menu), so the newest-first order, the duplicate rule and the cap cannot fork
/// between them.
pub fn recents_after_add(list: &[Recent], entry: Recent, cap: usize) -> Vec<Recent> {
    push_recent(list, entry, cap)
}

pub fn remove_recent(list: &[Recent], index: usize) -> Vec<Recent> {
    let mut out = list.to_vec();
    if index < out.len() {
        out.remove(index);
    }
    out
}

/// **Pure**, unit-tested: which history entry a Backspace or Delete press removes, if any
/// (DRAGON-680 item 24).
///
/// The owner's rule, in order:
///
/// * **never while a value box has the caret.** A user typing a colour presses Backspace
///   constantly, and a swatch vanishing because the pointer happened to be resting over the
///   history would be the worst kind of surprise: silent, destructive and unrelated to what
///   they were doing. This is the HARD guard and it comes first;
/// * otherwise the HOVERED swatch, because that is the one the user is pointing at;
/// * otherwise the NAVIGATED swatch, but only while the history holds the focus ring, so a
///   swatch reached with the arrows can be deleted from the keyboard alone. It was the
///   LOADED swatch until DRAGON-682 item 7 split navigation from application: the cursor is
///   now what the user is pointing at with the keyboard, and the loaded colour may be
///   somewhere else entirely on the grid;
/// * otherwise nothing.
///
/// The guard is checkable precisely because the window parks toolkit focus outside the
/// boxes for its other two stops (`App::apply_picker_focus`): `focus` is `Box` when, and
/// only when, a text input really has the caret.
pub fn remove_target(
    hovered: Option<usize>,
    cursor: Option<usize>,
    focus: Option<PickerFocus>,
    len: usize,
) -> Option<usize> {
    if matches!(focus, Some(PickerFocus::Box(_))) {
        return None;
    }
    let live = |i: Option<usize>| i.filter(|i| *i < len);
    live(hovered).or_else(|| {
        if focus == Some(PickerFocus::History) { live(cursor) } else { None }
    })
}

/// Pure, unit-tested: `list` with `entry` pushed to the FRONT, de-duplicated, capped.
///
/// An exact duplicate MOVES its existing entry to the front rather than adding a second
/// copy: two identical swatches carry no information, and the conventional behaviour
/// everywhere else that keeps a recents list is to promote. The oldest entry falls off
/// the end at [`RECENTS_CAP`].
///
/// **"Exact" includes the ALPHA** (DRAGON-680). The same colour at two transparencies is
/// two different values: they copy out differently, they draw differently, and collapsing
/// them would silently throw away whichever one the user filed second.
///
/// A caller must still ask [`writes_recents`] first; this function is the WHAT, that one
/// is the WHETHER.
pub fn push_recent(list: &[Recent], entry: Recent, cap: usize) -> Vec<Recent> {
    let mut out = Vec::with_capacity(list.len() + 1);
    out.push(entry);
    out.extend(list.iter().copied().filter(|c| *c != entry));
    out.truncate(cap.max(1));
    out
}

#[cfg(test)]
mod source_pixel_tests {
    use super::*;

    /// The identity case: one image pixel per capture unit, so the pixel index IS the
    /// truncated offset.
    #[test]
    fn an_unscaled_output_maps_one_to_one() {
        let cap = (1920, 1080);
        let img = (1920, 1080);
        assert_eq!(source_pixel((0.0, 0.0), cap, img), Some((0, 0)));
        assert_eq!(source_pixel((10.4, 10.6), cap, img), Some((10, 10)));
        assert_eq!(source_pixel((960.0, 540.0), cap, img), Some((960, 540)));
    }

    /// THE requirement: the screen's furthest edge pixel is reachable, at every scale.
    /// A cursor at the last point of the surface answers the last pixel, and one past
    /// the end (a rounding overshoot at the rim) still answers the last pixel rather
    /// than falling off.
    #[test]
    fn the_furthest_edges_are_reachable() {
        for (cap, img) in [
            ((1920, 1080), (1920u32, 1080u32)),
            ((1920, 1080), (3840, 2160)),
            ((2560, 1440), (3840, 2160)),
            ((1280, 720), (3840, 2160)),
        ] {
            let last = (img.0 - 1, img.1 - 1);
            let edge = (cap.0 as f32 - 0.001, cap.1 as f32 - 0.001);
            assert_eq!(source_pixel(edge, cap, img), Some(last), "{cap:?} -> {img:?}");
            // Exactly at, and past, the far edge: clamped, never wrapped or dropped.
            assert_eq!(source_pixel((cap.0 as f32, cap.1 as f32), cap, img), Some(last));
            assert_eq!(source_pixel((cap.0 as f32 + 50.0, 0.0), cap, img), Some((last.0, 0)));
            // And the FIRST pixel at the opposite corner.
            assert_eq!(source_pixel((0.0, 0.0), cap, img), Some((0, 0)));
            assert_eq!(source_pixel((-5.0, -5.0), cap, img), Some((0, 0)));
        }
    }

    /// A HiDPI snapshot addresses BOTH image pixels inside one capture unit, which a
    /// truncated capture coordinate could never do. This is why the mapping takes the
    /// fractional offset (see `OverlayUnits::capture_offset_f`).
    #[test]
    fn a_scaled_snapshot_resolves_within_one_capture_unit() {
        let (cap, img) = ((1920, 1080), (3840u32, 2160u32));
        assert_eq!(source_pixel((100.0, 50.0), cap, img), Some((200, 100)));
        assert_eq!(source_pixel((100.5, 50.5), cap, img), Some((201, 101)));
        assert_eq!(source_pixel((100.9, 50.9), cap, img), Some((201, 101)));
    }

    /// EVERY whole-point position on an unscaled output names its own pixel, with nothing
    /// dropped to rounding (DRAGON-587).
    ///
    /// This one is a regression pin with a body count. The mapping used to divide and then
    /// multiply back, and the round trip landed a hair under a whole number often enough that
    /// 22 of a 1920-wide output's 1920 whole-point positions reported the column to their LEFT.
    /// A colour picker reporting the wrong pixel is the worst kind of quiet defect: nothing
    /// looks broken, the answer is simply not the one on screen.
    #[test]
    fn every_whole_point_names_its_own_pixel() {
        for extent in [1920u32, 1080, 5120, 1440, 800, 480, 2560, 1366] {
            for i in 0..extent {
                assert_eq!(
                    source_pixel((i as f32, 0.0), (extent as i32, 1080), (extent, 1080)),
                    Some((i, 0)),
                    "a {extent}-wide output dropped point {i}"
                );
            }
        }
        // And the same on a 2x snapshot, where one point is two pixels.
        for i in 0..1920u32 {
            assert_eq!(
                source_pixel((i as f32, 0.0), (1920, 1080), (3840, 2160)),
                Some((i * 2, 0)),
                "a 2x snapshot dropped point {i}"
            );
        }
    }

    /// A degenerate output or snapshot has no pixel to name, and says so instead of
    /// guessing one.
    #[test]
    fn a_degenerate_source_names_no_pixel() {
        assert_eq!(source_pixel((5.0, 5.0), (0, 1080), (1920, 1080)), None);
        assert_eq!(source_pixel((5.0, 5.0), (1920, 0), (1920, 1080)), None);
        assert_eq!(source_pixel((5.0, 5.0), (-1, 1080), (1920, 1080)), None);
        assert_eq!(source_pixel((5.0, 5.0), (1920, 1080), (0, 1080)), None);
        assert_eq!(source_pixel((5.0, 5.0), (1920, 1080), (1920, 0)), None);
        // A non-finite offset degrades to the first pixel rather than panicking on the
        // cast; nothing upstream produces one, but the cast would be UB-adjacent.
        assert_eq!(source_pixel((f32::NAN, 0.0), (1920, 1080), (1920, 1080)), Some((0, 0)));
    }
}

#[cfg(test)]
mod magnifier_tests {
    use super::*;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> image::RgbaImage {
        image::RgbaImage::from_pixel(w, h, image::Rgba(px))
    }

    /// The WHOLE disc with no accent ring, which is what these tests are about: the content,
    /// the mask and the sampled-cell marker. The ring has its own test.
    pub(super) fn plain(
        src: &image::RgbaImage,
        center: (u32, u32),
        zoom: u32,
    ) -> (u32, Vec<u8>) {
        let (w, h, buf) = magnifier_rgba(src, center, zoom, (0.0, [0, 0, 0, 0]), DiscView::FULL);
        assert_eq!((w, h), (MAGNIFIER_DIAMETER, MAGNIFIER_DIAMETER), "an uncropped disc");
        (w, buf)
    }

    /// The disc is square, sized as declared, and its CORNERS are transparent: the
    /// circular mask is what makes it read as a lens.
    #[test]
    fn the_disc_is_masked_to_a_circle() {
        let src = solid(64, 64, [10, 200, 90, 255]);
        let (d, buf) = plain(&src, (32, 32), MAGNIFIER_ZOOM_DEFAULT);
        assert_eq!(d, MAGNIFIER_DIAMETER);
        assert_eq!(buf.len(), (d as usize) * (d as usize) * 4);
        let at = |x: u32, y: u32| {
            let i = ((y as usize) * (d as usize) + x as usize) * 4;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        assert_eq!(at(0, 0)[3], 0, "top-left corner is outside the circle");
        assert_eq!(at(d - 1, d - 1)[3], 0, "bottom-right corner too");
        assert_eq!(at(d / 2, 2)[3], 255, "the top of the circle is opaque");
        assert_eq!(at(2, d / 2)[3], 255, "and its left edge");
    }

    /// Each source pixel becomes one CELL-sized square: the grid a user aims with.
    #[test]
    fn each_source_pixel_becomes_one_cell() {
        // A two-colour source: the centre column red, everything else blue.
        let mut src = solid(9, 9, [0, 0, 255, 255]);
        for y in 0..9 {
            src.put_pixel(4, y, image::Rgba([255, 0, 0, 255]));
        }
        let (d, buf) = plain(&src, (4, 4), MAGNIFIER_ZOOM_DEFAULT);
        let at = |x: u32, y: u32| {
            let i = ((y as usize) * (d as usize) + x as usize) * 4;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        // The middle cell spans the centre column of the disc; sample just inside it so
        // the centre-cell OUTLINE (its own 1px border) is not what is read.
        let mid = d / 2;
        assert_eq!(at(mid, mid)[0], 255, "the centre cell carries the centre pixel");
        // One whole cell to the left is the blue neighbour.
        assert_eq!(at(mid - MAGNIFIER_CELL, mid)[2], 255, "its neighbour is the next pixel");
    }

    /// Off the edge of the world the disc stays TRANSPARENT rather than repeating the
    /// edge pixel: a picker may never draw a colour that is not on screen.
    #[test]
    fn out_of_bounds_stays_transparent() {
        let src = solid(4, 4, [255, 255, 255, 255]);
        // Centre on the top-left pixel: everything up and left of it is off the image.
        let (d, buf) = plain(&src, (0, 0), MAGNIFIER_ZOOM_DEFAULT);
        let at = |x: u32, y: u32| {
            let i = ((y as usize) * (d as usize) + x as usize) * 4;
            buf[i + 3]
        };
        let mid = d / 2;
        // A cell up-and-left of centre, still inside the circle, has no source pixel.
        assert_eq!(at(mid - MAGNIFIER_CELL, mid - MAGNIFIER_CELL), 0);
        // Down-and-right of centre there IS one.
        assert_eq!(at(mid + MAGNIFIER_CELL, mid + MAGNIFIER_CELL), 255);
    }

    /// The centre cell is outlined in the ink the centre pixel can be seen against, so
    /// the sampled pixel is identifiable under a light or a dark colour alike.
    #[test]
    fn the_centre_cell_is_outlined_against_its_own_colour() {
        let ink = |px: [u8; 4]| {
            let src = solid(9, 9, px);
            let (d, buf) = plain(&src, (4, 4), MAGNIFIER_ZOOM_DEFAULT);
            // The top-left corner of the centre cell is on its outline.
            let half = MAGNIFIER_SPAN / 2;
            let (x, y) = (half * MAGNIFIER_CELL, half * MAGNIFIER_CELL);
            let i = ((y as usize) * (d as usize) + x as usize) * 4;
            [buf[i], buf[i + 1], buf[i + 2]]
        };
        assert_eq!(ink([255, 255, 255, 255]), [0, 0, 0], "a light pixel gets a dark box");
        assert_eq!(ink([0, 0, 0, 255]), [255, 255, 255], "a dark pixel gets a light box");
    }
}

/// DRAGON-587 item 7: the magnifier CLIPS at a screen edge. It never stops following the
/// sample point, never flips to the pointer's other side, and never changes size or shape.
///
/// `cursor` in these tests is the disc's CENTRE, which is the sample point, which since
/// DRAGON-597 is the pointer itself.
#[cfg(test)]
mod disc_clip_tests {
    use super::*;

    const VIEW: (f32, f32) = (1920.0, 1080.0);
    const R: f32 = MAGNIFIER_DIAMETER as f32 / 2.0;

    /// Away from every wall the disc is whole, uncropped, and centred on the sample point.
    #[test]
    fn the_open_screen_disc_is_whole_and_centred() {
        for cursor in [(960.0, 540.0), (R, R), (VIEW.0 - R, VIEW.1 - R)] {
            let v = disc_view(cursor, VIEW).expect("on screen");
            assert_eq!(v.crop, (0, 0), "{cursor:?}");
            assert_eq!(v.size, (MAGNIFIER_DIAMETER, MAGNIFIER_DIAMETER), "{cursor:?}");
            assert_eq!(
                v.origin,
                ((cursor.0 - R).round() as i32, (cursor.1 - R).round() as i32),
                "{cursor:?}: centred on the pointer"
            );
        }
    }

    /// At every wall and every corner: the visible part is the disc's TRUE box intersected
    /// with the screen. Which is to say the circle is cut off, and nothing else happens to
    /// it — no clamped centre, no shrunken radius, no changed aspect.
    #[test]
    fn every_edge_cuts_the_circle_and_moves_nothing() {
        let d = MAGNIFIER_DIAMETER as f32;
        for cursor in [
            (0.0, 540.0),
            (3.0, 540.0),
            (VIEW.0, 540.0),
            (VIEW.0 - 3.0, 540.0),
            (960.0, 0.0),
            (960.0, VIEW.1),
            (0.0, 0.0),
            (VIEW.0, 0.0),
            (0.0, VIEW.1),
            (VIEW.0, VIEW.1),
            (1.0, VIEW.1 - 1.0),
        ] {
            let v = disc_view(cursor, VIEW).expect("part of it is always on screen");
            // The visible rectangle, recomputed from the TRUE box the same way a human
            // would: intersect it with the screen.
            let (l, t) = (cursor.0 - R, cursor.1 - R);
            let want_x = (-l).max(0.0).round() as u32;
            let want_y = (-t).max(0.0).round() as u32;
            assert_eq!(v.crop, (want_x, want_y), "{cursor:?}: cut off exactly what is off screen");
            assert_eq!(v.origin.0, l.max(0.0).round() as i32, "{cursor:?}");
            assert_eq!(v.origin.1, t.max(0.0).round() as i32, "{cursor:?}");
            // Nothing is ever scaled: what is drawn is a whole number of the disc's own
            // points, and it always fits in the surface from where it is placed.
            assert!(v.size.0 <= MAGNIFIER_DIAMETER - v.crop.0, "{cursor:?}: never grown");
            assert!(v.size.1 <= MAGNIFIER_DIAMETER - v.crop.1, "{cursor:?}");
            assert!(
                v.origin.0 as f32 + v.size.0 as f32 <= VIEW.0 + 0.5
                    && v.origin.1 as f32 + v.size.1 as f32 <= VIEW.1 + 0.5,
                "{cursor:?}: the drawn part fits, so the layout has nothing to squash"
            );
            // A pointer at least half a disc inside a wall keeps that side whole.
            if l >= 0.0 && l + d <= VIEW.0 {
                assert_eq!(v.size.0, MAGNIFIER_DIAMETER, "{cursor:?}: this axis is untouched");
            }
        }
    }

    /// The rendered buffer IS the visible part: same size, and its content is the same
    /// picture, just window-shifted. A clipped disc must never be a re-rendered smaller one.
    #[test]
    fn the_raster_matches_the_uncropped_disc_pixel_for_pixel() {
        let mut src = image::RgbaImage::from_pixel(400, 400, image::Rgba([20, 90, 200, 255]));
        src.put_pixel(200, 200, image::Rgba([255, 0, 0, 255]));
        let ring = (2.0, [10, 20, 30, 255]);
        let (fw, _fh, full) = magnifier_rgba(&src, (200, 200), 12, ring, DiscView::FULL);
        let crop = DiscView {
            origin: (0, 0),
            crop: (40, 25),
            size: (MAGNIFIER_DIAMETER - 40, MAGNIFIER_DIAMETER - 25),
        };
        let (cw, ch, cut) = magnifier_rgba(&src, (200, 200), 12, ring, crop);
        assert_eq!((cw, ch), crop.size);
        for y in 0..ch {
            for x in 0..cw {
                let ci = ((y as usize) * (cw as usize) + x as usize) * 4;
                let fi = (((y + crop.crop.1) as usize) * (fw as usize)
                    + (x + crop.crop.0) as usize)
                    * 4;
                assert_eq!(
                    cut[ci..ci + 4],
                    full[fi..fi + 4],
                    "({x},{y}) differs from the same point of the whole disc"
                );
            }
        }
    }

    /// A pointer entirely off the surface has no disc to draw, and says so rather than
    /// producing an empty image.
    #[test]
    fn a_pointer_off_the_surface_has_no_visible_disc() {
        assert_eq!(disc_view((-R - 1.0, 540.0), VIEW), None);
        assert_eq!(disc_view((VIEW.0 + R + 1.0, 540.0), VIEW), None);
        assert_eq!(disc_view((960.0, -R - 1.0), VIEW), None);
        assert_eq!(disc_view((960.0, VIEW.1 + R + 1.0), VIEW), None);
    }

    /// The lens's own rim is drawn even where there is no source pixel under it, so the
    /// circle still reads as a circle at the very edge of the frozen image; the CONTENT
    /// there stays transparent, which is what tells the user the screen ends.
    #[test]
    fn the_edge_of_the_world_shows_the_backdrop_not_invented_pixels() {
        let src = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 255]));
        let ring = (3.0, [7, 8, 9, 255]);
        let (w, _h, buf) = magnifier_rgba(&src, (0, 0), 12, ring, DiscView::FULL);
        let at = |x: u32, y: u32| {
            let i = ((y as usize) * (w as usize) + x as usize) * 4;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        let mid = w / 2;
        // Up and left of the sampled pixel there is no screen: transparent, never a
        // repeated edge pixel.
        assert_eq!(at(mid - MAGNIFIER_CELL, mid - MAGNIFIER_CELL)[3], 0);
        // The rim on that same side is still the ring.
        assert_eq!(at(2, mid), [7, 8, 9, 255], "the lens keeps its edge");
    }
}

/// DRAGON-650: the lens's PLACEMENT follows the live sample even while the pacing leaves
/// its raster stale. These pin the two ends: a fresh raster places exactly where
/// [`disc_view`] put it (so every unpaced path is byte-identical), and a stale one follows
/// the sample instead of parking at the raster's own origin, which was the reported
/// "skips around erratically".
#[cfg(test)]
mod drawn_origin_tests {
    use super::*;

    const VIEW: (f32, f32) = (1920.0, 1080.0);
    const R: f32 = MAGNIFIER_DIAMETER as f32 / 2.0;

    /// THE identity that keeps everything except the paced frames byte-identical: whenever
    /// the raster IS the current view (every unpaced route, and the settled lens), the drawn
    /// origin is the raster's own origin, exactly. Sampled across the open screen, every
    /// wall, every corner, and fractional positions.
    #[test]
    fn a_fresh_raster_is_placed_exactly_where_disc_view_put_it() {
        for sample in [
            (960.0, 540.0),
            (0.0, 0.0),
            (3.0, 540.0),
            (VIEW.0, 540.0),
            (960.0, VIEW.1),
            (VIEW.0, VIEW.1),
            (0.5, 0.5),
            (75.5, 540.0), // half-point sample with the disc off the left edge
            (1.0, VIEW.1 - 1.0),
            (1234.25, 77.75),
        ] {
            let v = disc_view(sample, VIEW).expect("on screen");
            assert_eq!(
                drawn_disc_origin(sample, v, VIEW),
                v.origin,
                "{sample:?}: a fresh raster must not move"
            );
        }
    }

    /// The fix itself: a stale FULL raster follows the sample on every frame. Mid-sweep the
    /// buffer is uncropped and the current view (away from walls) is uncropped too, so the
    /// drawn origin is the CURRENT view's origin — the lens glides with the pointer while
    /// its picture lags, instead of standing still and jumping.
    #[test]
    fn a_stale_raster_follows_the_sample_across_the_open_screen() {
        let rastered_at = (400.0, 400.0);
        let raster = disc_view(rastered_at, VIEW).expect("on screen");
        assert_eq!(raster.crop, (0, 0), "mid-screen rasters are whole");
        // 40ms of the owner's own flick (~17000 pt/s) is ~680 points of travel.
        for sample in [(420.0, 400.0), (700.0, 500.0), (1080.0, 400.0)] {
            let current = disc_view(sample, VIEW).expect("on screen");
            assert_eq!(
                drawn_disc_origin(sample, raster, VIEW),
                current.origin,
                "{sample:?}: the lens must be where a fresh disc would be"
            );
        }
    }

    /// At a wall a stale FULL buffer cannot be drawn half off the surface: the view places
    /// by padding (never negative) and squashes an overflowing image, so the placement
    /// clamps flush instead. One pacing interval later the raster is rebuilt clipped for
    /// that position and the clamp is moot.
    #[test]
    fn a_stale_full_raster_parks_flush_at_the_walls_rather_than_squashing() {
        let raster = disc_view((400.0, 400.0), VIEW).expect("on screen");
        let d = MAGNIFIER_DIAMETER as f32;
        // Swept to the left wall: never negative.
        let at_left = drawn_disc_origin((0.0, 400.0), raster, VIEW);
        assert_eq!(at_left.0, 0);
        // Swept to the right wall: the whole buffer still fits.
        let at_right = drawn_disc_origin((VIEW.0, 400.0), raster, VIEW);
        assert_eq!(at_right.0, (VIEW.0 - d) as i32);
        // Top and bottom, same rule.
        assert_eq!(drawn_disc_origin((400.0, 0.0), raster, VIEW).1, 0);
        assert_eq!(drawn_disc_origin((400.0, VIEW.1), raster, VIEW).1, (VIEW.1 - d) as i32);
    }

    /// The other direction: a raster built CLIPPED at a wall, swept back inland before the
    /// next rebuild. The buffer's first pixel is disc pixel `crop`, so it lands `crop`
    /// points right of where the full disc's left edge would be — the visible part stays
    /// glued to the disc the sample describes, with the missing slice honestly absent.
    #[test]
    fn a_clipped_raster_swept_inland_keeps_its_pixels_on_the_disc() {
        let raster = disc_view((10.0, 540.0), VIEW).expect("on screen");
        assert!(raster.crop.0 > 0, "a near-wall raster is clipped");
        let sample = (400.0, 540.0);
        let current = disc_view(sample, VIEW).expect("on screen");
        assert_eq!(current.crop, (0, 0));
        let drawn = drawn_disc_origin(sample, raster, VIEW);
        assert_eq!(
            drawn.0,
            current.origin.0 + raster.crop.0 as i32,
            "disc pixel `crop` belongs `crop` points right of the disc's left edge"
        );
        assert_eq!(drawn.1, current.origin.1);
    }

    /// A sample with no visible disc at all (unreachable from a pointer over its own
    /// surface) answers the raster's own origin: the one placement known to be valid.
    #[test]
    fn an_off_surface_sample_keeps_the_rasters_own_placement() {
        let raster = disc_view((400.0, 400.0), VIEW).expect("on screen");
        assert_eq!(
            drawn_disc_origin((-R - 1.0, 540.0), raster, VIEW),
            raster.origin
        );
    }
}

/// DRAGON-587, the EDGE-PIXEL GUARANTEE, pinned rather than argued.
///
/// The owner raised this three times and was right each time, so it gets a proof instead of a
/// paragraph. The claim now has two halves and both are tested here:
///
/// * the SAMPLE POINT is the pointer's own point, and `(w-1, h-1)` is still reachable from a
///   pointer the compositor stops a hair inside the wall, because [`source_pixel`] maps from
///   the FRACTIONAL offset and clamps per axis. This used to also depend on the arrow
///   fallback's shortened shift; see this file's DRAGON-597 tombstone.
/// * the DISC is centred ON that sample point and can therefore never move it. Reversing the
///   dependency is exactly the mistake that made a screen-edge pixel unreachable before, so the
///   direction of that arrow is asserted rather than assumed.
#[cfg(test)]
mod edge_pixel_tests {
    use super::*;

    const VIEW: (f32, f32) = (1920.0, 1080.0);
    const CAPTURE: (i32, i32) = (1920, 1080);
    const IMAGE: (u32, u32) = (1920, 1080);
    const R: f32 = MAGNIFIER_DIAMETER as f32 / 2.0;

    /// The four extreme pointer positions and the pixel each must answer. The far walls are
    /// approached a thousandth of a point short, which is where a pointer stopped by the
    /// compositor actually sits.
    fn extremes() -> [((f32, f32), (u32, u32)); 4] {
        let last = (IMAGE.0 - 1, IMAGE.1 - 1);
        [
            ((0.0, 0.0), (0, 0)),
            ((VIEW.0 - 0.001, VIEW.1 - 0.001), last),
            ((0.0, VIEW.1 - 0.001), (0, last.1)),
            ((VIEW.0 - 0.001, 0.0), (last.0, 0)),
        ]
    }

    /// At every corner the extreme pixel is read, AND there is still a lens on screen to read
    /// it with. The second half matters: a lens that vanished at the corner would leave the
    /// user aiming blind even though the sample was correct.
    #[test]
    fn every_corner_samples_its_pixel_and_still_draws_a_lens() {
        for (pointer, want) in extremes() {
            let centre = pointer;
            assert_eq!(
                source_pixel(centre, CAPTURE, IMAGE),
                Some(want),
                "{pointer:?}: the corner pixel must be reachable"
            );
            let v = disc_view(centre, VIEW).expect("a corner still shows part of the lens");
            assert!(v.size.0 > 0 && v.size.1 > 0, "{pointer:?}: and it has pixels in it");
        }
    }

    /// The teeth, in the direction that can actually go wrong now. The disc is centred on the
    /// SAMPLE, so its centre and the sample are the same point at every position; if anything
    /// ever made the sample follow the disc instead, that would be a cycle, and if anything ever
    /// re-introduced a displaced disc this equality would break.
    #[test]
    fn the_disc_is_centred_on_the_sample_and_not_the_other_way_round() {
        for (pointer, want) in extremes() {
            let centre = pointer;
            let (l, t, r, b) = disc_rect(centre, R);
            assert_eq!(((l + r) / 2.0, (t + b) / 2.0), centre, "{pointer:?}");
            assert_eq!((r - l, b - t), (R * 2.0, R * 2.0), "{pointer:?}: one disc across");
            // The sample IS the pointer since DRAGON-597, so the lens sits exactly on it. That
            // is the other half of "the lens sits on the pointer", and the thing the displaced
            // placement got wrong. It used to be a within-one-point bound, because the arrow
            // fallback shifted the sample; see the tombstone at the top of this file.
            assert_eq!(centre, pointer, "{pointer:?}: the lens drifted off the pointer");
            assert_eq!(source_pixel(centre, CAPTURE, IMAGE), Some(want), "{pointer:?}");
        }
    }

    /// The guarantee survives SCALING, which is where an off-by-one would actually bite: a
    /// HiDPI output reports logical points while its snapshot is physical pixels, so the last
    /// pixel is only reachable because the mapping works from the FRACTIONAL offset.
    #[test]
    fn a_scaled_output_reaches_its_last_pixel() {
        for image in [(3840u32, 2160u32), (2560, 1440), (1280, 720)] {
            let last = (image.0 - 1, image.1 - 1);
            let centre = (VIEW.0 - 0.001, VIEW.1 - 0.001);
            assert_eq!(source_pixel(centre, CAPTURE, image), Some(last), "{image:?}");
            let origin = (0.0, 0.0);
            assert_eq!(source_pixel(origin, CAPTURE, image), Some((0, 0)), "{image:?}");
        }
    }

    /// A pointer parked ON the far wall, and one nudged PAST it by a rounding overshoot, both
    /// still answer the last pixel. The compositor stopping the pointer at the edge is what
    /// makes the extreme reachable at all, so the boundary itself is worth pinning.
    #[test]
    fn the_wall_itself_is_a_sampleable_position() {
        let last = (IMAGE.0 - 1, IMAGE.1 - 1);
        for pointer in [(VIEW.0, VIEW.1), (VIEW.0 + 4.0, VIEW.1 + 4.0)] {
            let centre = pointer;
            assert_eq!(source_pixel(centre, CAPTURE, IMAGE), Some(last), "{pointer:?}");
        }
        for pointer in [(0.0, 0.0), (-4.0, -4.0)] {
            let centre = pointer;
            assert_eq!(source_pixel(centre, CAPTURE, IMAGE), Some((0, 0)), "{pointer:?}");
        }
    }

    /// The owner's OWN desktop, spelled out, because the report was about this exact geometry:
    /// a 5120x1440 ultrawide at the origin with a 800x480 panel abutting its right edge at
    /// `(5120, 960)` (`cosmic-randr list`). Over rows 960 and below there is no wall on the
    /// ultrawide's right side at all, so the pointer crosses onto the panel instead of stopping.
    ///
    /// That crossing is why the lens looked like it vanished: each output draws only while the
    /// pointer is over IT. What this pins is that the crossing is never NECESSARY. The last
    /// column of the ultrawide is readable from the ultrawide, and the panel's own last column
    /// is readable from the panel, so neither display needs the pointer pushed off it.
    #[test]
    fn the_owners_two_outputs_each_reach_their_own_last_column() {
        // The ultrawide.
        let wide_view = (5120.0, 1440.0);
        let wide = (5120i32, 1440i32);
        let wide_img = (5120u32, 1440u32);
        for y in [10.0, 700.0, 1439.999] {
            let centre = (wide_view.0 - 0.001, y);
            let got = source_pixel(centre, wide, wide_img).expect("a real output");
            assert_eq!(got.0, wide_img.0 - 1, "the ultrawide's last column at y={y}");
            assert!(disc_view(centre, wide_view).is_some(), "and the lens is still drawn");
        }
        // The little panel, whose own right edge IS a wall.
        let panel_view = (800.0, 480.0);
        let panel = (800i32, 480i32);
        let panel_img = (800u32, 480u32);
        let centre = (panel_view.0 - 0.001, panel_view.1 - 0.001);
        assert_eq!(
            source_pixel(centre, panel, panel_img),
            Some((panel_img.0 - 1, panel_img.1 - 1)),
            "the panel's own bottom-right pixel"
        );
        // And the first pixel of each, from the opposite corner.
        assert_eq!(source_pixel((0.0, 0.0), wide, wide_img), Some((0, 0)));
        assert_eq!(
            source_pixel((0.0, 0.0), panel, panel_img),
            Some((0, 0))
        );
    }

    // ── DRAGON-599: the KEYBOARD reaches the same pixels the pointer does ─────────

    /// The nudged sample obeys the same rule as the pointer's, rather than a parallel one:
    /// it is the pointer's answer plus a whole number of source pixels, clamped to the same
    /// `0 ..= extent` interval, so the extremes stay reachable from the keyboard too.
    ///
    /// From every corner, walking OUTWARD into the wall as hard as you like still answers that
    /// corner's pixel, and never leaves the surface.
    #[test]
    fn nudging_into_a_wall_still_answers_the_wall_pixel() {
        for (pointer, want) in extremes() {
            let base = pointer;
            // Twenty steps in each direction, well past any wall from these positions.
            for nudge in [(-20, -20), (20, 20), (-20, 20), (20, -20)] {
                let got = nudged_sample(base, nudge, VIEW, IMAGE);
                assert!(
                    got.0 >= 0.0 && got.0 <= VIEW.0 && got.1 >= 0.0 && got.1 <= VIEW.1,
                    "{pointer:?} + {nudge:?}: the sample left the surface at {got:?}"
                );
                // Walking outward from a corner cannot move off that corner's own pixel.
                let outward = (
                    if want.0 == 0 { -20 } else { 20 },
                    if want.1 == 0 { -20 } else { 20 },
                );
                let held = nudged_sample(base, outward, VIEW, IMAGE);
                assert_eq!(
                    source_pixel(held, CAPTURE, IMAGE),
                    Some(want),
                    "{pointer:?}: pushing out of the corner must stay on {want:?}"
                );
            }
        }
    }

    /// `(0,0)` and `(w-1,h-1)` are reachable by KEY from the middle of the screen, not only by
    /// pointer. Walking far enough in each direction lands on the extreme pixel and stops
    /// there, which is the keyboard half of the guarantee this module exists for.
    #[test]
    fn the_keyboard_can_walk_to_either_extreme_pixel() {
        let base = (960.0, 540.0);
        let far = 4000; // more than the surface is wide, in pixels
        assert_eq!(
            source_pixel(nudged_sample(base, (-far, -far), VIEW, IMAGE), CAPTURE, IMAGE),
            Some((0, 0))
        );
        assert_eq!(
            source_pixel(nudged_sample(base, (far, far), VIEW, IMAGE), CAPTURE, IMAGE),
            Some((IMAGE.0 - 1, IMAGE.1 - 1))
        );
    }

    /// One press is exactly ONE source pixel, on an unscaled output AND on a scaled one. The
    /// scaled case is the reason `nudge_step` exists: a fixed one-POINT step would skip every
    /// other pixel of a HiDPI snapshot, and half the screen would be unreachable by keyboard.
    #[test]
    fn one_press_moves_exactly_one_source_pixel() {
        for image in [(1920u32, 1080u32), (3840, 2160), (2560, 1440)] {
            let base = (960.0, 540.0);
            let here = source_pixel(base, CAPTURE, image).expect("a real output");
            for (nudge, want) in [
                ((1, 0), (here.0 + 1, here.1)),
                ((-1, 0), (here.0 - 1, here.1)),
                ((0, 1), (here.0, here.1 + 1)),
                ((0, -1), (here.0, here.1 - 1)),
                ((3, -2), (here.0 + 3, here.1 - 2)),
            ] {
                let got = source_pixel(nudged_sample(base, nudge, VIEW, image), CAPTURE, image);
                assert_eq!(got, Some(want), "{image:?} + {nudge:?}");
            }
        }
    }

    /// With no keys pressed the nudge is the identity, so every pointer-only path is exactly
    /// what it was before DRAGON-599. Checked at the corners, where a stray epsilon would show.
    #[test]
    fn no_nudge_leaves_the_pointer_answer_untouched() {
        for (pointer, want) in extremes() {
            let base = pointer;
            assert_eq!(nudged_sample(base, (0, 0), VIEW, IMAGE), base, "{pointer:?}");
            assert_eq!(source_pixel(base, CAPTURE, IMAGE), Some(want));
        }
    }
}

/// DRAGON-587: the magnifier's zoom. One clamp, three routes, and the disc it produces.
#[cfg(test)]
mod magnifier_zoom_tests {
    use super::magnifier_tests::plain;
    use super::*;

    /// The range the owner specified, both ends: never MORE than what shipped, and never all
    /// the way out to 1:1. Written against absurd inputs too, because a route hands this a
    /// step count and a fast trackpad flick can hand it a big one.
    #[test]
    fn the_clamp_holds_at_both_ends() {
        assert_eq!(clamp_magnification(MAGNIFIER_ZOOM_MAX as i32), MAGNIFIER_ZOOM_MAX);
        assert_eq!(clamp_magnification(MAGNIFIER_ZOOM_MIN as i32), MAGNIFIER_ZOOM_MIN);
        assert_eq!(clamp_magnification(MAGNIFIER_ZOOM_MAX as i32 + 1), MAGNIFIER_ZOOM_MAX);
        assert_eq!(clamp_magnification(1_000_000), MAGNIFIER_ZOOM_MAX);
        assert_eq!(clamp_magnification(0), MAGNIFIER_ZOOM_MIN);
        assert_eq!(clamp_magnification(-1), MAGNIFIER_ZOOM_MIN, "a step below the floor saturates");
        assert_eq!(clamp_magnification(i32::MIN), MAGNIFIER_ZOOM_MIN, "and never wraps");
    }

    /// DRAGON-598: the floor sits ABOVE 1:1, and every magnification under it comes back as
    /// the floor. Named on its own because "you cannot zoom out to 1:1" is the owner's ask,
    /// and a range test that only checks `>= MIN` would still pass with `MIN` back at 1.
    ///
    /// The relations between the constants themselves are the two `const _: () = assert!`s
    /// beside them; this pins the value the owner asked for and what the clamp DOES with
    /// everything below it.
    #[test]
    fn the_floor_stops_short_of_one_to_one() {
        assert_eq!(MAGNIFIER_ZOOM_MIN, 3, "the least cell that still holds its own outline");
        assert_eq!(clamp_magnification(1), MAGNIFIER_ZOOM_MIN, "1:1 is off the bottom now");
        for below in 0..MAGNIFIER_ZOOM_MIN as i32 {
            assert_eq!(clamp_magnification(below), MAGNIFIER_ZOOM_MIN, "{below} is under the floor");
        }
        // And there is still a range to travel: the floor must not have met the ceiling.
        assert_ne!(MAGNIFIER_ZOOM_MIN, MAGNIFIER_ZOOM_DEFAULT);
        assert_eq!(zoom_after_step(MAGNIFIER_ZOOM_DEFAULT, -1), MAGNIFIER_ZOOM_DEFAULT - 1);
    }

    /// The DEFAULT is still today's magnification, and there is now room ABOVE it
    /// (DRAGON-601). Both halves matter: the picker must OPEN exactly as it always has, and
    /// zooming IN must actually go somewhere.
    ///
    /// This replaces a test that asserted the default EQUALLED the ceiling. That relationship
    /// is precisely what the owner asked to change, so keeping it would have been a test
    /// defending the bug rather than the behaviour.
    #[test]
    fn the_default_is_todays_magnification_with_headroom_above_it() {
        assert_eq!(MAGNIFIER_ZOOM_DEFAULT, MAGNIFIER_CELL, "the picker opens as it always did");
        // "the default is below the ceiling" is the module-scope `const _: () = assert!` beside
        // the constants, so it is not restated here; what this test adds is what the CLAMP
        // does with that headroom.
        // One notch in from the default is one notch in, not a no-op against a ceiling that
        // was sitting on top of it.
        assert_eq!(zoom_after_step(MAGNIFIER_ZOOM_DEFAULT, 1), MAGNIFIER_ZOOM_DEFAULT + 1);
        // And the ceiling is still a ceiling.
        assert_eq!(zoom_after_step(MAGNIFIER_ZOOM_DEFAULT, 1_000), MAGNIFIER_ZOOM_MAX);
    }

    /// The ceiling stated the way the floor's doc states itself: in SOURCE PIXELS the disc
    /// holds edge to edge. Pinned as a number because that is the reasoning the constant was
    /// chosen by, and a future change to the span or the cell would otherwise move it
    /// silently.
    #[test]
    fn the_two_ends_of_the_range_are_described_in_the_same_terms() {
        let span_at = |zoom: u32| MAGNIFIER_DIAMETER / zoom;
        assert_eq!(span_at(MAGNIFIER_ZOOM_MIN), 52, "the floor shows 52 pixels edge to edge");
        assert_eq!(span_at(MAGNIFIER_ZOOM_DEFAULT), 13, "the default shows the designed span");
        assert_eq!(span_at(MAGNIFIER_ZOOM_MAX), 6, "the ceiling shows 6 pixels edge to edge");
        // The sample keeps neighbours on every side at the tightest zoom, which is the whole
        // reason the ceiling is where it is rather than higher.
        assert!(span_at(MAGNIFIER_ZOOM_MAX) >= 5, "no context left to aim with");
    }

    /// THE convergence the owner asked to see: all three routes are a signed notch count, so
    /// they walk the same ladder and land on the same value. A route cannot reach further by
    /// sending a bigger number, and it cannot step by a different amount.
    #[test]
    fn every_route_converges_on_the_one_clamp() {
        // (route, notches) pairs. The trackpad's fractional deltas are accumulated into whole
        // notches by the widget, so what reaches the model is the same integer the wheel and
        // the keys send.
        let routes = [("trackpad", -1), ("wheel", -1), ("numpad minus", -1)];
        let from_default: Vec<u32> = routes
            .iter()
            .map(|(_, steps)| zoom_after_step(MAGNIFIER_ZOOM_DEFAULT, *steps))
            .collect();
        assert!(
            from_default.iter().all(|z| *z == MAGNIFIER_ZOOM_DEFAULT - 1),
            "one notch out is one notch out, whichever device sent it: {from_default:?}"
        );
        // And none of them can leave the range, however hard they push.
        for (name, _) in routes {
            for steps in [-1_000, -13, -1, 0, 1, 13, 1_000] {
                let z = zoom_after_step(MAGNIFIER_ZOOM_DEFAULT, steps);
                assert!(
                    (MAGNIFIER_ZOOM_MIN..=MAGNIFIER_ZOOM_MAX).contains(&z),
                    "{name} escaped the clamp with {steps} steps: {z}"
                );
                let z = zoom_after_step(MAGNIFIER_ZOOM_MIN, steps);
                assert!(
                    (MAGNIFIER_ZOOM_MIN..=MAGNIFIER_ZOOM_MAX).contains(&z),
                    "{name} escaped the clamp from the floor with {steps} steps: {z}"
                );
            }
            // DRAGON-598: and none of them can push UNDER the floor, which is the end the
            // owner actually asked about. Stated as its own assertion, because a range check
            // reads the same whether the floor is 3 or 1.
            for steps in [-1, -2, -13, -1_000, i32::MIN] {
                assert_eq!(
                    zoom_after_step(MAGNIFIER_ZOOM_MIN, steps),
                    MAGNIFIER_ZOOM_MIN,
                    "{name} zoomed out past the floor with {steps} steps"
                );
            }
            // DRAGON-601: the same guarantee at the NEW end. Every route must be able to
            // REACH the raised ceiling and must stop there, which is the pair of properties
            // that made raising it a constant change rather than new machinery.
            assert_eq!(
                zoom_after_step(MAGNIFIER_ZOOM_DEFAULT, 1_000),
                MAGNIFIER_ZOOM_MAX,
                "{name} could not reach the ceiling"
            );
            for steps in [1, 2, 13, 1_000, i32::MAX] {
                assert_eq!(
                    zoom_after_step(MAGNIFIER_ZOOM_MAX, steps),
                    MAGNIFIER_ZOOM_MAX,
                    "{name} zoomed in past the ceiling with {steps} steps"
                );
            }
        }
    }

    /// Walking to either end and back lands on exactly the value it should, so a user who
    /// zooms about cannot end up unable to get their picker back.
    ///
    /// DRAGON-601 changed the far end of this walk. Zooming all the way IN used to stop at the
    /// default, because the default WAS the ceiling; it now continues to the raised ceiling,
    /// and the walk back out to the default is spelled out rather than assumed.
    #[test]
    fn the_range_is_walkable_in_both_directions() {
        let steps = (MAGNIFIER_ZOOM_MAX - MAGNIFIER_ZOOM_MIN) as usize + 10;
        let mut z = MAGNIFIER_ZOOM_DEFAULT;
        for _ in 0..steps {
            z = zoom_after_step(z, -1);
        }
        assert_eq!(z, MAGNIFIER_ZOOM_MIN, "the floor is reachable from the default");
        for _ in 0..steps {
            z = zoom_after_step(z, 1);
        }
        assert_eq!(z, MAGNIFIER_ZOOM_MAX, "the ceiling is reachable from the floor");
        // And the default is reachable again from the ceiling, one notch at a time.
        for _ in 0..(MAGNIFIER_ZOOM_MAX - MAGNIFIER_ZOOM_DEFAULT) {
            z = zoom_after_step(z, -1);
        }
        assert_eq!(z, MAGNIFIER_ZOOM_DEFAULT, "the opening view is reachable again");
    }

    /// The disc keeps its ON-SCREEN size at every zoom: what changes is how much of the
    /// screen it holds. A lens that grew and shrank would move the hex label and the whole
    /// placement ladder around with it.
    #[test]
    fn the_disc_is_the_same_size_at_every_zoom() {
        let src = image::RgbaImage::from_pixel(400, 400, image::Rgba([9, 9, 9, 255]));
        for zoom in MAGNIFIER_ZOOM_MIN..=MAGNIFIER_ZOOM_MAX {
            let (d, buf) = plain(&src, (200, 200), zoom);
            assert_eq!(d, MAGNIFIER_DIAMETER, "zoom {zoom}");
            assert_eq!(buf.len(), (d as usize) * (d as usize) * 4, "zoom {zoom}");
        }
    }

    /// The SAMPLED pixel stays at the middle of the disc at EVERY zoom, which is the property
    /// the odd-span const assert guarantees at the default and the centre-relative cell maths
    /// generalises to the rest of the range.
    ///
    /// The probe is one pixel up-and-left of the geometric centre because the diameter is
    /// EVEN, so the disc's exact middle falls on a pixel boundary; that probe is inside the
    /// centre cell, and clear of its one-pixel marker, at every magnification.
    #[test]
    fn the_centre_of_the_disc_is_always_the_sampled_pixel() {
        // A source whose centre pixel is unique, so "did we read the right one" is decidable.
        let mut src = image::RgbaImage::from_pixel(400, 400, image::Rgba([0, 0, 255, 255]));
        src.put_pixel(200, 200, image::Rgba([255, 0, 0, 255]));
        for zoom in MAGNIFIER_ZOOM_MIN..=MAGNIFIER_ZOOM_MAX {
            let (d, buf) = plain(&src, (200, 200), zoom);
            let p = d / 2 - 1;
            let i = ((p as usize) * (d as usize) + p as usize) * 4;
            assert_eq!(
                [buf[i], buf[i + 1], buf[i + 2]],
                [255, 0, 0],
                "zoom {zoom}: the middle of the disc must be the pixel being reported"
            );
        }
    }

    /// DRAGON-598: at the FLOOR the marker still fits inside the sampled cell, so the picker
    /// neither paints over the colour it is reporting nor over its neighbours' colours.
    ///
    /// This is the property that chose the floor. The cell is three rendered pixels wide
    /// there, which is exactly enough for a one-pixel border with one pixel of colour left in
    /// the middle. One step narrower and the marker had to spill onto the ring of cells around
    /// the sample, hiding eight real colours to point at one; the assertion on the neighbour
    /// below is what would catch a return to that.
    #[test]
    fn the_floor_marks_the_pixel_without_covering_it_or_its_neighbours() {
        let mut src = image::RgbaImage::from_pixel(400, 400, image::Rgba([255, 255, 255, 255]));
        src.put_pixel(200, 200, image::Rgba([255, 0, 0, 255]));
        let zoom = MAGNIFIER_ZOOM_MIN;
        let (d, buf) = plain(&src, (200, 200), zoom);
        let at = |x: u32, y: u32| {
            let i = ((y as usize) * (d as usize) + x as usize) * 4;
            [buf[i], buf[i + 1], buf[i + 2]]
        };
        // The centre cell spans the disc's middle, so with an even diameter and an odd cell
        // its one interior pixel is at `d / 2 - 1`, with the outline either side of it.
        let p = d / 2 - 1;
        assert_eq!(at(p, p), [255, 0, 0], "the sampled pixel is visible");
        assert_eq!(at(p - 1, p), [0, 0, 0], "outlined on the left, inside its own cell");
        assert_eq!(at(p + 1, p), [0, 0, 0], "and on the right");
        // The cell NEXT DOOR still shows its own colour. This is what the sub-floor marker
        // used to destroy.
        assert_eq!(at(p + zoom, p), [255, 255, 255], "the neighbour keeps its colour");
    }
}

/// macOS: a pinch's magnification delta reduced to whole zoom notches, with a kept remainder.
#[cfg(test)]
mod pinch_zoom_tests {
    use super::*;

    /// A small pinch is a small fraction of a notch, and must not round up to one early.
    #[test]
    fn a_small_pinch_carries_as_a_remainder_with_no_notch_yet() {
        let (notches, remainder) = pinch_notches(0.0, 0.02);
        assert_eq!(notches, 0, "0.02 / 0.1 is a fifth of a notch, not a whole one yet");
        assert!((remainder - 0.2).abs() < 1e-4, "remainder was {remainder}");
    }

    /// Exactly one step of magnification is exactly one notch, with nothing left over.
    #[test]
    fn a_full_step_of_magnification_is_exactly_one_notch() {
        let (notches, remainder) = pinch_notches(0.0, PINCH_ZOOM_STEP);
        assert_eq!(notches, 1);
        assert!(remainder.abs() < 1e-4, "remainder was {remainder}");
    }

    /// Pinching IN (negative magnification) must zoom OUT, not clamp to zero: the sign carries
    /// through the whole conversion.
    #[test]
    fn pinching_in_steps_the_opposite_direction() {
        let (notches, _) = pinch_notches(0.0, -PINCH_ZOOM_STEP);
        assert_eq!(notches, -1);
    }

    /// Several drains too small to cross a notch on their own must still accumulate into one,
    /// exactly like the widget's own wheel-scroll accumulator (DRAGON-587): six drains of
    /// magnification 0.19 notches each cross the 1.0 mark once, between the fifth (0.95) and
    /// the sixth (1.14).
    #[test]
    fn slow_repeated_drains_accumulate_into_one_step_instead_of_rounding_away() {
        let mut accum = 0.0;
        let mut total_notches = 0;
        for _ in 0..6 {
            let (notches, remainder) = pinch_notches(accum, 0.019);
            accum = remainder;
            total_notches += notches;
        }
        assert_eq!(total_notches, 1);
    }

    /// A large, fast pinch can cross several notches in one drain; nothing here caps it (the
    /// clamp against the magnifier's own range is [`zoom_after_step`]'s job, not this one's).
    #[test]
    fn a_large_pinch_can_cross_several_notches_at_once() {
        let (notches, _) = pinch_notches(0.0, PINCH_ZOOM_STEP * 3.5);
        assert_eq!(notches, 3, "3.5 notches worth of magnification, 3 whole ones landed");
    }
}

/// DRAGON-615: a magnification read back from a config file, made safe for this build.
#[cfg(test)]
mod persisted_zoom_tests {
    use super::*;

    /// The ordinary case: a value the user actually set comes back untouched, or the
    /// remembering is pointless.
    #[test]
    fn a_value_inside_the_range_survives_exactly() {
        for zoom in MAGNIFIER_ZOOM_MIN..=MAGNIFIER_ZOOM_MAX {
            assert_eq!(zoom_from_persisted(zoom), zoom, "{zoom} is a legal magnification");
        }
    }

    /// THE regression this function exists for. `MAGNIFIER_ZOOM_MAX` was 12 before
    /// DRAGON-601 and is 26 now, so both shapes of config are in the wild. A value stored
    /// under the OLD ceiling must still be honoured exactly, because it is inside the range
    /// this build allows; clamping it would silently reset a preference that is perfectly
    /// valid.
    #[test]
    fn a_value_from_the_old_twelve_ceiling_is_still_honoured() {
        const OLD_CEILING: u32 = 12;
        // A relation between two constants, so it is checked at COMPILE time rather than as a
        // runtime assertion, the same shape as the module-scope `const _: () = assert!`s that
        // pin the zoom bounds themselves.
        const {
            assert!(
                OLD_CEILING < MAGNIFIER_ZOOM_MAX,
                "this test assumes the ceiling has risen since 12; re-derive it if it ever falls"
            )
        };
        for zoom in MAGNIFIER_ZOOM_MIN..=OLD_CEILING {
            assert_eq!(zoom_from_persisted(zoom), zoom);
        }
    }

    /// And the other direction: if a future build LOWERS the ceiling, every config written
    /// above the new one is pulled down to it rather than handing the magnifier a
    /// magnification it does not support.
    #[test]
    fn a_value_above_the_ceiling_is_pulled_down_to_it() {
        assert_eq!(zoom_from_persisted(MAGNIFIER_ZOOM_MAX + 1), MAGNIFIER_ZOOM_MAX);
        assert_eq!(zoom_from_persisted(1_000), MAGNIFIER_ZOOM_MAX);
        assert_eq!(zoom_from_persisted(u32::MAX), MAGNIFIER_ZOOM_MAX, "and never wraps negative");
        assert_eq!(zoom_from_persisted(i32::MAX as u32 + 1), MAGNIFIER_ZOOM_MAX, "at the cast edge");
    }

    /// Below the floor, including the `0` a hand-edited or truncated config can hold. Zero is
    /// the interesting one: it is what an absent-but-present key looks like, and applying it
    /// raw would ask for a magnifier with no magnification at all.
    #[test]
    fn a_value_below_the_floor_is_raised_to_it() {
        for zoom in 0..MAGNIFIER_ZOOM_MIN {
            assert_eq!(zoom_from_persisted(zoom), MAGNIFIER_ZOOM_MIN, "{zoom} is under the floor");
        }
    }

    /// Whatever a config holds, the result is always something the picker can render. This is
    /// the property that must survive any future bounds change, so it is asserted as a
    /// property rather than as a list of cases.
    #[test]
    fn every_possible_stored_value_lands_in_range() {
        let interesting = [
            0,
            1,
            MAGNIFIER_ZOOM_MIN - 1,
            MAGNIFIER_ZOOM_MIN,
            MAGNIFIER_ZOOM_DEFAULT,
            MAGNIFIER_ZOOM_MAX,
            MAGNIFIER_ZOOM_MAX + 1,
            12,
            13,
            i32::MAX as u32,
            u32::MAX,
        ];
        for stored in interesting {
            let got = zoom_from_persisted(stored);
            assert!(
                (MAGNIFIER_ZOOM_MIN..=MAGNIFIER_ZOOM_MAX).contains(&got),
                "stored {stored} produced {got}, outside the picker's range"
            );
        }
    }

    /// The persisted route must not become a second clamp with its own opinion: for every
    /// value both can express, it agrees with the one the three interactive routes share.
    #[test]
    fn it_agrees_with_the_shared_clamp() {
        for stored in 0..(MAGNIFIER_ZOOM_MAX + 5) {
            assert_eq!(zoom_from_persisted(stored), clamp_magnification(stored as i32));
        }
    }
}

#[cfg(test)]
mod label_placement_tests {
    use super::*;

    const LABEL: (f32, f32) = (86.0, 28.0);
    const R: f32 = 78.0;
    const GAP: f32 = 10.0;
    const VIEW: (f32, f32) = (1920.0, 1080.0);

    /// In open screen the label is BELOW the disc, and it stays there: the ladder means
    /// it does not shuffle sides as the pointer crosses the middle of the display.
    #[test]
    fn the_middle_of_the_screen_is_always_below() {
        for cursor in [(960.0, 540.0), (400.0, 300.0), (1500.0, 800.0), (960.0, 200.0)] {
            assert_eq!(
                label_placement(cursor, LABEL, R, GAP, VIEW),
                LabelPlacement::Below,
                "{cursor:?}"
            );
        }
    }

    /// Near the BOTTOM there is no room below, so it flips ABOVE.
    #[test]
    fn the_bottom_edge_flips_it_above() {
        let cursor = (960.0, 1075.0);
        assert_eq!(label_placement(cursor, LABEL, R, GAP, VIEW), LabelPlacement::Above);
        let (_, y) = label_origin(LabelPlacement::Above, cursor, LABEL, R, GAP, VIEW);
        assert!(y + LABEL.1 <= VIEW.1, "and it lands fully on screen");
    }

    /// A BOTTOM-LEFT corner has no room below and none above either (the disc plus the
    /// label is taller than the remaining space), so the ladder walks on to RIGHT.
    #[test]
    fn a_corner_walks_down_the_ladder() {
        // A short viewport makes both vertical placements impossible.
        let view = (1920.0, 200.0);
        let cursor = (960.0, 100.0);
        assert_eq!(label_placement(cursor, LABEL, R, GAP, view), LabelPlacement::Right);
        // And against the RIGHT wall of that same short viewport, LEFT is the answer.
        let cursor = (1900.0, 100.0);
        assert_eq!(label_placement(cursor, LABEL, R, GAP, view), LabelPlacement::Left);
    }

    /// When nothing fits at all the answer is still a placement, and the origin is
    /// clamped on screen: the user must always be able to read the colour.
    #[test]
    fn an_impossible_viewport_still_shows_the_label() {
        let view = (40.0, 40.0);
        let placement = label_placement((20.0, 20.0), LABEL, R, GAP, view);
        assert_eq!(placement, LabelPlacement::Below);
        let (x, y) = label_origin(placement, (20.0, 20.0), LABEL, R, GAP, view);
        // As far in as the box can go: flush left (it is wider than the viewport, so the
        // clamp range collapses to zero) and as low as the last 12 points allow.
        assert_eq!((x, y), (0.0, view.1 - LABEL.1), "clamped in, not off screen");
        assert!(x >= 0.0 && y >= 0.0, "never negative");
    }

    /// The origin is CLAMPED on the cross axis, so a Below label near the left wall
    /// slides right instead of hanging off it.
    #[test]
    fn the_origin_is_clamped_into_the_viewport() {
        let (x, y) = label_origin(LabelPlacement::Below, (5.0, 540.0), LABEL, R, GAP, VIEW);
        assert_eq!(x, 0.0, "slid right off the left wall");
        assert_eq!(y, 540.0 + R + GAP);
        let (x, _) = label_origin(LabelPlacement::Below, (1918.0, 540.0), LABEL, R, GAP, VIEW);
        assert_eq!(x, VIEW.0 - LABEL.0, "and left off the right wall");
    }

    /// THE DRAGON-587 regression pin: whatever the ladder chooses, wherever the pointer is,
    /// the label box and the magnifier's box do not intersect.
    ///
    /// It is asserted against the DISC'S OWN BOUNDS rather than against the viewport, because
    /// "inside the viewport" is what the old ladder already guaranteed and it was not enough:
    /// near the left wall the disc is pushed right (it cannot be drawn off-surface), the Right
    /// label measured from the cursor plus a radius, and the two overlapped. The sweep walks
    /// every wall and every corner, plus a short viewport that forces the horizontal rungs.
    #[test]
    fn every_placement_clears_the_circle() {
        for view in [VIEW, (1920.0, 200.0), (800.0, 600.0), (3840.0, 2160.0)] {
            // Corners, walls and the middle, at and just inside every edge.
            let xs = [0.0, 1.0, R, view.0 / 2.0, view.0 - R, view.0 - 1.0, view.0];
            let ys = [0.0, 1.0, R, view.1 / 2.0, view.1 - R, view.1 - 1.0, view.1];
            for x in xs {
                for y in ys {
                    let cursor = (x, y);
                    let placement = label_placement(cursor, LABEL, R, GAP, view);
                    let origin = label_origin(placement, cursor, LABEL, R, GAP, view);
                    assert!(
                        !overlaps_disc(origin, LABEL, cursor, R),
                        "{placement:?} at {cursor:?} in {view:?}: label {origin:?} overlaps \
                         the disc {:?}",
                        disc_rect(cursor, R)
                    );
                    // And the label itself is always fully on screen, which is the other
                    // half of the requirement: it may not clear the circle by leaving.
                    assert!(
                        origin.0 >= 0.0
                            && origin.1 >= 0.0
                            && origin.0 + LABEL.0 <= view.0
                            && origin.1 + LABEL.1 <= view.1,
                        "{placement:?} at {cursor:?} in {view:?}: label {origin:?} off screen"
                    );
                }
            }
        }
    }

    /// The ladder re-checked against the REAL pipeline, not against a hand-placed centre
    /// (DRAGON-587). The pointer goes in, the disc is centred on it, the label is placed against
    /// that disc, and the two must still not overlap.
    ///
    /// Worth its own test rather than trusting the sweep above to compose: the sweep chooses its
    /// own centres, so it would stay green even if the disc landed somewhere the ladder had never
    /// been asked about. This walks the pointer positions a user actually reaches, including
    /// every wall and corner.
    #[test]
    fn the_ladder_still_clears_the_circle_at_every_pointer_a_user_reaches() {
        for view in [VIEW, (1920.0, 200.0), (800.0, 480.0), (5120.0, 1440.0)] {
            let xs = [0.0, 0.5, 1.0, R, view.0 / 2.0, view.0 - R, view.0 - 1.0, view.0 - 0.001];
            let ys = [0.0, 0.5, 1.0, R, view.1 / 2.0, view.1 - R, view.1 - 1.0, view.1 - 0.001];
            for x in xs {
                for y in ys {
                    let pointer = (x, y);
                    let centre = pointer;
                    let placement = label_placement(centre, LABEL, R, GAP, view);
                    let origin = label_origin(placement, centre, LABEL, R, GAP, view);
                    assert!(
                        !overlaps_disc(origin, LABEL, centre, R),
                        "{placement:?} for pointer {pointer:?} in {view:?}: label {origin:?} \
                         overlaps the disc {:?}",
                        disc_rect(centre, R)
                    );
                    assert!(
                        origin.0 >= 0.0
                            && origin.1 >= 0.0
                            && origin.0 + LABEL.0 <= view.0
                            && origin.1 + LABEL.1 <= view.1,
                        "{placement:?} for pointer {pointer:?} in {view:?}: label {origin:?} \
                         is off screen, so the hex cannot be read"
                    );
                }
            }
        }
    }

    /// The reported case, spelled out on its own so the fix cannot be quietly undone by a
    /// later tweak that happens to keep the sweep green: hard against the LEFT wall, where the
    /// ladder walks past Below and Above (their box would start at a negative x) and lands on
    /// Right.
    ///
    /// The label is placed against the disc's TRUE box, which here runs off the screen to the
    /// left, and the DRAWN disc is that box clipped (DRAGON-587 item 7). So the label sits a
    /// gap past the circle's real right edge, and the circle keeps following the cursor.
    #[test]
    fn a_left_wall_pick_puts_the_label_clear_of_the_circle() {
        let cursor = (6.0, 540.0);
        assert_eq!(label_placement(cursor, LABEL, R, GAP, VIEW), LabelPlacement::Right);
        let (x, _) = label_origin(LabelPlacement::Right, cursor, LABEL, R, GAP, VIEW);
        assert_eq!(x, cursor.0 + R + GAP, "a gap past the circle's own right edge");
        // The circle stays centred on the pointer: its box runs negative, and what is drawn
        // is that box CLIPPED, never a box slid back on screen.
        let (l, _, r, _) = disc_rect(cursor, R);
        assert_eq!((l, r), (cursor.0 - R, cursor.0 + R));
        let view = disc_view(cursor, VIEW).expect("part of the disc is on screen");
        assert_eq!(view.origin.0, 0, "the visible part starts at the wall");
        assert_eq!(view.crop.0, (R - cursor.0).round() as u32, "and the rest is cut off");
        assert_eq!(
            view.size.0,
            MAGNIFIER_DIAMETER - view.crop.0,
            "the remainder is drawn at full scale, never squashed"
        );
    }

    /// Below and Above are centred on the cursor; Right and Left are centred on it
    /// vertically and clear the disc horizontally.
    #[test]
    fn each_placement_sits_where_its_name_says() {
        let c = (960.0, 540.0);
        assert_eq!(
            label_origin(LabelPlacement::Below, c, LABEL, R, GAP, VIEW),
            (960.0 - LABEL.0 / 2.0, 540.0 + R + GAP)
        );
        assert_eq!(
            label_origin(LabelPlacement::Above, c, LABEL, R, GAP, VIEW),
            (960.0 - LABEL.0 / 2.0, 540.0 - R - GAP - LABEL.1)
        );
        assert_eq!(
            label_origin(LabelPlacement::Right, c, LABEL, R, GAP, VIEW),
            (960.0 + R + GAP, 540.0 - LABEL.1 / 2.0)
        );
        assert_eq!(
            label_origin(LabelPlacement::Left, c, LABEL, R, GAP, VIEW),
            (960.0 - R - GAP - LABEL.0, 540.0 - LABEL.1 / 2.0)
        );
    }
}

/// What SPACE and ENTER do in this window, one case per stop (DRAGON-682 items 7, 9 and 32).
#[cfg(test)]
mod accept_key_tests {
    use super::*;

    /// The HISTORY applies (item 7), and needs a cursor to apply.
    #[test]
    fn the_history_stop_applies_its_cursor() {
        assert_eq!(
            accept_action(Some(PickerFocus::History), true, false),
            Some(AcceptAction::ApplyRecent)
        );
        assert_eq!(accept_action(Some(PickerFocus::History), false, true), None);
    }

    /// The PANEL copies (item 32, which replaced item 9's do-nothing rule), and needs a
    /// cursor of its own.
    #[test]
    fn the_panel_stop_copies_its_cursor() {
        assert_eq!(
            accept_action(Some(PickerFocus::Panel), false, true),
            Some(AcceptAction::CopySwatch)
        );
        assert_eq!(accept_action(Some(PickerFocus::Panel), true, false), None);
    }

    /// The ASYMMETRY, pinned on purpose: the same key, the same window, two meanings. It is
    /// the owner's choice (`accept_action`'s doc says why), so a change that "tidied" it
    /// into one action would break this test, which is the point.
    #[test]
    fn the_two_grids_mean_different_things() {
        let history = accept_action(Some(PickerFocus::History), true, true);
        let panel = accept_action(Some(PickerFocus::Panel), true, true);
        assert_eq!(history, Some(AcceptAction::ApplyRecent));
        assert_eq!(panel, Some(AcceptAction::CopySwatch));
        assert_ne!(history, panel);
    }

    /// Every OTHER stop passes the key straight through, which is what leaves Space to a
    /// focused value box.
    #[test]
    fn the_other_stops_do_not_take_the_key() {
        for focus in [None, Some(PickerFocus::Box(0)), Some(PickerFocus::Mode)] {
            assert_eq!(accept_action(focus, true, true), None, "{focus:?}");
        }
    }
}

/// The transient card a harmony swatch shows, and what beats what (DRAGON-682 item 30).
#[cfg(test)]
mod swatch_tip_tests {
    use super::*;

    /// COPIED wins over the cursor's own pinned hex card. Both are anchored at the same
    /// segment, so without an order they would draw on top of each other, and the owner
    /// asked for the copy confirmation to be the one that shows.
    #[test]
    fn copied_beats_the_cursor_card() {
        assert_eq!(swatch_tip(true, true, false), SwatchTip::Copied);
        assert_eq!(swatch_tip(true, false, false), SwatchTip::Copied);
    }

    /// With nothing copied, the keyboard cursor keeps its pinned card and everything else
    /// falls back to an ordinary hover tooltip.
    #[test]
    fn the_cursor_pins_and_the_rest_hover() {
        assert_eq!(swatch_tip(false, true, false), SwatchTip::PinnedHex);
        assert_eq!(swatch_tip(false, false, false), SwatchTip::Hover);
    }

    /// A live DRAG silences every card, whatever else is true of the swatch (DRAGON-682
    /// item 35): the ghost is the only thing that should be following the pointer.
    #[test]
    fn a_drag_silences_every_card() {
        for copied in [false, true] {
            for on_cursor in [false, true] {
                assert_eq!(
                    swatch_tip(copied, on_cursor, true),
                    SwatchTip::Silent,
                    "copied={copied} on_cursor={on_cursor}"
                );
            }
        }
    }
}

/// The DRAG machine's pure half (DRAGON-682 items 35 to 39): the threshold, the zones, and
/// the owner's drop matrix.
#[cfg(test)]
mod drag_tests {
    use super::*;

    fn collapsed() -> (f32, f32) {
        color_window_size()
    }
    fn expanded() -> (f32, f32) {
        color_window_size_expanded()
    }
    /// A point in the middle of the picker column, at height `y`.
    fn column(y: f32) -> (f32, f32) {
        (WINDOW_BORDER + picker_column_w() / 2.0, y)
    }
    /// The palettes tab showing, unscrolled, with these group sizes.
    fn palettes(groups: &[usize]) -> PanelShape {
        PanelShape { palettes: true, scroll: 0.0, groups: groups.to_vec() }
    }
    /// No droppable panel: collapsed, or the Harmonies tab.
    fn no_panel() -> PanelShape {
        PanelShape::default()
    }
    /// A point in the middle of group `g`'s BAR row, at the panel's unscrolled layout.
    fn bar_mid(g: usize) -> (f32, f32) {
        (
            WINDOW_BORDER + picker_column_w() + WINDOW_PADDING + bar_w() / 2.0,
            palettes_scroll_top()
                + palette_group_offset(g)
                + PALETTE_TITLE_ROW_H
                + PANEL_HEADING_GAP
                + PANEL_SWATCH / 2.0,
        )
    }

    /// The threshold is what keeps a CLICK a click: a press that wobbles is not a drag.
    #[test]
    fn a_press_becomes_a_drag_only_after_it_travels() {
        let o = (100.0, 100.0);
        assert!(!drag_is_live(o, o));
        assert!(!drag_is_live(o, (100.0 + DRAG_THRESHOLD, 100.0)));
        assert!(drag_is_live(o, (100.0 + DRAG_THRESHOLD + 0.1, 100.0)));
        // Diagonal travel counts as travel: the test is distance, not per-axis.
        let diag = DRAG_THRESHOLD / std::f32::consts::SQRT_2 + 0.5;
        assert!(drag_is_live(o, (100.0 + diag, 100.0 + diag)));
    }

    /// WHAT CAN ARM A DRAG, and what cannot (DRAGON-682 item 41).
    ///
    /// The press carries its own source, so this is the validity check, not a hit test: an
    /// identity that no longer names anything arms nothing.
    #[test]
    fn only_a_real_source_arms_a_drag() {
        assert!(arms_drag(DragSource::Active, 0, false, &no_panel()));
        assert!(arms_drag(DragSource::Recent(2), 3, false, &no_panel()));
        assert!(arms_drag(DragSource::Harmony(0, 0), 0, true, &no_panel()));
        // A history index past the end: the entry was removed between the press and here.
        assert!(!arms_drag(DragSource::Recent(3), 3, false, &no_panel()));
        assert!(!arms_drag(DragSource::Recent(0), 0, false, &no_panel()));
        // A harmony swatch cannot be pressed while the panel is not on screen.
        assert!(!arms_drag(DragSource::Harmony(0, 0), 5, false, &no_panel()));
        // The palette sources (DRAGON-687): live exactly while the palettes tab shows
        // them, and only for identities that still name something.
        let sh = palettes(&[2, 0]);
        assert!(arms_drag(DragSource::PaletteSwatch(0, 1), 0, true, &sh));
        assert!(!arms_drag(DragSource::PaletteSwatch(0, 2), 0, true, &sh), "past the end");
        assert!(!arms_drag(DragSource::PaletteSwatch(1, 0), 0, true, &sh), "an empty group");
        assert!(arms_drag(DragSource::PaletteName(1), 0, true, &sh), "an empty group still drags");
        assert!(!arms_drag(DragSource::PaletteName(2), 0, true, &sh), "a group that is gone");
        assert!(
            !arms_drag(DragSource::PaletteSwatch(0, 0), 0, true, &no_panel()),
            "no palettes tab, no palette source"
        );
    }

    /// **The negative cases the owner asked for**, and where they really live.
    ///
    /// A press on the strips, the SV square, a value box, an EMPTY history slot, the panel's
    /// background or the window's own chrome arms nothing, and not because a function says
    /// so: those places publish no press message at all. Exactly three widgets in the view
    /// carry `on_press` for this machine, and this test is the statement of that fact next to
    /// the machine it protects.
    ///
    /// * `view::segment` (a harmony swatch) sends `DragPressed(Harmony(group, index))`;
    /// * `view::recent_swatch` (a FILLED history entry) sends `DragPressed(Recent(index))`;
    ///   `view::empty_slot` is not a control at all and sends nothing;
    /// * `view::controls_row`'s round swatch sends `DragPressed(Active)`.
    ///
    /// The SV square and both strips are `widgets::color_field`, the value boxes are text
    /// inputs, and none of them knows this message exists. If a fourth press site is ever
    /// added, add it here.
    #[test]
    fn nothing_else_in_the_window_arms_a_drag() {
        // The whole vocabulary is five shapes, and every one of them names a swatch or a
        // group heading (DRAGON-687 added the two palette shapes; `view::palette_segment`
        // and the group heading's mouse area are their only press sites).
        let every_source = [
            DragSource::Active,
            DragSource::Recent(0),
            DragSource::Harmony(0, 0),
            DragSource::PaletteSwatch(0, 0),
            DragSource::PaletteName(0),
        ];
        assert_eq!(every_source.len(), 5);
        // Each is valid only while the thing it names exists, which is the closest a pure
        // test can get to "a press somewhere else does nothing".
        assert!(
            !arms_drag(DragSource::Recent(0), 0, true, &no_panel()),
            "an empty slot is not a source"
        );
        assert!(
            !arms_drag(DragSource::Harmony(0, 0), 18, false, &no_panel()),
            "no panel, no harmony source"
        );
    }

    /// The BOUNDARY the owner named: everything above the divider row is the tool area, and
    /// the row itself belongs to the recents (it carries the "Add to recents" button).
    #[test]
    fn the_divider_row_is_the_boundary() {
        let w = collapsed();
        let np = no_panel();
        let d = divider_band_top();
        assert_eq!(drop_zone(column(d - 1.0), w, &np), Some(DropZone::Main));
        assert_eq!(drop_zone(column(d), w, &np), Some(DropZone::Recents));
        assert_eq!(drop_zone(column(d + DIVIDER_BAND_H / 2.0), w, &np), Some(DropZone::Recents));
        // The whole tool area answers Main, top to bottom.
        assert_eq!(drop_zone(column(WINDOW_BORDER + header_h()), w, &np), Some(DropZone::Main));
        assert_eq!(drop_zone(column(d - SV_H), w, &np), Some(DropZone::Main));
        // And the recents run to the bottom of the content.
        assert_eq!(
            drop_zone(column(w.1 - WINDOW_BORDER - 1.0), w, &np),
            Some(DropZone::Recents)
        );
    }

    /// The window's own EDGES: the header, the frosted border and anything past the frame
    /// are not zones at all.
    #[test]
    fn the_chrome_is_not_a_drop_zone() {
        let w = collapsed();
        let np = no_panel();
        for y in [0.0, WINDOW_BORDER, header_h(), WINDOW_BORDER + header_h() - 0.1] {
            assert_eq!(drop_zone(column(y), w, &np), None, "y={y}");
        }
        assert_eq!(drop_zone(column(w.1 - WINDOW_BORDER), w, &np), None);
        assert_eq!(drop_zone((-1.0, 200.0), w, &np), None);
        assert_eq!(drop_zone((w.0 + 1.0, 200.0), w, &np), None);
    }

    /// The PANEL half takes a drop only while Saved Palettes is showing, only under the
    /// create row, and PER GROUP since DRAGON-687: a group's block answers that group, the
    /// gaps answer the strip, and hitting all of it goes through the SCROLL offset.
    #[test]
    fn the_panel_takes_a_drop_only_on_saved_palettes() {
        let w = expanded();
        let sh = palettes(&[3, 1]);
        let x = WINDOW_BORDER + picker_column_w() + panel_w() / 2.0;
        assert_eq!(drop_zone(bar_mid(0), w, &sh), Some(DropZone::PaletteGroup(0)));
        assert_eq!(drop_zone(bar_mid(1), w, &sh), Some(DropZone::PaletteGroup(1)));
        assert_eq!(drop_zone(bar_mid(0), w, &no_panel()), None, "harmonies takes no drop");
        // Above the scroll area (the create row and the tab strip) is not a target.
        assert_eq!(drop_zone((x, palettes_scroll_top() - 1.0), w, &sh), None);
        // The gap between the two groups is the STRIP, not a guessed group.
        let gap_y = palettes_scroll_top() + palette_group_h() + PANEL_GROUP_GAP / 2.0;
        assert_eq!(drop_zone((x, gap_y), w, &sh), Some(DropZone::PaletteStrip));
        // The run-out below the last group is the strip too.
        let below = palettes_scroll_top() + palettes_content_h(2) + 4.0;
        assert_eq!(drop_zone((x, below), w, &sh), Some(DropZone::PaletteStrip));
        // SCROLLED: the same window point now hits the group the scroll brought under it.
        let pitch = palette_group_h() + PANEL_GROUP_GAP;
        let scrolled = PanelShape { scroll: pitch, ..sh.clone() };
        assert_eq!(drop_zone(bar_mid(0), w, &scrolled), Some(DropZone::PaletteGroup(1)));
        // The picker column is unaffected by which tab is showing.
        assert_eq!(drop_zone(column(divider_band_top() - 1.0), w, &sh), Some(DropZone::Main));
    }

    /// Off the window is a real answer, and it is the only one that can reach past the
    /// frame.
    #[test]
    fn a_release_past_the_frame_is_off_the_window() {
        let w = collapsed();
        assert!(!off_window((1.0, 1.0), w));
        assert!(!off_window((w.0 - 1.0, w.1 - 1.0), w));
        assert!(off_window((-1.0, 100.0), w));
        assert!(off_window((100.0, -1.0), w));
        assert!(off_window((w.0, 100.0), w));
        assert!(off_window((100.0, w.1), w));
    }

    /// THE MATRIX (item 38), source by source, exactly as the owner stated it, with
    /// DRAGON-687's palette rows and columns.
    #[test]
    fn the_drop_matrix_is_the_owners_table() {
        use DragSource::{Active, Harmony, PaletteName, PaletteSwatch, Recent};
        use DropAction as A;
        use DropZone::{Main, PaletteGroup, PaletteStrip, Recents};
        let sh = palettes(&[3, 2]);
        let at = (0.0, 0.0);
        // A harmony swatch: set active above the divider, file it below.
        assert_eq!(drop_action(Harmony(0, 1), Some(Main), at, &sh, false), Some(A::SetActive));
        assert_eq!(
            drop_action(Harmony(0, 1), Some(Recents), at, &sh, false),
            Some(A::AddToRecents)
        );
        assert_eq!(drop_action(Harmony(0, 1), None, at, &sh, true), None);
        // The ACTIVE swatch: filing is the only thing it can do, since the tools already
        // show it.
        assert_eq!(drop_action(Active, Some(Recents), at, &sh, false), Some(A::AddToRecents));
        assert_eq!(drop_action(Active, Some(Main), at, &sh, false), None);
        assert_eq!(drop_action(Active, None, at, &sh, true), None);
        // A history entry: load it above the divider, forget it off the window, and dropping
        // it back on the history changes nothing.
        assert_eq!(drop_action(Recent(4), Some(Main), at, &sh, false), Some(A::LoadRecent(4)));
        assert_eq!(drop_action(Recent(4), Some(Recents), at, &sh, false), None);
        assert_eq!(drop_action(Recent(4), None, at, &sh, true), Some(A::RemoveRecent(4)));
        // A saved palette's GROUP takes every colour, APPENDED (DRAGON-687): the three
        // original sources append, a colour of ANOTHER group moves, and its own group
        // reorders (below, where the slot needs a real position).
        for source in [Harmony(0, 0), Active, Recent(0)] {
            assert_eq!(
                drop_action(source, Some(PaletteGroup(1)), at, &sh, false),
                Some(A::AppendToPalette(1)),
                "{source:?}"
            );
        }
        // A colour of ANOTHER group COPIES (the owner's reversal: the drag is the safe
        // gesture; the menu's Move stays the vacating form).
        assert_eq!(
            drop_action(PaletteSwatch(0, 2), Some(PaletteGroup(1)), at, &sh, false),
            Some(A::CopyToPalette { from: (0, 2), to: 1 })
        );
        // A palette colour on the picker column: set active WITHOUT filing (a saved
        // colour is loaded, not derived), and file it on the recents half.
        assert_eq!(
            drop_action(PaletteSwatch(0, 1), Some(Main), at, &sh, false),
            Some(A::SetActiveNoFile)
        );
        assert_eq!(
            drop_action(PaletteSwatch(0, 1), Some(Recents), at, &sh, false),
            Some(A::AddToRecents)
        );
        // ...and off the window it is forgotten, with no confirmation (colours never
        // confirm; groups do).
        assert_eq!(
            drop_action(PaletteSwatch(1, 0), None, at, &sh, true),
            Some(A::RemovePaletteColor { group: 1, index: 0 })
        );
        // A group NAME: reorders on the strip, asks to delete off the window, and means
        // nothing on the picker column.
        assert_eq!(
            drop_action(PaletteName(0), None, at, &sh, true),
            Some(A::DeleteGroupRequest(0))
        );
        assert_eq!(drop_action(PaletteName(0), Some(Main), at, &sh, false), None);
        assert_eq!(drop_action(PaletteName(0), Some(Recents), at, &sh, false), None);
        // A COLOUR dropped in the strip's gaps names no group: cancel, never a guess.
        for source in [Harmony(0, 0), Active, Recent(0), PaletteSwatch(0, 0)] {
            assert_eq!(
                drop_action(source, Some(PaletteStrip), at, &sh, false),
                None,
                "{source:?}"
            );
        }
        // A release over nothing is a cancel for everyone.
        for source in [Harmony(0, 0), Active, Recent(0), PaletteSwatch(0, 0), PaletteName(0)] {
            assert_eq!(drop_action(source, None, at, &sh, false), None, "{source:?}");
        }
    }

    /// The two REORDER answers resolve their slot from the release position, and the slots
    /// that would put the thing back where it is answer a cancel (so the highlight follows
    /// them off too).
    #[test]
    fn a_reorder_reads_its_slot_from_the_release() {
        use DropAction as A;
        let w = expanded();
        let sh = palettes(&[3, 2]);
        // A colour of group 0 released at its own bar's far RIGHT: insertion slot 3.
        let bar_left = WINDOW_BORDER + picker_column_w() + WINDOW_PADDING;
        let y = bar_mid(0).1;
        let far_right = (bar_left + bar_w() - 1.0, y);
        assert_eq!(
            drop_action(
                DragSource::PaletteSwatch(0, 0),
                Some(DropZone::PaletteGroup(0)),
                far_right,
                &sh,
                false
            ),
            Some(A::ReorderPaletteColor { group: 0, from: 0, to: 3 })
        );
        // Released over its own first segment: the no-move slots cancel.
        let over_self = (bar_left + 1.0, y);
        assert_eq!(
            drop_action(
                DragSource::PaletteSwatch(0, 0),
                Some(DropZone::PaletteGroup(0)),
                over_self,
                &sh,
                false
            ),
            None
        );
        // A NAME released over the bottom of the strip: insertion slot 2 (after both).
        let below = (bar_mid(0).0, palettes_scroll_top() + palettes_content_h(2) + 2.0);
        assert_eq!(
            drop_action(DragSource::PaletteName(0), Some(DropZone::PaletteStrip), below, &sh, false),
            Some(A::ReorderGroup { from: 0, to: 2 })
        );
        // ...and over its own block, the no-move slots cancel.
        assert_eq!(
            drop_action(
                DragSource::PaletteName(0),
                Some(DropZone::PaletteGroup(0)),
                bar_mid(0),
                &sh,
                false
            ),
            None
        );
        let _ = w;
    }

    /// Off the window WINS over any zone, because the two can be reported together by a
    /// release the platform locates past the frame while a stale zone would still match.
    #[test]
    fn off_the_window_decides_on_its_own() {
        let sh = palettes(&[2]);
        assert_eq!(
            drop_action(DragSource::Recent(2), Some(DropZone::Main), (0.0, 0.0), &sh, true),
            Some(DropAction::RemoveRecent(2))
        );
        assert_eq!(
            drop_action(DragSource::Harmony(1, 1), Some(DropZone::Main), (0.0, 0.0), &sh, true),
            None
        );
    }

    /// The ghost is centred on the pointer, in EVERY direction, clamped nowhere.
    ///
    /// The owner reported the clamped version: "we can't drag a swatch above the window or
    /// beyond the left edge". Those are exactly the two directions a padding-based placement
    /// cannot express, so the contract is stated here and `widgets::positioned` is what
    /// renders it.
    #[test]
    fn the_ghost_is_centred_on_the_pointer() {
        let half = DRAG_GHOST / 2.0;
        for at in [
            (100.0, 200.0),
            (0.0, 0.0),
            (-5.0, -5.0),
            (-400.0, 30.0),
            (30.0, -400.0),
            (5_000.0, 5_000.0),
        ] {
            let (x, y) = ghost_origin(at);
            assert!((x + half - at.0).abs() < 0.01, "x at {at:?}");
            assert!((y + half - at.1).abs() < 0.01, "y at {at:?}");
        }
        // Negative origins really are produced, in both axes: a clamp anywhere in here is
        // the bug coming back.
        assert!(ghost_origin((0.0, 0.0)).0 < 0.0);
        assert!(ghost_origin((0.0, 0.0)).1 < 0.0);
    }

    /// A CLICK on a history swatch is a press and a release on the same swatch with nothing
    /// in between (item 41, now that the swatch is not a button).
    #[test]
    fn a_click_is_a_press_and_a_release_on_one_swatch() {
        use DragSource::{Active, Recent};
        assert!(completes_click(Some((Recent(2), false)), Recent(2)));
        // A real drag that ended here is not a click, or dropping a swatch back where it
        // started would load it as well.
        assert!(!completes_click(Some((Recent(2), true)), Recent(2)));
        // A release over a swatch whose press happened somewhere else is not a click.
        assert!(!completes_click(Some((Recent(1), false)), Recent(2)));
        assert!(!completes_click(Some((Active, false)), Recent(2)));
        assert!(!completes_click(None, Recent(2)));
    }

    /// The highlight and the ACTION are one decision: every zone that lights up would really
    /// do something, and every zone that would do something lights up.
    #[test]
    fn the_highlight_is_the_drop_matrix() {
        let w = expanded();
        let sh = palettes(&[3, 2]);
        let strip_gap_y = palettes_scroll_top() + palette_group_h() + PANEL_GROUP_GAP / 2.0;
        let probes = [
            column(divider_band_top() - 10.0),
            column(divider_band_top() + 10.0),
            bar_mid(0),
            bar_mid(1),
            (bar_mid(0).0, strip_gap_y),
            column(WINDOW_BORDER),
        ];
        for source in [
            DragSource::Harmony(0, 0),
            DragSource::Active,
            DragSource::Recent(0),
            DragSource::PaletteSwatch(1, 0),
            DragSource::PaletteName(1),
        ] {
            for at in probes {
                let lit = zone_highlight(source, at, w, &sh);
                let zone = drop_zone(at, w, &sh);
                let acts = drop_action(source, zone, at, &sh, false).is_some();
                assert_eq!(
                    lit.is_some(),
                    acts,
                    "{source:?} at {at:?}: lit={lit:?} but action={acts}"
                );
                if let Some(lit) = lit {
                    assert_eq!(Some(lit), zone);
                }
            }
        }
    }

    /// A zone's RECTANGLE and its hit test describe the same region: the middle AND the
    /// inner corners of every rectangle hit-test back to the zone they came from. A
    /// palette group's rectangle is the WHOLE block since the owner widened it (title
    /// row plus bar, one rect), so it round-trips corner to corner exactly like the
    /// picker column's zones, which is the alignment-by-construction the widening buys.
    #[test]
    fn zone_round_trips_its_rect() {
        let w = expanded();
        let sh = palettes(&[3, 2]);
        for zone in [
            DropZone::Main,
            DropZone::Recents,
            DropZone::PaletteGroup(0),
            DropZone::PaletteGroup(1),
        ] {
            let (x, y, rw, rh) = zone_rect(zone, w, &sh);
            assert!(rw > 0.0 && rh > 0.0, "{zone:?} has no area");
            let probes = [
                (x + rw / 2.0, y + rh / 2.0),
                (x + 1.0, y + 1.0),
                (x + rw - 1.0, y + rh - 1.0),
            ];
            for at in probes {
                assert_eq!(drop_zone(at, w, &sh), Some(zone), "{zone:?} at {at:?}");
            }
        }
        // The group rect IS the laid-out group: the same offset and height the view
        // builds from, no interior offset re-derived between them (the 1-to-2px class
        // of drift this replaces), clipped to the viewport like everything in it.
        let (_, y, rw, rh) = zone_rect(DropZone::PaletteGroup(1), w, &sh);
        assert_eq!(y, palettes_scroll_top() + palette_group_offset(1));
        assert_eq!(rh, palette_group_h());
        assert_eq!(rw, card_w(), "the title row and the bar share the card's width");
        // A group scrolled fully past the fold has no visible rectangle left.
        let far = PanelShape { scroll: 10_000.0, ..sh.clone() };
        let (_, _, _, rh) = zone_rect(DropZone::PaletteGroup(0), w, &far);
        assert!(rh <= 0.0, "an off-screen group must not draw a highlight");
    }

    /// Item five's ending-to-tab table: exactly the two into-a-palette drops COMMIT
    /// the transient Saved Palettes activation, and every other ending reverts. Every
    /// variant is named, so a new ending must choose a row here.
    #[test]
    fn palette_drop_endings_commit_the_tab_and_nothing_else_does() {
        let commits = [
            DropAction::AppendToPalette(0),
            DropAction::CopyToPalette { from: (0, 1), to: 2 },
        ];
        let reverts = [
            DropAction::SetActive,
            DropAction::SetActiveNoFile,
            DropAction::AddToRecents,
            DropAction::LoadRecent(3),
            DropAction::RemoveRecent(3),
            DropAction::ReorderPaletteColor { group: 0, from: 1, to: 2 },
            DropAction::RemovePaletteColor { group: 0, index: 1 },
            DropAction::ReorderGroup { from: 0, to: 1 },
            DropAction::DeleteGroupRequest(0),
        ];
        for a in commits {
            assert!(drop_commits_palette_tab(a), "{a:?} must keep the palettes tab");
        }
        for a in reverts {
            assert!(!drop_commits_palette_tab(a), "{a:?} must restore the pre-drag tab");
        }
    }

    /// The drag-jump round's item three: the dashed OUTLINE always has the wash's own
    /// rect. The wash is `zone_rect` read live; the outline is a cached raster whose
    /// key is now that rect's size (`zone_raster_size`), so walking a group through
    /// the viewport, sliver at the fold, growing per scroll step, full, clipping out
    /// at the other edge, demands a rebuild at every size change and the cache can
    /// never serve a stale size. The stale key it replaces was the zone's IDENTITY,
    /// which is constant across that whole walk. Filtered shapes hold by construction:
    /// `zone_rect` reads the same visible-row `PanelShape` the wash does.
    #[test]
    fn the_zone_outline_always_matches_the_wash_rect() {
        let w = expanded();
        for groups in [vec![3usize, 2, 4, 1, 2, 5], vec![2usize, 1]] {
            let base = palettes(&groups);
            let mut cached: Option<(u32, u32)> = None;
            let mut sizes_seen = std::collections::HashSet::new();
            let max = palettes_max_scroll(w.1, groups.len());
            let mut scroll = 0.0;
            while scroll <= max + 60.0 {
                let sh = PanelShape { scroll, ..base.clone() };
                for g in 0..groups.len() {
                    let (_, _, rw, rh) = zone_rect(DropZone::PaletteGroup(g), w, &sh);
                    if rw <= 0.0 || rh <= 0.0 {
                        continue; // no highlight is drawn at all
                    }
                    // The refresh rule: rebuild exactly when the cached size differs.
                    if let Some(want) = zone_raster_size(cached, rw, rh) {
                        cached = Some(want);
                    }
                    let want = (rw.round().max(1.0) as u32, rh.round().max(1.0) as u32);
                    assert_eq!(
                        cached,
                        Some(want),
                        "groups={groups:?} scroll={scroll} g={g}: the outline would \
                         draw at {cached:?} under a wash of {want:?}"
                    );
                    sizes_seen.insert(want);
                }
                scroll += 7.0;
            }
            assert!(
                sizes_seen.len() > 2,
                "the walk never produced clipped rects, so it pinned nothing"
            );
        }
    }

    /// The zone maths and the window's own height are built from the same stack, so a change
    /// to one that forgets the other fails here.
    #[test]
    fn drop_zone_matches_the_window_height() {
        let expected = divider_band_top()
            + DIVIDER_BAND_H
            + SECTION_GAP
            + 2.0 * RECENT_SWATCH
            + recents_gap()
            + WINDOW_PADDING
            + WINDOW_BORDER
            // The window's own bit of give, which sits under the grid rather than above it,
            // so it moves nothing this function names.
            + LAYOUT_SLACK_H;
        assert!(
            (expected - color_window_size().1).abs() < 0.01,
            "the divider's y and the window's height disagree: {expected} vs {}",
            color_window_size().1
        );
    }
}

#[cfg(test)]
mod window_size_tests {
    use super::*;

    /// The permission window's width (`app::permissions::open_permissions_window`), the
    /// reference the owner named at DRAGON-582. A test-only constant: production code
    /// derives the picker's size from its own parts, and the only thing this is for is
    /// checking the result stayed in the neighbourhood the brief described.
    const PERMISSIONS_WINDOW_W: f32 = 629.0;

    /// The width is the CONTENT width plus padding, still bigger than the brief's naive
    /// half and still smaller than the window it derived from. Pinned so a later tweak
    /// has to be a deliberate one.
    #[test]
    fn the_width_is_the_content_plus_padding() {
        let (w, _) = color_window_size();
        assert_eq!(w, 422.0);
        assert_eq!(w, 2.0 * (WINDOW_BORDER + WINDOW_PADDING) + CONTENT_W);
        assert!(w > PERMISSIONS_WINDOW_W / 2.0);
        assert!(w < PERMISSIONS_WINDOW_W, "still smaller than the window it derives from");
        assert!(
            w < 510.0,
            "and narrower than the one-line value row that carried the chip and both \
             icon buttons beside the boxes"
        );
    }

    /// CONTENT_W's whole reason for being 332: the history grid lands on whole points, so
    /// nothing floors away from its flush right edge, and the value boxes then span their
    /// own budget exactly.
    #[test]
    fn the_content_width_divides_evenly() {
        assert_eq!(recents_gap(), 17.0, "eight whole-point history gaps");
        assert_eq!(value_box_width(5), 55.0, "CMYK's five boxes floor to a readable 55");
    }

    /// DRAGON-680 item 23: the copy button leads the value row, and [`CONTENT_W`] was
    /// picked so that costs the BOXES nothing. Pinned as the two numbers that would have
    /// moved if it had been picked by eye: the box budget, and CMYK's own box width.
    #[test]
    fn the_copy_button_leader_costs_the_boxes_nothing() {
        assert_eq!(value_boxes_total(), 300.0, "the boxes keep the budget they had");
        assert_eq!(value_box_width(5), 55.0, "and CMYK's five keep their width");
        assert_eq!(value_box_width(4), 70.0);
        // The row really does spend the leader and the activator, or the budget above is
        // the whole content width by accident.
        assert_eq!(
            CONTROLS_BUTTON + ROW_SPACING + value_boxes_total() + ROW_SPACING + MODE_STEP_W,
            CONTENT_W,
            "the value row is flush (copy, boxes, activator)"
        );
        // Squeezing instead of widening is what this rejects: at the OLD content width the
        // same row would have left CMYK's boxes unusable, well under the 54pt floor the
        // readability test holds every mode to.
        let squeezed = 332.0 - CONTROLS_BUTTON - MODE_STEP_W - 2.0 * ROW_SPACING;
        assert!(
            ((squeezed - 4.0 * BOX_GAP) / 5.0).floor() < 54.0,
            "the old width would have fitted CMYK, so widening was not forced"
        );
    }

    /// The box BAND is the copy button's height, not a box's, and the value row grew by
    /// exactly that difference (DRAGON-680 item 23).
    #[test]
    fn the_box_band_is_as_tall_as_its_tallest_control() {
        assert_eq!(BOX_BAND_H, CONTROLS_BUTTON);
        // The button really is the tallest thing on the row, which is a compile-time fact
        // (a const assert beside `BOX_BAND_H` fails the BUILD if a box ever grows past it).
        const { assert!(BOX_BAND_H > VALUE_BOX_H, "the button is not the tallest control") };
        assert_eq!(VALUE_ROW_H, BOX_BAND_H + VALUE_LABEL_GAP + VALUE_LABEL_H);
        assert_eq!(
            VALUE_ROW_H - (VALUE_BOX_H + VALUE_LABEL_GAP + VALUE_LABEL_H),
            CONTROLS_BUTTON - VALUE_BOX_H,
            "the row grew by the button's overhang and nothing else"
        );
    }

    /// Every box row is FLUSH at both edges of the BOX BUDGET: the floor()'s remainder is
    /// handed out a point at a time rather than dropped, so the last box ends exactly
    /// where the mode stepper's gap begins. And no two boxes in a row differ by more than
    /// that single point.
    #[test]
    fn the_box_row_spans_its_budget_exactly() {
        for boxes in 1..=6usize {
            let widths = value_box_widths(boxes);
            assert_eq!(widths.len(), boxes);
            let span: f32 = widths.iter().sum::<f32>() + (boxes as f32 - 1.0) * BOX_GAP;
            assert_eq!(span, value_whole_width(), "{boxes} boxes span the box budget");
            let (min, max) = (
                widths.iter().cloned().fold(f32::MAX, f32::min),
                widths.iter().cloned().fold(0.0, f32::max),
            );
            assert!(max - min <= 1.0, "{boxes} boxes: {min} vs {max} is a visible step");
            assert!(widths.iter().all(|w| w.fract() == 0.0), "whole points only");
        }
    }

    /// The box row carries the boxes and TWO neighbours: the copy button leading it
    /// (DRAGON-680 item 23) and the mode activator closing it.
    ///
    /// Pinned as the arithmetic rather than as a number, because every point either
    /// control takes comes out of the boxes, and the window's width was chosen from what
    /// is left ([`CONTENT_W`]). Putting anything else on this row makes every box
    /// narrower, and this is where that shows up.
    #[test]
    fn the_box_row_carries_the_boxes_and_its_two_controls() {
        assert_eq!(
            value_whole_width(),
            CONTENT_W - CONTROLS_BUTTON - MODE_STEP_W - 2.0 * ROW_SPACING
        );
        assert_eq!(
            CONTROLS_BUTTON + ROW_SPACING + value_whole_width() + ROW_SPACING + MODE_STEP_W,
            CONTENT_W
        );
    }

    /// The stepper is exactly one value box tall, so the boxes stay the tallest thing in
    /// their band and the row's height is still theirs (the same rule the mode row's icon
    /// buttons used to follow). Doubly pinned: the const assert beside [`MODE_STEP_GAP`]
    /// fails the BUILD, and this fails the suite with the numbers printed.
    #[test]
    fn the_chevron_pair_is_one_value_box_tall() {
        assert_eq!(2.0 * MODE_STEP_H + MODE_STEP_GAP, VALUE_BOX_H);
        assert!(
            f32::from(MODE_STEP_ICON) < MODE_STEP_H,
            "the chevron glyph has to fit inside its own button"
        );
    }

    /// The height is the sum of its parts (DRAGON-630's stack, rev 2's gaps, divider
    /// and second history row included), so nothing scrolls. Recomputed here from the
    /// same constants, which is what would catch a part being dropped from the sum.
    #[test]
    fn the_height_is_the_sum_of_the_parts() {
        let (_, h) = color_window_size();
        let want = 2.0 * WINDOW_BORDER
            + header_h()
            + 2.0 * WINDOW_PADDING
            + SV_H
            + GAP_SQUARE_CONTROLS
            + CONTROLS_H
            + GAP_CONTROLS_VALUE
            + VALUE_ROW_H
            + GAP_VALUE_DIVIDER
            + DIVIDER_BAND_H
            + SECTION_GAP
            + 2.0 * RECENT_SWATCH
            + recents_gap()
            + LAYOUT_SLACK_H;
        assert_eq!(h, want);
        // 582 before DRAGON-680, 542 once the mode row was deleted, 563 once item 23's
        // copy button made the box band 14pt taller, all written while the header was
        // the 44pt fiction; the height follows the REAL header now (`header_h`), so the
        // pin is header-relative: the rest of the stack is the 519pt those numbers
        // always contained.
        assert_eq!(h, header_h() + 519.0);
        const {
            assert!(
                DIVIDER_BAND_H >= DIVIDER_H,
                "the divider's band has to hold the line it is named for"
            )
        };
    }

    /// Every section fits the content width EXACTLY where the owner asked for
    /// alignment: the controls row is flush, and a full history row's last swatch lands
    /// on the content's right edge, which is the tracks' right edge.
    #[test]
    fn the_rows_align_to_the_content_edges() {
        // The copy button left this row for the value row (DRAGON-680 item 23), so it is
        // the pipette, the swatch and the tracks again, and the tracks took every point
        // the button was using.
        assert_eq!(
            CONTROLS_BUTTON + ROW_SPACING + SWATCH_CIRCLE + GAP_SWATCH_TRACKS + STRIPS_W,
            CONTENT_W,
            "the controls row is flush (pipette, swatch, tracks)"
        );
        let row = RECENTS_PER_ROW as f32 * RECENT_SWATCH
            + (RECENTS_PER_ROW as f32 - 1.0) * recents_gap();
        assert!(
            (row - CONTENT_W).abs() < 0.01,
            "a full history row spans the content exactly ({row} vs {CONTENT_W})"
        );
        assert!(recents_gap() > 0.0, "the swatches do not overlap");
        // The floor is a const assert now (DRAGON-680 re-derived it at 150 when the copy
        // button joined this row and took 56pt of track). Restated here so the number a
        // reader sees beside the row arithmetic is the one the build enforces.
        const { assert!(STRIPS_W >= 150.0, "the strips keep enough travel to aim a hue in") };
        assert_eq!(STRIPS_W, 268.0, "the longest tracks the picker has had");
    }

    /// DRAGON-680: the history's focus frame is paid for by the MARGINS it sits in, so it
    /// can be drawn outside the swatches (the owner's veto of a frame that clipped their
    /// rims) without moving anything or changing the window's size.
    ///
    /// This is the whole of that claim, in both axes, and it is worth pinning as
    /// arithmetic rather than trusting the layout code: the payment is spread over three
    /// different numbers in `view` (the gap above the history, the window padding, and the
    /// horizontal padding handed back to every section above), and getting one of them
    /// wrong shows up as a window whose last row is squeezed, which is exactly the defect
    /// `LAYOUT_SLACK_H` exists because of.
    #[test]
    fn the_history_frame_is_paid_for_by_the_margins_it_sits_in() {
        let o = HISTORY_FOCUS_OUTSET;
        let block = 2.0 * RECENT_SWATCH + recents_gap();
        // VERTICAL: the gap above the history and the window's bottom padding each give up
        // the outset, and the frame's own inset gives both back.
        let before = SECTION_GAP + block + WINDOW_PADDING;
        let after = (SECTION_GAP - o) + (block + 2.0 * o) + (WINDOW_PADDING - o);
        assert_eq!(after, before, "the frame changed the window's height");
        // HORIZONTAL: the window padding gives up the outset on both sides, the frame's
        // inset gives it back, and the grid keeps the full content width.
        let before = 2.0 * WINDOW_PADDING + CONTENT_W;
        let after = 2.0 * (WINDOW_PADDING - o) + (CONTENT_W + 2.0 * o);
        assert_eq!(after, before, "the frame changed the window's width");
        // …and every section ABOVE the history is handed that horizontal outset back, so
        // it is laid out at exactly the width it always was.
        assert_eq!((WINDOW_PADDING - o) + o, WINDOW_PADDING);
        // The air is real: the frame's own width comes out of the outset, and what is left
        // is the gap between the frame and the swatch rims.
        assert!(
            o - FOCUS_RING_W >= 3.0,
            "only {}pt between the frame and the swatches",
            o - FOCUS_RING_W
        );
        // And the window is exactly what the rest of the derivation says it is (the
        // height is header-relative since the header stopped being a 44pt fiction).
        assert_eq!(color_window_size(), (422.0, header_h() + 519.0));
    }

    /// The two strips still start at the round swatch's top edge and end at its bottom
    /// one, which is what the owner asked to keep while they got thinner (DRAGON-680).
    /// The thickness they gave up went into the gap between them and nowhere else.
    #[test]
    fn the_strips_still_span_the_swatch_exactly() {
        assert_eq!(2.0 * STRIP_H + STRIP_GAP, SWATCH_CIRCLE, "top and bottom still align");
        assert_eq!(STRIP_H, 16.0, "thinner than the 20 it was");
        assert_eq!(STRIP_GAP, 16.0, "and the 8 points they gave up are the extra air");
        // The thumb is DERIVED from the track, so "it shrank with it" is a compile-time
        // fact rather than a runtime one; stated as a const assert, which is also what
        // fails the build if someone gives the thumb a literal of its own.
        const {
            assert!(
                STRIP_MARKER_D > STRIP_H,
                "the thumb has to stand proud of its track to be grabbable"
            )
        };
        assert_eq!(STRIP_MARKER_D, 20.0, "24 while the track was 20");
    }

    /// DRAGON-682: the expanded window is exactly TWICE the base width and the same
    /// height, which is the owner's whole spec for the expand button.
    #[test]
    fn expanding_doubles_the_width_and_nothing_else() {
        let (bw, bh) = color_window_size();
        let (ew, eh) = color_window_size_expanded();
        assert_eq!(ew, bw * 2.0, "the expanded window is not double");
        assert_eq!(eh, bh, "expanding must not change the height");
        assert_eq!(color_window_size_for(false), (bw, bh));
        assert_eq!(color_window_size_for(true), (ew, eh));
        // Header-relative since the header stopped being the 44pt fiction: the rest of
        // the stack is the 519pt the historical 563 always contained.
        assert_eq!((bw, bh), (422.0, header_h() + 519.0));
        assert_eq!((ew, eh), (844.0, header_h() + 519.0));
    }

    /// The two halves ACCOUNT for the expanded window: the picker's column is pinned at
    /// its own width and the panel takes the rest, so opening the panel cannot move a
    /// single point of the left half (the owner's requirement).
    #[test]
    fn the_panel_takes_exactly_the_half_the_picker_does_not() {
        let inside = color_window_size_expanded().0 - 2.0 * WINDOW_BORDER;
        assert_eq!(picker_column_w() + panel_w(), inside, "the two halves do not add up");
        assert_eq!(picker_column_w(), CONTENT_W + 2.0 * WINDOW_PADDING);
        // The panel is the same size as the picker's own column, give or take the single
        // window border the frosted container spends on the outside edge.
        assert!((panel_w() - picker_column_w()).abs() <= 2.0 * WINDOW_BORDER);
        // And its content is what is left inside its own padding.
        assert_eq!(panel_content_w(), panel_w() - 2.0 * WINDOW_PADDING);
    }

    /// The panel's tabs round-trip through the id the config stores, and anything else
    /// answers the tab that has content in it.
    #[test]
    fn the_panel_tab_survives_the_config() {
        for t in PanelTab::ALL {
            assert_eq!(PanelTab::from_id(t.id()), t);
            assert!(!t.label().is_empty());
            assert!(!t.label().contains('\u{2014}'), "em dash in {}", t.id());
        }
        for junk in ["", "Harmonies", "history", " harmonies "] {
            assert_eq!(PanelTab::from_id(junk), PanelTab::Harmonies, "{junk:?}");
        }
        assert_eq!(PanelTab::default(), PanelTab::Harmonies);
    }

    /// The panel's ragged grid is the harmony list's own shape, which is what the
    /// keyboard cursor navigates. A card that grew a swatch without this following would
    /// leave the last one unreachable.
    #[test]
    fn the_harmony_grid_matches_the_cards() {
        let rows = harmony_card_lengths();
        assert_eq!(rows.len(), crate::color::Harmony::ALL.len());
        for (row, h) in rows.iter().zip(crate::color::Harmony::ALL) {
            assert_eq!(*row, h.swatches(Srgb::new(255, 136, 0)).len(), "{}", h.id());
            assert!(*row >= 2, "{}: a card with nothing to hold", h.id());
            assert!(*row <= MAX_SEGMENTS, "{}: wider than a bar can be", h.id());
        }
        // The owner's five, in the owner's order (item 20).
        assert_eq!(rows.len(), 5);
        // Every group's scroll offset climbs, and the first is the top of the panel.
        assert_eq!(harmony_group_offset(0), 0.0);
        for g in 1..rows.len() {
            assert!(
                harmony_group_offset(g) > harmony_group_offset(g - 1),
                "group {g} scrolls no further than the one above it"
            );
        }
    }

    /// A harmony swatch's menu lands inside its CARD whichever segment it was opened on,
    /// at every bar width a card can have.
    #[test]
    fn the_harmony_menu_fits_the_card_at_every_segment() {
        let panel = harmony_menu_width();
        for n in 1..=MAX_SEGMENTS {
            for col in 0..n {
                let left = segment_x(col, n) - harmony_menu_dx(col, n, panel);
                assert!(left >= 0.0, "{n} segments, {col}: the menu starts at {left}");
                assert!(
                    left + panel <= card_w() + 0.01,
                    "{n} segments, {col}: the menu ends at {}",
                    left + panel
                );
            }
            assert_eq!(harmony_menu_dx(0, n, panel), 0.0, "the first segment needs no shift");
        }
        // THREE rows since item 28 added "Add to recents" between the other two.
        let n = HARMONY_MENU_ROWS as f32;
        assert_eq!(
            harmony_menu_panel_h(),
            n * MODE_MENU_ITEM_H + (n - 1.0) * MODE_MENU_GAP + 2.0 * f32::from(MODE_MENU_PAD)
        );
        assert_eq!(HARMONY_MENU_ROWS, 3, "set active, add to recents, copy");
    }

    /// DRAGON-682 item 17: a swatch BAR is full width whatever subdivides it, its segments
    /// differ by at most a point, and only its two ends are rounded.
    #[test]
    fn a_swatch_bar_fills_its_card_at_every_count() {
        for n in 1..=MAX_SEGMENTS {
            let widths = segment_widths(n);
            assert_eq!(widths.len(), n);
            assert_eq!(widths.iter().sum::<f32>(), bar_w(), "{n} segments do not fill the bar");
            let (min, max) = (
                widths.iter().cloned().fold(f32::MAX, f32::min),
                widths.iter().cloned().fold(0.0, f32::max),
            );
            assert!(max - min <= 1.0, "{n}: {min} vs {max} is a visible step");
            assert!(widths.iter().all(|w| w.fract() == 0.0), "{n}: whole points only");
            // The corners: the first segment's left, the last segment's right, nothing in
            // between, and a one-segment bar is both.
            for i in 0..n {
                assert_eq!(segment_corners(i, n), [i == 0, i + 1 == n], "{n}/{i}");
            }
            // The segments march left to right with no gaps: each starts where the last
            // one ended.
            for i in 1..n {
                assert_eq!(
                    segment_x(i, n),
                    segment_x(i - 1, n) + widths[i - 1],
                    "{n}: segment {i} does not start where {} ended",
                    i - 1
                );
            }
        }
        // The bar is a HISTORY SWATCH tall (item 21), read from that constant.
        assert_eq!(PANEL_SWATCH, RECENT_SWATCH);
        // And the widest bar the panel can hold is what `MAX_SEGMENTS` claims.
        assert_eq!(
            harmony_card_lengths().into_iter().max().unwrap_or(0),
            MAX_SEGMENTS,
            "MAX_SEGMENTS is not the widest card"
        );
    }

    /// DRAGON-682 item 16: the cards leave the scrollbar its own lane, so a row and the
    /// scrollbar can never overlap.
    #[test]
    fn the_cards_leave_the_scrollbar_its_lane() {
        assert_eq!(card_w(), panel_content_w() - PANEL_SCROLLBAR_GAP);
        const { assert!(PANEL_SCROLLBAR_GAP > 0.0, "there is no lane at all") };
        // The BAR is the whole card since item 27 took the card padding off the harmony
        // groups; the padding survives for a group that is still carded.
        assert_eq!(bar_w(), card_w());
        assert!(bar_w() > 0.0);
    }

    /// DRAGON-682 item 27, re-pinned against the EXACT viewport by DRAGON-687's spacing
    /// round: the Harmonies tab FITS its viewport (the derived gap makes it fill it, see
    /// `the_gap_constant_fills_the_harmonies_viewport`), so the scrollable never engages.
    /// The scrollable stays either way, which is what this pins next: a sixth group must
    /// scroll rather than clip.
    #[test]
    fn the_harmony_tab_fits_without_scrolling() {
        let content = harmony_content_h();
        let viewport = harmonies_viewport_h();
        assert!(
            content <= viewport,
            "the five groups need {content}pt of a {viewport}pt viewport"
        );
        // A sixth group would NOT fit, which is why the scrollable stays.
        let sixth = content + harmony_group_h() + PANEL_GROUP_GAP;
        assert!(
            sixth > viewport,
            "a sixth group still fits ({sixth}pt), so this test proves nothing about the \
             scrollable"
        );
        // And a group is heading, gap, bar: no card padding left in the sum.
        assert_eq!(harmony_group_h(), PANEL_HEADING_H + PANEL_HEADING_GAP + PANEL_SWATCH);
    }

    /// The cap is two FULL rows, so the grid can always be drawn ragged-last-row at
    /// worst and never a third row.
    #[test]
    fn the_cap_is_two_full_rows() {
        assert_eq!(RECENTS_CAP, RECENTS_PER_ROW * 2);
    }

    /// Every box count a SPLIT mode can produce (4 everywhere, CMYK's 5) gets a box a
    /// value is actually readable in, and the row never overflows its budget. It is exact
    /// rather than merely fitting, since the boxes own everything the stepper leaves
    /// ([`the_box_row_spans_its_budget_exactly`] pins that end of it).
    #[test]
    fn value_boxes_stay_readable_at_every_mode() {
        for f in crate::color::ColorFormat::ALL.into_iter().filter(|f| splits_components(*f)) {
            let boxes = f.component_labels().len() + 1;
            let bw = value_box_width(boxes);
            assert!(
                bw >= 54.0,
                "{}: {boxes} boxes of {bw}pt cannot hold a 5-character value",
                f.id()
            );
            let row: f32 =
                value_box_widths(boxes).iter().sum::<f32>() + (boxes as f32 - 1.0) * BOX_GAP;
            assert!(row <= value_whole_width(), "{}: the box row overflows ({row})", f.id());
        }
        // And hex's UNIFIED box holds the longest spelling it can ever show, at the
        // ~8pt-per-character the original row arithmetic used: `#FF8800CC` is 9
        // characters, so the one box it gets is enormously more than it needs. The value
        // of the check is the other direction: this box also has to stay wide enough to
        // be worth being one box at all.
        assert!(value_whole_width() >= 9.0 * 8.0 + 24.0);
    }

    /// The cap is EIGHTEEN (two rows of nine; two rows of ten through DRAGON-649, two of
    /// eight through DRAGON-630, one row of ten before that), and the nineteenth pick
    /// drops the oldest.
    #[test]
    fn the_cap_is_eighteen_and_the_nineteenth_pick_drops_the_oldest() {
        assert_eq!(RECENTS_CAP, 18);
        let e = |i: u8| Recent::opaque(Srgb::new(i, 0, 0));
        let full: Vec<Recent> = (0..18u8).map(e).collect();
        let after = push_recent(&full, e(200), RECENTS_CAP);
        assert_eq!(after.len(), 18, "still eighteen");
        assert_eq!(after[0], e(200), "the newest leads");
        assert_eq!(after.last(), Some(&e(16)), "the list ends one earlier");
        assert!(!after.contains(&e(17)), "and the oldest fell off entirely");
    }
}

/// DRAGON-680: which layout a mode's value row takes, and the box ids that follow from
/// it. The rule is a pure function of the mode now, so this is where "hex is one box,
/// everything else is per-component" is pinned against the notation list rather than
/// against a remembered flag.
#[cfg(test)]
mod value_layout_tests {
    use super::*;
    use crate::color::ColorFormat;

    /// The owner's rule, stated over the whole list rather than at the two ends: hex is
    /// the ONE unified box and every other notation splits.
    #[test]
    fn hex_is_the_only_unified_mode() {
        assert!(!splits_components(ColorFormat::Hex), "hex is one whole-value box");
        for f in ColorFormat::ALL.into_iter().filter(|f| *f != ColorFormat::Hex) {
            assert!(splits_components(f), "{} splits into its components", f.id());
        }
    }

    /// The two layouts really are different SHAPES, not the same row with a different
    /// label: hex shows one box and every split mode shows at least four. Pinned so a
    /// future "simplification" that made hex four boxes again (its DRAGON-630 rev-2
    /// shape) fails here rather than on the owner's screen.
    #[test]
    fn the_two_layouts_are_different_shapes() {
        let boxes = |f: ColorFormat| {
            if splits_components(f) { f.component_labels().len() + 1 } else { 1 }
        };
        assert_eq!(boxes(ColorFormat::Hex), 1);
        for f in ColorFormat::ALL.into_iter().filter(|f| splits_components(*f)) {
            assert!(boxes(f) >= 4, "{}: {} boxes", f.id(), boxes(f));
        }
    }

    /// EVERY notation's word fits the panel, with the panel's own inset and the row's
    /// padding taken off first. Checked against the LIST rather than against "OKLCH", so
    /// adding an eighth notation fails here instead of clipping on screen.
    #[test]
    fn every_menu_row_fits_the_panel() {
        let row_w = mode_menu_width()
            - 2.0 * f32::from(MODE_MENU_PAD)
            - 2.0 * f32::from(MODE_MENU_ROW_PAD);
        for f in crate::color::ColorFormat::ALL {
            let label = crate::app::preview::text_annot::measure(
                crate::app::preview::text_annot::TextFont::Clean,
                MODE_LABEL_SIZE,
                f.label(),
            );
            assert!(row_w >= label, "{}: {row_w}pt of row for a {label}pt word", f.id());
        }
        // The widest label is what SETS the width, so the panel is what that one needs.
        assert_eq!(mode_menu_width(), mode_menu_width_for(widest_mode_label()));
        for w in [0.0f32, 12.0, 43.5, 200.0] {
            assert_eq!(
                mode_menu_width_for(w).fract(),
                0.0,
                "{w}: a fractional panel width lands its edges on half pixels"
            );
        }
        assert!(
            mode_menu_width_for(80.0) > mode_menu_width_for(40.0),
            "a wider label must widen the panel"
        );
    }

    /// The panel is RIGHT-aligned to the activator, and the whole of it lands inside the
    /// window's content. That is the reason `chrome::FlyoutDir::UpRight` exists: the
    /// activator is at the content's right edge, so the app's ordinary left-aligned flyout
    /// would hang the panel off the window.
    #[test]
    fn the_menu_hangs_inside_the_window() {
        assert_eq!(mode_menu_dx(), mode_menu_width() - MODE_STEP_W);
        // The activator's right edge IS the content's right edge, so the panel spans
        // `[CONTENT_W - width, CONTENT_W]`, which has to start at or after the content's
        // left edge.
        assert!(
            mode_menu_width() <= CONTENT_W,
            "the panel ({}pt) is wider than the content it must sit inside",
            mode_menu_width()
        );
        // And a LEFT-aligned panel really would have run off, or the new flyout direction
        // would be solving nothing.
        assert!(
            MODE_STEP_W < mode_menu_width(),
            "the panel is no wider than its anchor, so alignment could not matter"
        );
    }

    /// The panel's height is the exact sum of its parts, which is what the upward flyout's
    /// offset is: under-counted, the menu slides down into the control it hangs off.
    #[test]
    fn the_panel_height_is_the_sum_of_its_rows() {
        let n = crate::color::ColorFormat::ALL.len() as f32;
        assert_eq!(
            mode_menu_panel_h(),
            n * MODE_MENU_ITEM_H + (n - 1.0) * MODE_MENU_GAP + 2.0 * f32::from(MODE_MENU_PAD)
        );
        // It must also FIT above the activator, which sits in the value row: the square,
        // its gap, the controls row and its gap are what is above.
        let above = SV_H + GAP_SQUARE_CONTROLS + CONTROLS_H + GAP_CONTROLS_VALUE + VALUE_BOX_H;
        assert!(
            mode_menu_panel_h() <= above,
            "the menu ({}pt) is taller than the {above}pt above its anchor",
            mode_menu_panel_h()
        );
    }

    /// The owner's ring, forwards: every box in order, then the mode activator, then the
    /// history, then back to the first box.
    #[test]
    fn tab_walks_the_boxes_then_the_activator_then_the_history() {
        use PickerFocus::*;
        let ring = |from| next_focus(Some(from), true, 4, true, false);
        assert_eq!(ring(Box(0)), Box(1));
        assert_eq!(ring(Box(2)), Box(3));
        assert_eq!(ring(Box(3)), Mode, "after the last input comes the activator");
        assert_eq!(ring(Mode), History, "then the whole history as one stop");
        assert_eq!(ring(History), Box(0), "and back to the first input");
    }

    /// And backwards, which is the same ring walked the other way. Pinned separately
    /// because Shift+Tab is the half a hand-rolled cycle usually gets wrong.
    #[test]
    fn shift_tab_walks_the_same_ring_backwards() {
        use PickerFocus::*;
        let back = |from| next_focus(Some(from), false, 4, true, false);
        assert_eq!(back(Box(0)), History, "the owner's ask, in so many words");
        assert_eq!(back(History), Mode);
        assert_eq!(back(Mode), Box(3));
        assert_eq!(back(Box(1)), Box(0));
    }

    /// Every stop is reachable from every other by tabbing, in both directions, at every
    /// box count a mode can have. A ring with a hole in it is exactly the defect the owner
    /// reported, so this walks the whole cycle rather than sampling it.
    #[test]
    fn the_ring_is_closed_at_every_mode() {
        for boxes in 1..=MAX_VALUE_BOXES {
            let len = boxes + 2; // the boxes, the activator, the history
            let mut at = next_focus(None, true, boxes, true, false);
            assert_eq!(at, PickerFocus::Box(0), "a fresh Tab enters at the first input");
            let mut seen = vec![at];
            for _ in 1..len {
                at = next_focus(Some(at), true, boxes, true, false);
                assert!(!seen.contains(&at), "{boxes} boxes: {at:?} came round twice");
                seen.push(at);
            }
            assert_eq!(seen.len(), len, "{boxes} boxes: the ring is the wrong length");
            assert_eq!(
                next_focus(Some(at), true, boxes, true, false),
                PickerFocus::Box(0),
                "{boxes} boxes: the ring does not close"
            );
            // And every forward step is undone by a backward one.
            for stop in seen {
                let there = next_focus(Some(stop), true, boxes, true, false);
                assert_eq!(next_focus(Some(there), false, boxes, true, false), stop, "{stop:?}");
            }
        }
    }

    /// DRAGON-682 item 9: the PANEL joins the ring, last, and only while the window is
    /// expanded. Both halves matter: a stop that existed collapsed would be a place Tab
    /// vanishes into a panel nobody can see.
    #[test]
    fn the_panel_is_the_last_stop_and_only_when_it_is_open() {
        use PickerFocus::*;
        // Expanded: after the history comes the panel, and the panel wraps to the boxes.
        assert_eq!(next_focus(Some(History), true, 4, true, true), Panel);
        assert_eq!(next_focus(Some(Panel), true, 4, true, true), Box(0));
        assert_eq!(next_focus(Some(Box(0)), false, 4, true, true), Panel, "backwards too");
        assert_eq!(next_focus(Some(Panel), false, 4, true, true), History);
        // Collapsed: the ring is exactly what it was before this ticket.
        assert_eq!(next_focus(Some(History), true, 4, true, false), Box(0));
        assert_eq!(next_focus(Some(Box(0)), false, 4, true, false), History);
        // And with no history either, the panel still sits after the activator.
        assert_eq!(next_focus(Some(Mode), true, 4, false, true), Panel);
        assert_eq!(next_focus(Some(Panel), true, 4, false, true), Box(0));
    }

    /// The ring stays CLOSED with the panel in it, at every mode, in both directions.
    /// Same property as the collapsed ring, re-checked because a fourth kind of stop is
    /// exactly where a hand-rolled cycle grows a hole.
    #[test]
    fn the_expanded_ring_is_closed_too() {
        for boxes in 1..=MAX_VALUE_BOXES {
            let len = boxes + 3; // the boxes, the activator, the history, the panel
            let mut at = next_focus(None, true, boxes, true, true);
            let mut seen = vec![at];
            for _ in 1..len {
                at = next_focus(Some(at), true, boxes, true, true);
                assert!(!seen.contains(&at), "{boxes} boxes: {at:?} came round twice");
                seen.push(at);
            }
            assert_eq!(seen.len(), len, "{boxes} boxes: the ring is the wrong length");
            assert_eq!(next_focus(Some(at), true, boxes, true, true), PickerFocus::Box(0));
            for stop in seen {
                let there = next_focus(Some(stop), true, boxes, true, true);
                assert_eq!(next_focus(Some(there), false, boxes, true, true), stop, "{stop:?}");
            }
        }
    }

    /// An EMPTY history is not a stop: a frame around nothing but placeholder slots would
    /// be a place Tab lands with nothing to do and no arrow key that means anything.
    #[test]
    fn an_empty_history_is_not_in_the_ring() {
        use PickerFocus::*;
        assert_eq!(next_focus(Some(Box(3)), true, 4, false, false), Mode);
        assert_eq!(next_focus(Some(Mode), true, 4, false, false), Box(0), "straight back to the row");
        assert_eq!(next_focus(Some(Box(0)), false, 4, false, false), Mode);
    }

    /// Entering the ring from nothing takes the end you pressed from, and a STALE box
    /// position (the mode changed to fewer boxes while it held focus) does the same rather
    /// than wrapping from a box that no longer exists.
    #[test]
    fn entering_from_nothing_or_a_stale_stop_is_safe() {
        use PickerFocus::*;
        assert_eq!(next_focus(None, true, 4, true, false), Box(0));
        assert_eq!(next_focus(None, false, 4, true, false), History, "backwards enters at the end");
        assert_eq!(next_focus(None, false, 4, false, false), Mode, "…or the last stop there is");
        // Hex has ONE box: a position left over from CMYK's five is not in this ring.
        assert_eq!(next_focus(Some(Box(4)), true, 1, true, false), Box(0));
        assert_eq!(next_focus(Some(Box(4)), false, 1, true, false), History);
    }

    /// The two numbering schemes agree everywhere they can, and differ exactly where
    /// they must: a split mode's box edits the component at its own position, and hex's
    /// one box edits the WHOLE spelling. Pinned because a mismatch is invisible until a
    /// draft renders in the wrong box or focus lands where the caret is not.
    #[test]
    fn the_draft_index_follows_the_position_except_for_hex() {
        for f in ColorFormat::ALL.into_iter().filter(|f| splits_components(*f)) {
            for pos in 0..MAX_VALUE_BOXES {
                assert_eq!(draft_index(f, pos), pos, "{}", f.id());
            }
        }
        assert_eq!(
            draft_index(ColorFormat::Hex, 0),
            crate::app::color_picker::WHOLE_VALUE_BOX,
            "hex's one box edits the whole spelling"
        );
    }

    /// Every mode's box row fits inside [`MAX_VALUE_BOXES`], which is what lets the
    /// window mint one stable focus id per box position ONCE and reuse it for every
    /// mode. A notation with more components than this would silently share an id
    /// between two boxes, and Tab would then stop moving between them.
    #[test]
    fn every_mode_fits_the_focus_id_list() {
        for f in ColorFormat::ALL {
            let boxes =
                if splits_components(f) { f.component_labels().len() + 1 } else { 1 };
            assert!(
                boxes <= MAX_VALUE_BOXES,
                "{}: {boxes} boxes, but only {MAX_VALUE_BOXES} ids exist",
                f.id()
            );
        }
        // And the constant is not loose: some mode really does need all five, or it is
        // just a number nobody has checked.
        let widest = ColorFormat::ALL
            .into_iter()
            .map(|f| if splits_components(f) { f.component_labels().len() + 1 } else { 1 })
            .max()
            .unwrap_or(0);
        assert_eq!(widest, MAX_VALUE_BOXES, "CMYK's five is what sets the ceiling");
    }
}

#[cfg(test)]
mod recents_tests {
    use super::*;

    fn c(r: u8) -> Recent {
        Recent::opaque(Srgb::new(r, 0, 0))
    }

    /// THE rule, all three cases: only a PICK writes.
    #[test]
    fn only_a_pick_writes_the_list() {
        assert!(writes_recents(ColorSource::Pick));
        assert!(!writes_recents(ColorSource::RecentClick));
        assert!(!writes_recents(ColorSource::Edit));
    }

    /// Clicking recents, however many times, leaves the list BYTE-IDENTICAL. Written as
    /// the loop the owner described rather than as a single assertion, because the thing
    /// being ruled out is a slow drift over repeated clicks.
    #[test]
    fn clicking_recents_never_reorders_them() {
        let list = vec![c(1), c(2), c(3), c(4)];
        let mut after = list.clone();
        for pick in [c(3), c(1), c(4), c(3), c(2)] {
            if writes_recents(ColorSource::RecentClick) {
                after = push_recent(&after, pick, RECENTS_CAP);
            }
        }
        assert_eq!(after, list);
    }

    /// Editing a value box does not write either, so exploring a colour cannot fill the
    /// row with the intermediate colours of one edit.
    #[test]
    fn editing_a_row_never_writes() {
        let list = vec![c(1), c(2)];
        let mut after = list.clone();
        for typed in [c(9), c(8), c(7)] {
            if writes_recents(ColorSource::Edit) {
                after = push_recent(&after, typed, RECENTS_CAP);
            }
        }
        assert_eq!(after, list);
    }

    /// A pick after a run of recent-clicks goes to the FRONT of the unchanged list,
    /// which is the composite the owner asked for.
    #[test]
    fn a_pick_after_clicks_leads_the_unchanged_list() {
        let list = vec![c(1), c(2), c(3)];
        // Three clicks: nothing moves.
        let after_clicks = list.clone();
        // Then a pick.
        let after = push_recent(&after_clicks, c(9), RECENTS_CAP);
        assert_eq!(after, vec![c(9), c(1), c(2), c(3)]);
    }

    /// A duplicate pick PROMOTES its existing entry rather than adding a second copy.
    #[test]
    fn a_duplicate_pick_moves_to_the_front() {
        let list = vec![c(1), c(2), c(3)];
        assert_eq!(push_recent(&list, c(3), RECENTS_CAP), vec![c(3), c(1), c(2)]);
        // Re-picking what is already leading is a no-op in effect.
        assert_eq!(push_recent(&list, c(1), RECENTS_CAP), list);
        // And the length never grows on a duplicate.
        assert_eq!(push_recent(&list, c(2), RECENTS_CAP).len(), list.len());
    }

    /// The cap drops the OLDEST, and a degenerate cap still keeps the newest rather than
    /// emptying the row.
    #[test]
    fn the_cap_drops_the_oldest() {
        let full: Vec<Recent> = (0..RECENTS_CAP as u8).map(c).collect();
        let after = push_recent(&full, c(200), RECENTS_CAP);
        assert_eq!(after.len(), RECENTS_CAP);
        assert_eq!(after[0], c(200));
        assert_eq!(after.last(), Some(&c(RECENTS_CAP as u8 - 2)), "the oldest fell off");
        assert_eq!(push_recent(&full, c(200), 0), vec![c(200)], "a zero cap still keeps one");
    }

    /// DRAGON-682 item 15: a SWATCH menu's copy does not light the window's copy BUTTON.
    ///
    /// The owner's report, and the two halves that must both hold: the flash belongs to
    /// that button, and the other two copies still raise it.
    #[test]
    fn only_the_windows_own_copies_flash_the_copy_button() {
        assert!(!copy_flashes(CopySource::SwatchMenu), "a swatch copy must not light it");
        assert!(copy_flashes(CopySource::CopyButton), "the button flashing IS the button");
        assert!(copy_flashes(CopySource::Pick), "the pick's own copy still says so");
    }

    /// DRAGON-682 item 22: a HARMONY apply files the colour into the recents; a recents
    /// click still does not. Both directions, because the whole point of the source table
    /// is that neither is a side call at a call site.
    #[test]
    fn a_harmony_apply_files_the_colour_and_a_recents_click_does_not() {
        assert!(writes_recents(ColorSource::Harmony), "a harmony apply must file it");
        assert!(writes_recents(ColorSource::Pick), "and a pick still does");
        assert!(!writes_recents(ColorSource::RecentClick), "a load is not a write");
        assert!(!writes_recents(ColorSource::Edit), "nor is typing");
        // And the ALPHA rule that goes with it: everything but a pick brings its own.
        assert!(!keeps_alpha(ColorSource::Pick), "a screen pixel is opaque");
        for source in
            [ColorSource::Harmony, ColorSource::RecentClick, ColorSource::Edit]
        {
            assert!(keeps_alpha(source), "{source:?} carries its own alpha");
        }
    }

    /// DRAGON-680 item 24: removal takes exactly one entry and shifts the rest up.
    #[test]
    fn removing_an_entry_shifts_the_list_up() {
        let list = vec![c(1), c(2), c(3)];
        assert_eq!(remove_recent(&list, 0), vec![c(2), c(3)]);
        assert_eq!(remove_recent(&list, 1), vec![c(1), c(3)]);
        assert_eq!(remove_recent(&list, 2), vec![c(1), c(2)]);
        // The last one leaves an EMPTY history, which the window has to survive: the focus
        // ring drops that stop and the grid shows its "colors you pick appear here" line.
        assert!(remove_recent(&[c(1)], 0).is_empty());
    }

    /// An index the list does not have changes nothing rather than panicking. It is
    /// reachable: the menu is opened over a swatch, and a pick delivered from another
    /// process (DRAGON-613) can reorder the history before the click lands.
    #[test]
    fn removing_out_of_range_is_a_no_op() {
        let list = vec![c(1), c(2)];
        assert_eq!(remove_recent(&list, 2), list);
        assert_eq!(remove_recent(&list, usize::MAX), list);
        assert!(remove_recent(&[], 0).is_empty());
    }

    /// DRAGON-680 item 24: WHICH swatch Backspace or Delete removes. The owner's ordering,
    /// case by case, because getting it wrong is destructive and silent.
    #[test]
    fn the_delete_key_picks_its_target_in_the_owners_order() {
        use PickerFocus::*;
        // A value box has the caret: NOTHING, whatever else is true. This is the hard
        // guard, and it is first because a user typing a colour presses Backspace
        // constantly while the pointer sits wherever they left it.
        assert_eq!(remove_target(Some(2), Some(3), Some(Box(0)), 6), None);
        assert_eq!(remove_target(None, Some(3), Some(Box(1)), 6), None);
        // The HOVERED swatch wins: it is the one being pointed at.
        assert_eq!(remove_target(Some(2), Some(3), Some(History), 6), Some(2));
        assert_eq!(remove_target(Some(2), None, Some(Mode), 6), Some(2));
        assert_eq!(remove_target(Some(2), None, None, 6), Some(2));
        // With no hover, the SELECTED one, but only while the history holds the ring.
        assert_eq!(remove_target(None, Some(3), Some(History), 6), Some(3));
        assert_eq!(remove_target(None, Some(3), Some(Mode), 6), None);
        assert_eq!(remove_target(None, Some(3), None, 6), None);
        // Nothing to aim at.
        assert_eq!(remove_target(None, None, Some(History), 6), None);
        assert_eq!(remove_target(None, None, None, 0), None);
    }

    /// A STALE index (the list shrank under a hover, or the selection outlived its entry)
    /// never removes the wrong swatch: it removes nothing.
    #[test]
    fn a_stale_target_removes_nothing() {
        use PickerFocus::*;
        assert_eq!(remove_target(Some(9), None, None, 3), None);
        assert_eq!(remove_target(None, Some(9), Some(History), 3), None);
        // …and a live selection behind a stale hover is still reachable.
        assert_eq!(remove_target(Some(9), Some(1), Some(History), 3), Some(1));
    }

    /// DRAGON-680 item 24: the context menu lands INSIDE the window whichever swatch it
    /// was opened on. Walked over every swatch position rather than sampled, because the
    /// failure is a menu clipped at one end of one row.
    #[test]
    fn the_recents_menu_fits_the_window_at_every_swatch() {
        let panel = recents_menu_width();
        assert!(panel <= CONTENT_W, "the menu ({panel}pt) is wider than the content");
        for i in 0..RECENTS_CAP {
            let col = i % RECENTS_PER_ROW;
            let x = col as f32 * (RECENT_SWATCH + recents_gap());
            let left = x - recents_menu_dx(i, panel);
            assert!(left >= 0.0, "swatch {i}: the menu starts at {left}");
            assert!(
                left + panel <= CONTENT_W + 0.01,
                "swatch {i}: the menu ends at {}",
                left + panel
            );
        }
        // The FIRST swatch's menu is left-aligned (nothing to avoid) and the LAST one's is
        // pushed left by exactly its overflow, which is what makes the rule a clamp rather
        // than a side choice.
        assert_eq!(recents_menu_dx(0, panel), 0.0);
        assert!(recents_menu_dx(RECENTS_PER_ROW - 1, panel) > 0.0);
        // Both rows behave the same: the column is what matters, not the row.
        assert_eq!(
            recents_menu_dx(0, panel),
            recents_menu_dx(RECENTS_PER_ROW, panel),
            "the second row's first swatch is the first row's first swatch"
        );
    }

    /// The menu's own panel is one row tall and wide enough for its one label, measured the
    /// same way the notation menu measures its longest.
    #[test]
    fn the_recents_menu_holds_its_label() {
        let row_w = recents_menu_width()
            - 2.0 * f32::from(MODE_MENU_PAD)
            - 2.0 * f32::from(MODE_MENU_ROW_PAD);
        for text in [REMOVE_RECENT_LABEL, SET_ACTIVE_LABEL] {
            let label = crate::app::preview::text_annot::measure(
                crate::app::preview::text_annot::TextFont::Clean,
                MODE_LABEL_SIZE,
                text,
            );
            assert!(row_w >= label, "{row_w}pt of row for a {label}pt {text:?}");
        }
        // TWO rows since DRAGON-682 item 7 put "Set as active color" above the remove
        // entry, so the menus of the two swatch kinds read as one vocabulary.
        assert_eq!(
            recents_menu_panel_h(),
            2.0 * MODE_MENU_ITEM_H + MODE_MENU_GAP + 2.0 * f32::from(MODE_MENU_PAD)
        );
        for label in [REMOVE_RECENT_LABEL, SET_ACTIVE_LABEL, COPY_COLOR_LABEL] {
            assert!(!label.contains('\u{2014}'), "em dash in {label:?}");
            assert!(!label.contains('\u{2013}'), "en dash in {label:?}");
        }
    }

    /// DRAGON-680: the history remembers TRANSPARENCY, and the same colour at two
    /// alphas is two entries. They copy out differently and they draw differently, so
    /// collapsing them would throw away whichever the user filed second.
    #[test]
    fn the_same_colour_at_two_alphas_is_two_entries() {
        let solid = Recent::opaque(Srgb::new(255, 0, 0));
        let half = Recent::new(Srgb::new(255, 0, 0), 128);
        let after = push_recent(&push_recent(&[], solid, RECENTS_CAP), half, RECENTS_CAP);
        assert_eq!(after, vec![half, solid], "both kept, newest first");
        // And an exact duplicate (colour AND alpha) still promotes rather than doubling.
        assert_eq!(push_recent(&after, solid, RECENTS_CAP), vec![solid, half]);
    }

    /// The persisted spelling round-trips, in both directions and both shapes: an opaque
    /// entry writes the SIX-digit form every older build wrote (so an untouched history
    /// is byte-identical on disk after this ticket), and a translucent one writes eight.
    #[test]
    fn entries_round_trip_through_their_persisted_spelling() {
        let solid = Recent::opaque(Srgb::new(255, 136, 0));
        let half = Recent::new(Srgb::new(255, 136, 0), 128);
        assert_eq!(solid.hex(), "#FF8800", "an opaque entry spells as it always did");
        assert_eq!(half.hex(), "#FF880080");
        for e in [solid, half] {
            assert_eq!(Recent::parse(&e.hex()), Some(e), "{}", e.hex());
        }
    }

    /// A history written by ANY older build loads, and loads OPAQUE. This is the
    /// migration: nothing is dropped, nothing is guessed, and a six-digit entry means
    /// exactly what it meant before alpha existed.
    #[test]
    fn a_legacy_alpha_less_entry_loads_opaque() {
        assert_eq!(Recent::parse("#FF8800"), Some(Recent::opaque(Srgb::new(255, 136, 0))));
        assert_eq!(Recent::parse("f80"), Some(Recent::opaque(Srgb::new(255, 136, 0))));
        for junk in ["", "#", "nope", "#12345", "#GG0000"] {
            assert_eq!(Recent::parse(junk), None, "{junk:?}");
        }
    }
}

/// DRAGON-TBD: pacing the magnifier's raster against a moving pointer.
///
/// The property that matters is NOT "it saves work", it is that the two ends are right: an
/// aiming pointer is byte-identical to having no pacing at all, and a pointer that STOPS gets
/// its accurate lens back on the very next frame. Everything in between is a ramp whose exact
/// value nobody should depend on, so these tests pin the ends and the monotonicity rather than
/// interpolated numbers.
#[cfg(test)]
mod raster_pacing_tests {
    use super::*;
    use std::time::Duration;

    /// One frame at the two refresh rates this app actually meets: the owner's ProMotion panel
    /// and an ordinary display.
    const FRAME_120: Duration = Duration::from_micros(8_333);
    const FRAME_60: Duration = Duration::from_micros(16_667);

    /// Speed is plain distance over time, on both axes together, in points per second.
    #[test]
    fn speed_is_the_distance_covered_per_second() {
        // 3-4-5: 5 points in 10ms is 500 pt/s.
        let at = |from, to| sample_speed(from, to, Duration::from_millis(10));
        assert!((at((0.0, 0.0), (3.0, 4.0)) - 500.0).abs() < 0.1);
        // Direction is irrelevant, only distance.
        assert!((at((10.0, 10.0), (7.0, 6.0)) - 500.0).abs() < 0.1);
        // A pointer that did not move has no speed, whatever the interval.
        assert_eq!(sample_speed((5.0, 5.0), (5.0, 5.0), FRAME_120), 0.0);
    }

    /// An unknown interval must fall toward ACCURACY, never toward the throttle. Two samples
    /// stamped in the same instant say the clock could not separate them, which is not
    /// evidence of a fast pointer.
    #[test]
    fn an_unmeasurable_interval_reads_as_stationary_rather_than_infinite() {
        let unmeasurable = sample_speed((0.0, 0.0), (900.0, 900.0), Duration::ZERO);
        assert_eq!(unmeasurable, 0.0);
        assert!(raster_due(Some(Duration::ZERO), unmeasurable));
    }

    /// THE end that must not regress: a pointer being aimed is paced exactly as it was before
    /// pacing existed. Anything at or under `DELIBERATE_SPEED` rasters on every single frame.
    #[test]
    fn an_aiming_pointer_is_never_paced() {
        for speed in [0.0, 1.0, 120.0, 599.0, DELIBERATE_SPEED] {
            assert_eq!(raster_min_interval(speed), Duration::ZERO, "{speed} pt/s was paced");
            assert!(raster_due(Some(Duration::ZERO), speed), "{speed} pt/s was refused a raster");
        }
    }

    /// THE other end, and the one the owner explicitly asked about: the frame after the hand
    /// stops is already stationary, so the lens catches up immediately. No decay, no timer.
    #[test]
    fn a_pointer_that_stops_gets_its_accurate_lens_back_on_the_next_frame() {
        let settled = sample_speed((2000.0, 500.0), (2000.0, 500.0), FRAME_120);
        assert_eq!(settled, 0.0);
        assert_eq!(raster_min_interval(settled), Duration::ZERO);
        // Even with a raster only microseconds old, having stopped means due NOW.
        assert!(raster_due(Some(Duration::from_micros(1)), settled));
    }

    /// A full-screen flick, which is the case this exists for: the owner's 3456-point display
    /// crossed in ~200ms. It saturates the ramp, so the content refreshes at
    /// `RASTER_MAX_INTERVAL` instead of every frame.
    #[test]
    fn a_full_screen_flick_is_paced_to_the_ceiling() {
        let flick = sample_speed((0.0, 700.0), (3456.0, 700.0), Duration::from_millis(200));
        assert!(flick > FLICK_SPEED, "a screen-crossing flick should saturate, got {flick} pt/s");
        assert_eq!(raster_min_interval(flick), RASTER_MAX_INTERVAL);
        // Mid-flick, one frame after a raster, it declines…
        assert!(!raster_due(Some(FRAME_120), flick));
        assert!(!raster_due(Some(FRAME_60), flick));
        // …and once the ceiling has passed, it takes one.
        assert!(raster_due(Some(RASTER_MAX_INTERVAL), flick));
    }

    /// What the pacing is actually worth on the owner's hardware, stated as a test so the
    /// claim in `raster_min_interval`'s doc cannot rot: a flick that would have rastered on
    /// every one of 120 frames a second refreshes 25 times instead.
    #[test]
    fn the_ceiling_is_a_reduction_and_not_a_freeze() {
        let per_sec = 1.0 / RASTER_MAX_INTERVAL.as_secs_f32();
        assert!((20.0..=30.0).contains(&per_sec), "{per_sec}/s is no longer 'less often'");
        let frames_120 = 1.0 / FRAME_120.as_secs_f32();
        assert!(per_sec < frames_120 / 3.0, "the ceiling saves less than two thirds of the work");
    }

    /// It is a RAMP, not a switch: the pacing only ever grows with speed, so a hand wavering
    /// near a threshold changes how often the lens refreshes by a little rather than freezing
    /// and unfreezing it.
    #[test]
    fn the_pacing_only_ever_grows_with_speed() {
        let mut last = Duration::ZERO;
        let mut speed = 0.0f32;
        while speed <= FLICK_SPEED * 1.5 {
            let got = raster_min_interval(speed);
            assert!(got >= last, "the ramp went backwards at {speed} pt/s");
            assert!(got <= RASTER_MAX_INTERVAL, "the ramp overshot its ceiling at {speed} pt/s");
            last = got;
            speed += 25.0;
        }
        // And it really does move in between, or it would be a switch wearing a ramp's name.
        let mid = raster_min_interval((DELIBERATE_SPEED + FLICK_SPEED) / 2.0);
        assert!(
            mid > Duration::ZERO && mid < RASTER_MAX_INTERVAL,
            "the middle is not a ramp: {mid:?}"
        );
    }

    /// Nonsense must not become a throttle. NaN can only mean the speed is unknown, and an
    /// unknown speed rasters.
    #[test]
    fn a_speed_that_is_not_a_number_rasters() {
        assert_eq!(raster_min_interval(f32::NAN), Duration::ZERO);
        assert!(raster_due(Some(Duration::ZERO), f32::NAN));
        // A negative cannot arise from `sample_speed`, but the ramp must not invert if it did.
        assert_eq!(raster_min_interval(-1.0), Duration::ZERO);
    }

    /// A picker that has never rastered is ALWAYS due, at any speed. Otherwise a session that
    /// opened under a hand already in motion could show no lens at all until it stopped.
    #[test]
    fn the_very_first_raster_is_never_paced() {
        for speed in [0.0, DELIBERATE_SPEED, FLICK_SPEED, 50_000.0] {
            assert!(raster_due(None, speed), "the first raster was paced at {speed} pt/s");
        }
    }
}

/// DRAGON-599: the keyboard nudge's own arithmetic — how the offset accumulates, what clears
/// it, and how far one press is.
///
/// The reachability half (that a nudged sample still lands on every pixel including the
/// corners, and never leaves the surface) lives in `edge_pixel_tests` on purpose, beside the
/// pointer's version of the same guarantee, so the two can never be proved by different rules.
#[cfg(test)]
mod nudge_tests {
    use super::*;

    /// **The bug this test exists to prevent.** A real pointer motion RESETS the offset. If it
    /// accumulated instead, every mouse move would add to the displacement and the lens would
    /// drift permanently away from the cursor with no way back short of relaunching.
    #[test]
    fn a_pointer_motion_resets_the_offset() {
        let after_keys = nudge_after(nudge_after((0, 0), SampleMove::Keys(3, -2)), SampleMove::Keys(1, 1));
        assert_eq!(after_keys, (4, -1), "keys accumulate");
        assert_eq!(nudge_after(after_keys, SampleMove::Pointer), (0, 0));
        // And it stays reset: the mouse does not have to be moved twice.
        assert_eq!(nudge_after((0, 0), SampleMove::Pointer), (0, 0));
        // Even a huge accumulated offset is cleared by ONE motion.
        assert_eq!(nudge_after((10_000, -10_000), SampleMove::Pointer), (0, 0));
    }

    /// Keys accumulate, in both directions, and opposite presses cancel exactly — so walking
    /// four left and four right comes back to the pointer's own pixel rather than somewhere
    /// near it.
    #[test]
    fn opposite_presses_cancel_exactly() {
        let mut n = (0, 0);
        for _ in 0..4 {
            n = nudge_after(n, SampleMove::Keys(-1, 0));
        }
        assert_eq!(n, (-4, 0));
        for _ in 0..4 {
            n = nudge_after(n, SampleMove::Keys(1, 0));
        }
        assert_eq!(n, (0, 0), "back on the pointer, exactly");
    }

    /// A key held against a wall for a very long time saturates rather than wrapping. The
    /// clamp that actually keeps the sample on screen is `nudged_sample`'s; this only has to
    /// not produce nonsense.
    #[test]
    fn a_very_long_hold_saturates_instead_of_wrapping() {
        assert_eq!(nudge_after((i32::MAX, i32::MIN), SampleMove::Keys(1, -1)), (i32::MAX, i32::MIN));
    }

    /// The step is the surface's own points-per-source-pixel, per axis, so one press is one
    /// pixel whatever the display's scale or aspect.
    #[test]
    fn the_step_is_one_source_pixel_in_points() {
        assert_eq!(nudge_step((1920.0, 1080.0), (1920, 1080)), (1.0, 1.0), "unscaled");
        assert_eq!(nudge_step((1920.0, 1080.0), (3840, 2160)), (0.5, 0.5), "2x");
        // The axes are independent: a snapshot need not share the surface's aspect.
        assert_eq!(nudge_step((1920.0, 1080.0), (960, 2160)), (2.0, 0.5));
    }

    /// A degenerate surface or snapshot falls back to one POINT per press. There is no ratio
    /// to compute there, and a picker that refused to move at all would be worse than one that
    /// moves by something sane.
    #[test]
    fn a_degenerate_surface_falls_back_to_one_point() {
        assert_eq!(nudge_step((0.0, 1080.0), (1920, 1080)), (1.0, 1.0));
        assert_eq!(nudge_step((1920.0, 1080.0), (0, 0)), (1.0, 1.0));
        assert_eq!(nudge_step((f32::NAN, f32::INFINITY), (1920, 1080)), (1.0, 1.0));
        // And the sample itself never becomes a NaN the disc maths would carry.
        let got = nudged_sample((f32::NAN, 5.0), (1, 1), (0.0, 1080.0), (1920, 1080));
        assert!(got.0.is_finite() && got.1.is_finite(), "{got:?}");
    }
}

/// DRAGON-687: the saved-palette mutations. Every gesture in the tab lands on one of
/// these, and `None` is the shared "nothing changed, save nothing" signal, so each case
/// pins both the change and the refusal.
#[cfg(test)]
mod palette_ops_tests {
    use super::*;

    fn c(r: u8, g: u8, b: u8) -> Recent {
        Recent::opaque(Srgb::new(r, g, b))
    }

    fn groups() -> Vec<Palette> {
        vec![
            Palette { name: "Warm".into(), colors: vec![c(255, 0, 0), c(255, 136, 0)] },
            Palette { name: "Cool".into(), colors: vec![c(0, 0, 255)] },
            Palette { name: "Empty".into(), colors: vec![] },
        ]
    }

    /// A fresh group takes the first free number, and deleting one frees its number for
    /// the next create rather than minting ever-higher names.
    #[test]
    fn a_new_palette_takes_the_first_free_number() {
        assert_eq!(default_palette_name(&[]), "Palette 1");
        let named = vec![Palette::new("Palette 1".into()), Palette::new("Palette 3".into())];
        assert_eq!(default_palette_name(&named), "Palette 2");
        // A renamed group does not reserve the number it started with.
        let renamed = vec![Palette::new("Reds".into())];
        assert_eq!(default_palette_name(&renamed), "Palette 1");
    }

    /// Create PREPENDS (the owner's correction; it appended until this round): the new
    /// group is FIRST, empty, first-free-numbered, and every existing group keeps its
    /// order below it.
    #[test]
    fn a_new_palette_lands_at_the_top() {
        let p = groups();
        let out = palettes_with_new(&p);
        assert_eq!(out.len(), p.len() + 1);
        assert_eq!(out[0].name, "Palette 1", "the fresh group leads");
        assert!(out[0].colors.is_empty());
        assert_eq!(out[1..], p[..], "everyone else keeps their order below it");
        // From nothing: still the top, still numbered from one.
        let first = palettes_with_new(&[]);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].name, "Palette 1");
    }

    /// THE append rule (the owner's): at the END, never the middle, and a colour the group
    /// already holds is a no-op rather than a duplicate. Alpha counts: the same colour at
    /// another transparency is a different value, the history's own rule.
    #[test]
    fn an_add_appends_at_the_end_and_never_duplicates() {
        let p = groups();
        let added = palette_append(&p, 0, c(0, 255, 0)).expect("a new colour appends");
        assert_eq!(added[0].colors.last(), Some(&c(0, 255, 0)), "at the END");
        assert_eq!(added[0].colors.len(), 3);
        assert_eq!(palette_append(&p, 0, c(255, 0, 0)), None, "a duplicate is a no-op");
        let translucent = Recent::new(Srgb::new(255, 0, 0), 128);
        assert!(palette_append(&p, 0, translucent).is_some(), "another alpha is another value");
        assert_eq!(palette_append(&p, 9, c(1, 2, 3)), None, "a group that is gone");
        // The other groups are untouched by an append.
        assert_eq!(added[1], p[1]);
    }

    /// Removing a colour, and the refusals: a stale index and a stale group.
    #[test]
    fn a_colour_can_be_forgotten() {
        let p = groups();
        let out = palette_remove_color(&p, 0, 0).expect("a live colour removes");
        assert_eq!(out[0].colors, vec![c(255, 136, 0)]);
        assert_eq!(palette_remove_color(&p, 0, 2), None);
        assert_eq!(palette_remove_color(&p, 9, 0), None);
    }

    /// The reorder slot arithmetic: `to` is an insertion slot in the ORIGINAL order, so
    /// moving right accounts for the removal, and the two no-move slots decline.
    #[test]
    fn a_reorder_moves_by_insertion_slot() {
        let list = vec![1, 2, 3, 4];
        assert_eq!(reorder(&list, 0, 2), Some(vec![2, 1, 3, 4]));
        assert_eq!(reorder(&list, 0, 4), Some(vec![2, 3, 4, 1]), "to the very end");
        assert_eq!(reorder(&list, 3, 0), Some(vec![4, 1, 2, 3]), "to the very front");
        assert_eq!(reorder(&list, 1, 1), None, "its own slot is a no-op");
        assert_eq!(reorder(&list, 1, 2), None, "and so is the slot after it");
        assert_eq!(reorder(&list, 4, 0), None, "a stale index declines");
        assert_eq!(reorder(&list, 0, 5), None, "a slot past the end declines");
    }

    /// The colour reorder rides the shared arithmetic inside its own group.
    #[test]
    fn a_colour_reorders_inside_its_group() {
        let p = groups();
        let out = palette_reorder_color(&p, 0, 0, 2).expect("a real move");
        assert_eq!(out[0].colors, vec![c(255, 136, 0), c(255, 0, 0)]);
        assert_eq!(out[1], p[1], "the other groups are untouched");
        assert_eq!(palette_reorder_color(&p, 0, 0, 1), None, "the no-move slot");
    }

    /// MOVE removes from the source and appends to the target's END; a target that
    /// already holds the colour still takes the removal (the ask is already true there).
    #[test]
    fn a_move_lands_at_the_targets_end() {
        let p = groups();
        let out = palette_move_color(&p, (0, 0), 1).expect("a real move");
        assert_eq!(out[0].colors, vec![c(255, 136, 0)]);
        assert_eq!(out[1].colors, vec![c(0, 0, 255), c(255, 0, 0)], "appended at the end");
        // Target already holds it: removed from the source, not duplicated in the target.
        let dup = vec![
            Palette { name: "A".into(), colors: vec![c(1, 1, 1)] },
            Palette { name: "B".into(), colors: vec![c(1, 1, 1)] },
        ];
        let out = palette_move_color(&dup, (0, 0), 1).expect("the removal still happens");
        assert!(out[0].colors.is_empty());
        assert_eq!(out[1].colors.len(), 1);
        assert_eq!(palette_move_color(&p, (0, 0), 0), None, "its own group is not a move");
        assert_eq!(palette_move_color(&p, (0, 9), 1), None);
        assert_eq!(palette_move_color(&p, (0, 0), 9), None);
    }

    /// COPY appends without removing, and declines when nothing would change.
    #[test]
    fn a_copy_leaves_the_source_alone() {
        let p = groups();
        let out = palette_copy_color(&p, (0, 0), 1).expect("a real copy");
        assert_eq!(out[0], p[0], "the source is untouched");
        assert_eq!(out[1].colors, vec![c(0, 0, 255), c(255, 0, 0)]);
        let dup = vec![
            Palette { name: "A".into(), colors: vec![c(1, 1, 1)] },
            Palette { name: "B".into(), colors: vec![c(1, 1, 1)] },
        ];
        assert_eq!(palette_copy_color(&dup, (0, 0), 1), None, "already there: a no-op");
    }

    /// The rename rule: trimmed, an empty commit keeps the old name, and an unchanged
    /// name saves nothing.
    #[test]
    fn a_rename_trims_and_an_empty_one_reverts() {
        let p = groups();
        let out = palette_rename(&p, 0, "  Sunset  ").expect("a real rename");
        assert_eq!(out[0].name, "Sunset");
        assert_eq!(palette_rename(&p, 0, "   "), None, "empty keeps the old name");
        assert_eq!(palette_rename(&p, 0, "Warm"), None, "unchanged saves nothing");
        assert_eq!(palette_rename(&p, 9, "X"), None);
    }

    /// The cross-process target snapshot (the pipette-to-palette pick): the exact
    /// position wins while nothing moved, the name finds a re-sorted group, and a
    /// deleted or renamed group answers `None` so the pick degrades instead of filing
    /// into whatever drifted under the index.
    #[test]
    fn a_pick_target_survives_a_resort_but_not_a_rename() {
        let p = groups(); // Warm, Cool, Empty
        assert_eq!(resolve_palette_target(&p, 1, "Cool"), Some(1), "nothing moved");
        // Re-sorted while the pick was out: the name still finds it.
        let sorted = sort_palettes(&p, PaletteSort::Alphabetical); // Cool, Empty, Warm
        assert_eq!(resolve_palette_target(&sorted, 1, "Cool"), Some(0));
        // Deleted: no target, degrade.
        let gone = palette_delete(&p, 1).unwrap();
        assert_eq!(resolve_palette_target(&gone, 1, "Cool"), None);
        // Renamed: the snapshot's name no longer names anything, degrade (the recorded
        // compromise: a rename mid-pick loses the shortcut, never the colour).
        let renamed = palette_rename(&p, 1, "Chilly").unwrap();
        assert_eq!(resolve_palette_target(&renamed, 1, "Cool"), None);
        // Two groups sharing a name: the snapshot's own position wins where it still
        // holds, and the first match answers where it does not.
        let dup = vec![Palette::new("Same".into()), Palette::new("Same".into())];
        assert_eq!(resolve_palette_target(&dup, 1, "Same"), Some(1));
        assert_eq!(resolve_palette_target(&dup, 5, "Same"), Some(0));
    }

    /// Deleting and reordering whole groups.
    #[test]
    fn groups_delete_and_reorder() {
        let p = groups();
        let out = palette_delete(&p, 1).expect("a live group deletes");
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].name, "Empty");
        assert_eq!(palette_delete(&p, 9), None);
        let out = palette_reorder_group(&p, 0, 3).expect("to the end");
        assert_eq!(
            out.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            vec!["Cool", "Empty", "Warm"]
        );
        assert_eq!(palette_reorder_group(&p, 0, 1), None, "the no-move slot");
    }
}

/// DRAGON-687's duplicate-guard audit: EVERY path that inserts a colour into a palette,
/// pinned BY NAME against the one rule ([`palette_admits`]): insert when absent, no-op
/// when present. The audit's finding was that the interactive paths were all guarded and
/// the LOAD was not, so the load's case is the one that would have failed before the fix.
#[cfg(test)]
mod duplicate_guard_tests {
    use super::*;

    fn c(r: u8, g: u8, b: u8) -> Recent {
        Recent::opaque(Srgb::new(r, g, b))
    }

    fn groups() -> Vec<Palette> {
        vec![
            Palette { name: "A".into(), colors: vec![c(1, 1, 1), c(2, 2, 2)] },
            Palette { name: "B".into(), colors: vec![c(2, 2, 2)] },
        ]
    }

    /// The PLUS button (`AddActiveToPalette`): the window's colour and alpha, through
    /// [`palette_append`].
    #[test]
    fn the_plus_button_inserts_once() {
        let p = groups();
        let window = Recent::new(Srgb::new(9, 9, 9), 200);
        let added = palette_append(&p, 0, window).expect("absent: inserts");
        assert_eq!(added[0].colors.last(), Some(&window));
        assert_eq!(palette_append(&added, 0, window), None, "present: no-op");
    }

    /// The PIPETTE delivery (`apply_handoff_palette_pick`): an OPAQUE entry, through
    /// [`palette_append`].
    #[test]
    fn the_pipette_delivery_inserts_once() {
        let p = groups();
        let picked = Recent::opaque(Srgb::new(7, 8, 9));
        let added = palette_append(&p, 1, picked).expect("absent: inserts");
        assert_eq!(
            palette_append(&added, 1, picked),
            None,
            "picking the same pixel again is a no-op (the pick still acks: the colour \
             IS in the palette, which is what was asked)"
        );
    }

    /// Every DROP-append (`DropAction::AppendToPalette`, from a harmony swatch, the
    /// active swatch or a history entry): one dispatch (`AddColorToPalette`), one rule.
    /// The three sources differ only in where their payload came from, so what is pinned
    /// per source is that the payload SHAPE each one carries hits the same guard.
    #[test]
    fn every_drop_append_inserts_once() {
        let p = groups();
        for (name, payload) in [
            ("a harmony swatch at the window's alpha", Recent::new(Srgb::new(30, 40, 50), 180)),
            ("the active swatch", Recent::new(Srgb::new(60, 70, 80), 255)),
            ("a translucent history entry", Recent::new(Srgb::new(90, 100, 110), 64)),
        ] {
            let added = palette_append(&p, 0, payload).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(palette_append(&added, 0, payload), None, "{name}: twice is once");
        }
    }

    /// The Add-to-palette SUBMENU on harmony and recents swatches: the same
    /// `AddColorToPalette` dispatch as the drops, pinned separately because the menus
    /// postdate the original rule and were the audit's stated suspects.
    #[test]
    fn the_add_to_palette_submenu_inserts_once() {
        let p = groups();
        let from_menu = Recent::new(Srgb::new(120, 130, 140), 255);
        let added = palette_append(&p, 1, from_menu).expect("absent: inserts");
        assert_eq!(palette_append(&added, 1, from_menu), None, "present: no-op");
    }

    /// MOVE-to-palette: inserts when absent; when the target already holds the colour
    /// the REMOVAL still happens (the settled semantics) and the skipped insertion is
    /// [`palette_append`]'s own answer, not a second copy of the rule.
    #[test]
    fn move_to_palette_inserts_once_and_still_removes() {
        let p = groups();
        // Absent in the target: a full move.
        let moved = palette_move_color(&p, (0, 0), 1).expect("a real move");
        assert_eq!(moved[0].colors, vec![c(2, 2, 2)]);
        assert_eq!(moved[1].colors, vec![c(2, 2, 2), c(1, 1, 1)]);
        // Present in the target: removed from the source, NOT duplicated in the target.
        let dup = palette_move_color(&p, (0, 1), 1).expect("the removal still happens");
        assert_eq!(dup[0].colors, vec![c(1, 1, 1)]);
        assert_eq!(dup[1].colors, vec![c(2, 2, 2)], "no second copy");
    }

    /// COPY-to-palette: inserts when absent, whole-action no-op when present (nothing
    /// changed, nothing saved, the menu still closes in the handler).
    #[test]
    fn copy_to_palette_inserts_once() {
        let p = groups();
        let copied = palette_copy_color(&p, (0, 0), 1).expect("absent: inserts");
        assert_eq!(copied[1].colors, vec![c(2, 2, 2), c(1, 1, 1)]);
        assert_eq!(copied[0], p[0], "the source is untouched");
        assert_eq!(palette_copy_color(&p, (0, 1), 1), None, "present: no-op");
    }

    /// The LOAD (`palette_from_saved`): the path the audit found OUTSIDE the rule. A
    /// byte-equal pair already in the file loads as ONE entry (first occurrence wins,
    /// order otherwise kept), so a duplicate can no longer outlive every guard by
    /// arriving on disk; alpha-distinct entries are two values and both survive, the
    /// settled semantics.
    #[test]
    fn the_load_dedupes_what_no_interactive_guard_could_reach() {
        let loaded = palette_from_saved(
            "File".into(),
            vec![
                c(1, 1, 1),
                c(2, 2, 2),
                c(1, 1, 1),                       // the duplicate a file can carry
                Recent::new(Srgb::new(1, 1, 1), 128), // alpha-distinct: a different value
                c(2, 2, 2),
            ],
        );
        assert_eq!(
            loaded.colors,
            vec![c(1, 1, 1), c(2, 2, 2), Recent::new(Srgb::new(1, 1, 1), 128)],
            "first wins, order kept, alpha-distinct survives"
        );
        assert_eq!(loaded.name, "File");
    }

    /// REORDER is not an insertion and never dedupes: even a list that somehow still
    /// holds a byte-equal pair (constructed here directly, since no path can make one
    /// any more) reorders with both entries intact.
    #[test]
    fn a_reorder_never_dedupes() {
        let p = vec![Palette {
            name: "Legacy".into(),
            colors: vec![c(1, 1, 1), c(2, 2, 2), c(1, 1, 1)],
        }];
        let out = palette_reorder_color(&p, 0, 2, 0).expect("a real move");
        assert_eq!(out[0].colors, vec![c(1, 1, 1), c(1, 1, 1), c(2, 2, 2)], "both survive");
    }

    /// The equality the rule runs on is BYTES, not spellings: case-different hex and the
    /// eight-digit full-alpha spelling parse to the same value, so no spelling variation
    /// in a file can manufacture an in-memory duplicate the guard cannot see.
    #[test]
    fn spelling_variants_cannot_split_the_equality() {
        let canonical = Recent::parse("#FF8800").unwrap();
        for variant in ["#ff8800", "#Ff8800", "#FF8800FF", "#ff8800ff"] {
            assert_eq!(Recent::parse(variant), Some(canonical), "{variant}");
        }
        assert!(!palette_admits(&[canonical], Recent::parse("#ff8800ff").unwrap()));
    }
}

/// DRAGON-687: the six sorts, with both formulas pinned. The luminance is the colour
/// model's own WCAG relative luminance averaged over the group; the warmth is the
/// saturation-weighted cosine of the hue's distance from orange (45 degrees), averaged.
#[cfg(test)]
mod palette_sort_tests {
    use super::*;

    fn named(name: &str, colors: Vec<Recent>) -> Palette {
        Palette { name: name.into(), colors }
    }
    fn c(r: u8, g: u8, b: u8) -> Recent {
        Recent::opaque(Srgb::new(r, g, b))
    }

    /// Alphabetical is BYTE order on the name (the repo's covermark-prefs precedent),
    /// ascending, and the reverse spelling descends.
    #[test]
    fn alphabetical_is_byte_order() {
        let p = vec![named("beta", vec![]), named("Alpha", vec![]), named("alpha", vec![])];
        let sorted = sort_palettes(&p, PaletteSort::Alphabetical);
        assert_eq!(
            sorted.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            vec!["Alpha", "alpha", "beta"],
            "byte order puts uppercase first, which is the documented choice"
        );
        let rev = sort_palettes(&p, PaletteSort::AlphabeticalReverse);
        assert_eq!(
            rev.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            vec!["beta", "alpha", "Alpha"]
        );
    }

    /// The group luminance is the MEAN of the members' relative luminance, and the sort
    /// runs dark to light with the empty groups LAST in both directions.
    #[test]
    fn luminance_runs_dark_to_light_and_empties_sort_last() {
        let dark = named("dark", vec![c(0, 0, 0), c(40, 40, 40)]);
        let light = named("light", vec![c(255, 255, 255)]);
        let empty = named("empty", vec![]);
        assert!(group_luminance(&dark).unwrap() < group_luminance(&light).unwrap());
        assert_eq!(group_luminance(&empty), None);
        let p = vec![light.clone(), empty.clone(), dark.clone()];
        let asc = sort_palettes(&p, PaletteSort::Luminance);
        assert_eq!(
            asc.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            vec!["dark", "light", "empty"],
            "ascending, empty last"
        );
        let desc = sort_palettes(&p, PaletteSort::LuminanceReverse);
        assert_eq!(
            desc.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            vec!["light", "dark", "empty"],
            "descending, empty STILL last: reversing answers, not the unanswerable"
        );
    }

    /// The warmth formula, at its own landmarks: orange is the peak, azure the trough, the
    /// two crossings read neutral, and a grey contributes nothing because saturation
    /// weights the cosine.
    #[test]
    fn warmth_is_the_saturation_weighted_cosine_from_orange() {
        let warmth = |r, g, b| group_warmth(&named("x", vec![c(r, g, b)])).unwrap();
        let orange = warmth(255, 128, 0); // hue ~30
        let red = warmth(255, 0, 0); // hue 0
        let azure = warmth(0, 128, 255); // hue ~210
        let blue = warmth(0, 0, 255); // hue 240
        let grey = warmth(128, 128, 128);
        assert!(orange > 0.9, "orange is the warm peak: {orange}");
        assert!(red > 0.5, "red is warm: {red}");
        assert!(azure < -0.9, "azure is the cool trough: {azure}");
        assert!(blue < -0.5, "blue is cool: {blue}");
        assert!(grey.abs() < 1e-9, "a grey has no hue and no warmth: {grey}");
        // Saturation weighting: a washed-out red is less warm than a vivid one.
        assert!(warmth(200, 150, 150) < red);
        // And the sort runs cool to warm ascending, empties last, warm to cool reversed.
        let p = vec![
            named("warm", vec![c(255, 128, 0)]),
            named("empty", vec![]),
            named("cool", vec![c(0, 128, 255)]),
        ];
        let asc = sort_palettes(&p, PaletteSort::CoolToWarm);
        assert_eq!(
            asc.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            vec!["cool", "warm", "empty"]
        );
        let desc = sort_palettes(&p, PaletteSort::WarmToCool);
        assert_eq!(
            desc.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            vec!["warm", "cool", "empty"]
        );
    }

    /// Every sort is STABLE: groups that compare equal keep their order, so re-applying a
    /// sort shuffles nothing.
    #[test]
    fn equal_groups_keep_their_order() {
        let p = vec![
            named("same", vec![c(10, 10, 10)]),
            named("same", vec![c(10, 10, 10)]),
        ];
        for sort in PaletteSort::ALL {
            let sorted = sort_palettes(&p, sort);
            assert_eq!(sorted, sort_palettes(&sorted, sort), "{sort:?} must be idempotent");
        }
    }
}

/// DRAGON-687: the palettes tab's layout numbers and the two insertion-slot maps.
#[cfg(test)]
mod palette_layout_tests {
    use super::*;

    /// The UX round's row shape: the bar is FULL card width again (the icons live in the
    /// title row, so the bar shrinks for nothing), and the title text's own budget is
    /// what gives the right-aligned pair and the hover pencil their room.
    #[test]
    fn the_bar_is_full_width_and_the_title_leaves_room_for_the_icons() {
        assert_eq!(bar_w(), card_w(), "a palette bar IS a harmony bar now");
        assert!(
            (palette_title_w()
                + 2.0 * (RECENT_SWATCH + PALETTE_PLUS_GAP)
                + f32::from(PANEL_HINT_ICON)
                + PANEL_HINT_GAP
                - card_w())
            .abs()
                < 0.01,
            "the title, the pencil's room and the icon pair must span the card exactly"
        );
        // The truncation predicate that gates the full-name tooltip: a short name never
        // offers it, an absurd one always does.
        assert!(!palette_title_truncates("Sunset"));
        assert!(palette_title_truncates(&"x".repeat(200)));
    }

    /// The two tabs have their OWN group heights since the icons moved into the palette
    /// title row: a palette group is one icon-button-tall title over its bar, and it is
    /// TALLER than a text-headed harmony group, which every per-tab consumer now picks
    /// explicitly instead of assuming the old parity.
    #[test]
    fn a_palette_group_is_a_title_row_over_its_bar() {
        assert_eq!(palette_group_h(), PALETTE_TITLE_ROW_H + PANEL_HEADING_GAP + PANEL_SWATCH);
        assert!(palette_group_h() > harmony_group_h(), "the title row is a button tall");
        assert_eq!(palette_group_offset(0), 0.0);
        assert_eq!(palette_group_offset(2), 2.0 * (palette_group_h() + PANEL_GROUP_GAP));
    }

    /// THE spacing derivation (the owner's ask): the Harmonies tab's five groups plus the
    /// four derived gaps fill its real viewport exactly to the point, so there is no
    /// trailing dead block and no scroll; and the derived gap is the shared default, well
    /// clear of its cramp floor.
    #[test]
    fn the_gap_constant_fills_the_harmonies_viewport() {
        let n = crate::color::Harmony::ALL.len() as f32;
        let content = n * harmony_group_h() + (n - 1.0) * PANEL_GROUP_GAP;
        let leftover = harmonies_viewport_h() - content;
        assert!(
            leftover >= 0.0,
            "the groups must fit the viewport with no scroll: {leftover}pt over of {}pt",
            harmonies_viewport_h()
        );
        // The fill exercise divided to 36 exactly; the owner's two-point tightening then
        // leaves exactly two points per seam as trailing slack, never a scroll.
        assert_eq!(
            leftover,
            2.0 * (n - 1.0),
            "two points per seam trail after the last group"
        );
    }

    /// The COLOUR slot map: each segment's middle is the boundary, the ends append, and
    /// every slot the map answers is a legal insertion slot.
    #[test]
    fn the_colour_slot_is_the_nearest_seam() {
        let bar_left = WINDOW_BORDER + picker_column_w() + WINDOW_PADDING;
        let n = 4;
        let widths = segment_widths(n);
        // Just inside the first segment's left half: slot 0.
        assert_eq!(palette_color_slot((bar_left + 1.0, 0.0), n), 0);
        // Just past the first segment's middle: slot 1.
        assert_eq!(palette_color_slot((bar_left + widths[0] / 2.0 + 1.0, 0.0), n), 1);
        // Past the bar's right edge: the append slot.
        assert_eq!(palette_color_slot((bar_left + bar_w() + 50.0, 0.0), n), n);
        // Left of the bar entirely: the front.
        assert_eq!(palette_color_slot((bar_left - 50.0, 0.0), n), 0);
    }

    /// The GROUP slot map goes through the scroll offset, and its boundaries are the
    /// block middles.
    #[test]
    fn the_group_slot_reads_through_the_scroll() {
        let sh = PanelShape { palettes: true, scroll: 0.0, groups: vec![1, 1, 1] };
        let top = palettes_scroll_top();
        assert_eq!(palette_group_slot((0.0, top + 1.0), &sh), 0);
        // Past the first block's middle: before the second.
        assert_eq!(
            palette_group_slot((0.0, top + palette_group_h() / 2.0 + 1.0), &sh),
            1
        );
        // Below everything: the end slot.
        assert_eq!(palette_group_slot((0.0, top + palettes_content_h(3) + 40.0), &sh), 3);
        // Scrolled a full pitch: the same window point means one slot further down.
        let scrolled = PanelShape { scroll: palette_group_h() + PANEL_GROUP_GAP, ..sh };
        assert_eq!(
            palette_group_slot((0.0, top + palette_group_h() / 2.0 + 1.0), &scrolled),
            2
        );
    }

    /// The insertion LINES sit at the slot boundaries they mark.
    #[test]
    fn the_insertion_lines_mark_their_slots() {
        assert_eq!(palette_insert_line_x(0, 3), 0.0);
        let widths = segment_widths(3);
        assert!((palette_insert_line_x(1, 3) - widths[0]).abs() < 0.01);
        assert!((palette_insert_line_x(3, 3) - bar_w()).abs() < 0.01);
        assert_eq!(palette_group_line_y(0, 3), 0.0);
        assert!(
            (palette_group_line_y(3, 3) - (palettes_content_h(3) - PANEL_GROUP_GAP / 2.0))
                .abs()
                < 0.01,
            "the end slot's line is the content's own end"
        );
        assert!(
            (palette_group_line_y(1, 3)
                - (palette_group_offset(1) - PANEL_GROUP_GAP / 2.0))
                .abs()
                < 0.01
        );
    }

    /// The pencil's visibility rect (the stranded-pencil fix): ON a title, left of the
    /// icon pair, at any scroll (content-local coords make scroll the widget's problem);
    /// OFF on the icons, the bar row, the gaps, past the last group, and with no
    /// pointer at all, which is what a stranded flag could never guarantee.
    #[test]
    fn the_pencil_shows_exactly_on_the_title_side_of_a_title_row() {
        let n = 3;
        let pitch = palette_group_h() + PANEL_GROUP_GAP;
        let icons_left = card_w() - 2.0 * (RECENT_SWATCH + PALETTE_PLUS_GAP);
        // On each group's title, at its left edge and just left of the icons.
        for g in 0..n {
            let y = g as f32 * pitch + PALETTE_TITLE_ROW_H / 2.0;
            assert_eq!(hovered_palette_title(Some((1.0, y)), n), Some(g));
            assert_eq!(hovered_palette_title(Some((icons_left - 1.0, y)), n), Some(g));
            // The icon pair's side of the same row is NOT the title.
            assert_eq!(hovered_palette_title(Some((icons_left + 1.0, y)), n), None);
            // The bar row below it is not either.
            let bar_y = g as f32 * pitch + PALETTE_TITLE_ROW_H + PANEL_HEADING_GAP + 1.0;
            assert_eq!(hovered_palette_title(Some((1.0, bar_y)), n), None);
        }
        // The gap between groups, past the last group, and no pointer at all.
        assert_eq!(
            hovered_palette_title(Some((1.0, palette_group_h() + 1.0)), n),
            None,
            "the inter-group gap is nobody's title"
        );
        assert_eq!(hovered_palette_title(Some((1.0, n as f32 * pitch + 5.0)), n), None);
        assert_eq!(hovered_palette_title(None, n), None);
        assert_eq!(hovered_palette_title(Some((-1.0, 5.0)), n), None);
    }

    /// THE OWNER'S REPRO, pinned (the pencil's second stranding): from the first title,
    /// the pointer moves UP into the create row and the tab strip. The window-level
    /// source keeps reporting there, and every position above the scroll viewport
    /// answers NO title, scrolled or not, so the pencil clears where the region-scoped
    /// report went silent and left it stuck.
    #[test]
    fn a_pointer_above_the_content_region_hovers_no_title() {
        let w = color_window_size_expanded();
        let n = 3;
        // The pointer x that IS over a title's own band, so only the y decides.
        let x = panel_content_left() + 4.0;
        // On the first title: the pencil shows (the mapping round-trips).
        let on_title = (x, palettes_scroll_top() + PALETTE_TITLE_ROW_H / 2.0);
        assert_eq!(hovered_palette_title_at(Some(on_title), w, 0.0, n, true), Some(0));
        // Moving UP, through the create row and the tab strip to the header: no title,
        // at every stop, which is exactly the strand's path.
        for y in [
            palettes_scroll_top() - 1.0,                       // the create row's bottom
            palettes_scroll_top() - PALETTE_CREATE_ROW_H / 2.0, // mid create row
            WINDOW_BORDER + header_h() + WINDOW_PADDING + 5.0,   // the tab strip
            WINDOW_BORDER + header_h() / 2.0,                    // the header itself
        ] {
            assert_eq!(hovered_palette_title_at(Some((x, y)), w, 0.0, n, true), None, "y={y}");
            // The SCROLLED failing case: the naive mapping would put the create row at
            // `-20 + scroll`, a positive content y inside some title rect. The viewport
            // clip is what answers first.
            let pitch = palette_group_h() + PANEL_GROUP_GAP;
            assert_eq!(
                hovered_palette_title_at(Some((x, y)), w, pitch, n, true),
                None,
                "scrolled, y={y}"
            );
        }
        // Below the viewport (the window's own padding band): no title either.
        let below = (x, panel_scroll_bottom(w.1) + 2.0);
        assert_eq!(hovered_palette_title_at(Some(below), w, 0.0, n, true), None);
        // The scroll mirror maps a visible SECOND group's title correctly.
        let pitch = palette_group_h() + PANEL_GROUP_GAP;
        assert_eq!(hovered_palette_title_at(Some(on_title), w, pitch, n, true), Some(1));
        // Not on the palettes tab: nothing, wherever the pointer is.
        assert_eq!(hovered_palette_title_at(Some(on_title), w, 0.0, n, false), None);
        // And no pointer at all is no title, the absence-clears contract.
        assert_eq!(hovered_palette_title_at(None, w, 0.0, n, true), None);
    }

    /// The max-scroll clamp: zero while the list fits, and exactly the overflow once it
    /// does not.
    #[test]
    fn the_scroll_clamp_is_the_overflow() {
        let h = color_window_size().1;
        assert_eq!(palettes_max_scroll(h, 0), 0.0);
        assert_eq!(palettes_max_scroll(h, 1), 0.0, "one group always fits");
        let many = 20;
        let expect = palettes_content_h(many) - (panel_scroll_bottom(h) - palettes_scroll_top());
        assert!(expect > 0.0, "twenty groups must overflow this window");
        assert!((palettes_max_scroll(h, many) - expect).abs() < 0.01);
    }
}

/// DRAGON-687 items six, eight and nine: the filter, the row window, and the one
/// scroll-extent source.
#[cfg(test)]
mod filter_and_window_tests {
    use super::*;

    fn named(names: &[&str]) -> Vec<Palette> {
        names.iter().map(|n| Palette::new((*n).to_string())).collect()
    }

    /// The filter (item six): case-insensitive substring over the names, blank shows
    /// all, order kept, indices REAL.
    #[test]
    fn the_filter_matches_names_case_insensitively() {
        let p = named(&["Catppuccin Latte", "Sunset", "catppuccin mocha", "Reds"]);
        assert_eq!(visible_palettes(&p, ""), vec![0, 1, 2, 3]);
        assert_eq!(visible_palettes(&p, "   "), vec![0, 1, 2, 3], "whitespace is blank");
        assert_eq!(visible_palettes(&p, "CATPPUCCIN"), vec![0, 2]);
        assert_eq!(visible_palettes(&p, "sun"), vec![1]);
        assert_eq!(visible_palettes(&p, "zzz"), Vec::<usize>::new());
        // Substring, not prefix: mid-name matches count.
        assert_eq!(visible_palettes(&p, "mocha"), vec![2]);
    }

    /// The name-drag slot mapping under a filter (item six): a visible slot inserts
    /// before its anchoring REAL group, and the end slot goes to the very end.
    #[test]
    fn a_visible_slot_maps_before_its_real_anchor() {
        let visible = vec![1, 4, 6];
        assert_eq!(visible_slot_to_real(&visible, 0, 8), 1);
        assert_eq!(visible_slot_to_real(&visible, 2, 8), 6);
        assert_eq!(visible_slot_to_real(&visible, 3, 8), 8, "past the last row: the end");
    }

    /// The row window (item eight): the extents, the middle, a short list, and the
    /// buffer, over the FILTERED row count (the window is rows-in, rows-out; which real
    /// groups those rows are is the filter's business).
    #[test]
    fn the_row_window_tracks_the_scroll() {
        let pitch = palette_group_h() + PANEL_GROUP_GAP;
        let viewport = 5.0 * pitch;
        // At the top: starts at zero, no top spacer.
        let w = visible_row_window(100, 0.0, viewport, None);
        assert_eq!(w.first, 0);
        assert!(w.top.is_none());
        assert!(w.last >= 5 + VIRTUAL_ROW_BUFFER && w.last <= 6 + VIRTUAL_ROW_BUFFER);
        assert!(w.bottom.is_some());
        // Mid-list: buffered both sides.
        let w = visible_row_window(100, 50.0 * pitch, viewport, None);
        assert_eq!(w.first, 50 - VIRTUAL_ROW_BUFFER);
        assert!(w.last <= 56 + VIRTUAL_ROW_BUFFER);
        assert!(w.top.is_some() && w.bottom.is_some());
        // The bottom: reaches the last row, no bottom spacer.
        let max = palettes_max_scroll_rows(100);
        let w = visible_row_window(100, max, viewport, None);
        assert_eq!(w.last, 100);
        assert!(w.bottom.is_none());
        // A short list builds everything with no spacers at all.
        let w = visible_row_window(3, 0.0, viewport, None);
        assert_eq!((w.first, w.last), (0, 3));
        assert!(w.top.is_none() && w.bottom.is_none());
        // Empty is empty.
        assert_eq!(visible_row_window(0, 0.0, viewport, None).last, 0);
    }

    /// The KEPT row (item eight's rename rule): a named row outside the viewport is
    /// still built, the range stretches contiguously to reach it, and the extent
    /// invariant below holds unchanged because the spacers derive from the stretched
    /// bounds. A kept row the list does not have keeps nothing.
    #[test]
    fn a_kept_row_is_always_built() {
        let pitch = palette_group_h() + PANEL_GROUP_GAP;
        let viewport = 4.0 * pitch;
        let w = visible_row_window(100, 50.0 * pitch, viewport, Some(2));
        assert_eq!(w.first, 2, "the window stretches up to the kept row");
        let w = visible_row_window(100, 0.0, viewport, Some(90));
        assert_eq!(w.last, 91, "and down to it");
        assert_eq!(
            visible_row_window(100, 0.0, viewport, Some(100)),
            visible_row_window(100, 0.0, viewport, None),
            "an out-of-range keep changes nothing"
        );
        // The extent invariant survives the stretch.
        let w = visible_row_window(100, 50.0 * pitch, viewport, Some(2));
        let built = (w.last - w.first) as f32;
        let mut children = built;
        let mut sum = built * palette_group_h();
        if let Some(t) = w.top {
            sum += t;
            children += 1.0;
        }
        if let Some(b) = w.bottom {
            sum += b;
            children += 1.0;
        }
        sum += (children - 1.0).max(0.0) * PANEL_GROUP_GAP;
        // The container's trailing pad (item four), outside the column.
        sum += PANEL_GROUP_GAP;
        assert!((sum - palettes_content_h(100)).abs() < 0.01);
    }

    /// A convenience for these tests: the max scroll at the standard window height.
    fn palettes_max_scroll_rows(rows: usize) -> f32 {
        palettes_max_scroll(color_window_size().1, rows)
    }

    /// THE virtualization invariant (item eight, feeding item nine): built rows plus
    /// spacers plus the column's own inter-child gaps reconstruct the full content
    /// extent EXACTLY, at the top, the middle and the bottom, so the widget's scrollbar
    /// and its own maximum extent are unchanged by windowing.
    #[test]
    fn the_row_window_preserves_the_content_extent() {
        let pitch = palette_group_h() + PANEL_GROUP_GAP;
        let viewport = 4.0 * pitch;
        for rows in [1usize, 4, 12, 100, 10_000] {
            for scroll in [0.0, 2.5 * pitch, palettes_max_scroll_rows(rows)] {
                let w = visible_row_window(rows, scroll, viewport, None);
                let built = (w.last - w.first) as f32;
                // The column: [top?] built rows [bottom?], PANEL_GROUP_GAP between each
                // adjacent pair of children.
                let mut children = built;
                let mut sum = built * palette_group_h();
                if let Some(t) = w.top {
                    sum += t;
                    children += 1.0;
                }
                if let Some(b) = w.bottom {
                    sum += b;
                    children += 1.0;
                }
                sum += (children - 1.0).max(0.0) * PANEL_GROUP_GAP;
                // Plus the tab's trailing pad (item four), which lives on the content
                // CONTAINER, outside the windowed column these spacers reconstruct.
                sum += PANEL_GROUP_GAP;
                assert!(
                    (sum - palettes_content_h(rows)).abs() < 0.01,
                    "rows={rows} scroll={scroll}: windowed extent {sum} vs full {}",
                    palettes_content_h(rows)
                );
            }
        }
    }

    /// ITEM NINE's pin: the auto-scroll clamp, the user-scroll extent and the content
    /// height are ONE derivation, so the drag edge reaches the very bottom: the clamp
    /// equals content minus viewport exactly, for several shapes, trailing gap included,
    /// and under a filter it is the VISIBLE count's own answer.
    ///
    /// The observed few-pixel shortfall was the `HEADER_H = 44` fiction reaching the
    /// clamp through the viewport term: the real CSD header is 47pt at Standard density,
    /// so the derived viewport was 3pt too tall and the clamp 3pt short of the true
    /// extent, the same stale term the drop-zone offset traced to (`header_h`'s doc).
    /// One source, one fix, both symptoms.
    #[test]
    fn the_autoscroll_clamp_reaches_the_true_extent() {
        let h = color_window_size().1;
        let viewport = panel_scroll_bottom(h) - palettes_scroll_top();
        for rows in [5usize, 6, 23, 10_000] {
            let expect = (palettes_content_h(rows) - viewport).max(0.0);
            assert!(
                (palettes_max_scroll(h, rows) - expect).abs() < 0.001,
                "rows={rows}"
            );
            // The trailing anatomy is counted: adding one row moves the extent by
            // exactly one pitch once the list overflows.
            if palettes_max_scroll(h, rows) > 0.0 {
                assert!(
                    (palettes_max_scroll(h, rows + 1) - palettes_max_scroll(h, rows)
                        - (palette_group_h() + PANEL_GROUP_GAP))
                        .abs()
                        < 0.001
                );
            }
        }
        // Under a filter the extent is the visible list's: the same function over the
        // visible count, which is what every caller passes now.
        let p: Vec<Palette> =
            (0..40).map(|i| Palette::new(format!("{}{i}", if i % 2 == 0 { "A" } else { "B" }))).collect();
        let visible = visible_palettes(&p, "a");
        assert_eq!(visible.len(), 20);
        let filtered_max = palettes_max_scroll(h, visible.len());
        assert!(filtered_max < palettes_max_scroll(h, p.len()));
        assert!((filtered_max - (palettes_content_h(20) - viewport).max(0.0)).abs() < 0.001);
    }

    /// The main swatch's menu (item seven): recents, the gated palette row, copy, and
    /// never a set-active (it IS the active colour).
    #[test]
    fn the_main_swatch_menu_offers_no_set_active() {
        assert_eq!(
            main_swatch_menu_labels(0),
            vec![ADD_TO_RECENTS_LABEL, COPY_COLOR_LABEL]
        );
        assert_eq!(
            main_swatch_menu_labels(2),
            vec![ADD_TO_RECENTS_LABEL, ADD_TO_PALETTE_LABEL, COPY_COLOR_LABEL]
        );
        assert!(!main_swatch_menu_labels(5).contains(&SET_ACTIVE_LABEL));
    }
}

/// DRAGON-687 item ten: the outgoing-colour bump at the one apply path (item five's
/// click bump, generalised), and the click gesture per surface.
#[cfg(test)]
mod files_outgoing_tests {
    use super::*;

    fn c(r: u8, g: u8, b: u8, a: u8) -> Recent {
        Recent::new(Srgb::new(r, g, b), a)
    }

    /// THE table: every discrete source bumps an unsaved outgoing colour, Edit never
    /// does, and the window-open gate stops the state's DEFAULT being filed by the
    /// launch loads.
    #[test]
    fn discrete_sources_bump_and_edit_never_does() {
        let outgoing = c(10, 20, 30, 255);
        let incoming = c(200, 100, 50, 128);
        for source in [ColorSource::Pick, ColorSource::RecentClick, ColorSource::Harmony] {
            assert!(files_outgoing(true, source, outgoing, incoming, &[]), "{source:?}");
        }
        assert!(
            !files_outgoing(true, ColorSource::Edit, outgoing, incoming, &[]),
            "continuous editing waits for the explicit Add"
        );
        assert!(
            !files_outgoing(false, ColorSource::RecentClick, outgoing, incoming, &[]),
            "a window-open load replaces the default, not a held colour"
        );
    }

    /// The absent-check: an outgoing colour the history already holds is not filed
    /// again, and alpha counts (the same colour at another alpha is absent).
    #[test]
    fn a_present_outgoing_colour_is_not_filed_twice() {
        let outgoing = c(10, 20, 30, 255);
        let incoming = c(200, 100, 50, 128);
        let held = [c(1, 2, 3, 255), outgoing];
        assert!(!files_outgoing(true, ColorSource::RecentClick, outgoing, incoming, &held));
        let other_alpha = [c(10, 20, 30, 128)];
        assert!(
            files_outgoing(true, ColorSource::RecentClick, outgoing, incoming, &other_alpha),
            "the same colour at another alpha is a different entry"
        );
    }

    /// Replacing a colour with ITSELF bumps nothing (nothing is going missing), and
    /// alpha counts both ways: the same colour at another alpha IS a change and bumps.
    #[test]
    fn replacing_a_colour_with_itself_bumps_nothing() {
        let current = c(10, 20, 30, 255);
        assert!(!files_outgoing(true, ColorSource::RecentClick, current, current, &[]));
        let other_alpha = c(10, 20, 30, 128);
        assert!(files_outgoing(true, ColorSource::RecentClick, current, other_alpha, &[]));
    }

    /// The lost-release invariant (DRAGON-687): an armed-but-not-live machine DISARMS
    /// on any release, under EVERY source and wherever the release lands; a live drag
    /// never disarms here (its release is the drop); no machine, no-op. The bug this
    /// pins against: a tap's release was dispatched before the drag-gated release
    /// listener existed, nothing cleared the armed machine, and the next idle mouse
    /// move became a buttonless drag.
    #[test]
    fn any_release_disarms_an_armed_machine_and_never_a_live_drag() {
        use DragSource::{Active, Harmony, PaletteName, PaletteSwatch, Recent};
        for source in
            [Active, Recent(3), Harmony(1, 2), PaletteSwatch(0, 4), PaletteName(2)]
        {
            assert!(release_disarms(Some((source, false))), "{source:?} armed must disarm");
            assert!(!release_disarms(Some((source, true))), "{source:?} live is the drop's");
        }
        assert!(!release_disarms(None), "no machine, nothing to disarm");
    }

    /// The APPLY path's ordering: the click decision and the disarm read the SAME
    /// snapshot, so on the swatch that was pressed both answer yes at once, and the
    /// disarm can never eat the apply. On any OTHER swatch the click declines while the
    /// disarm still fires, which is the boundary-release case that used to leave the
    /// machine armed.
    #[test]
    fn a_click_release_both_applies_and_disarms_from_one_snapshot() {
        let pressed = DragSource::PaletteSwatch(1, 2);
        let snapshot = Some((pressed, false));
        assert!(completes_click(snapshot, pressed) && release_disarms(snapshot));
        let elsewhere = DragSource::Recent(0);
        assert!(!completes_click(snapshot, elsewhere) && release_disarms(snapshot));
    }

    /// The click GESTURE, per surface: a sub-threshold press-release on the pressed
    /// swatch is the click, on harmonies and saved palettes alike; a drag past the
    /// threshold is never one, and neither is a release over a swatch whose press
    /// happened elsewhere. `completes_click` was already the recents' rule; these pins
    /// are the two panel surfaces joining it.
    #[test]
    fn the_click_gesture_holds_on_both_panel_surfaces() {
        use DragSource::{Harmony, PaletteSwatch};
        for source in [Harmony(1, 2), PaletteSwatch(0, 3)] {
            assert!(completes_click(Some((source, false)), source), "{source:?}");
            assert!(!completes_click(Some((source, true)), source), "a drag never applies");
            assert!(!completes_click(None, source));
        }
        assert!(!completes_click(Some((Harmony(1, 2), false)), PaletteSwatch(1, 2)));
        assert!(!completes_click(Some((Harmony(0, 0), false)), Harmony(0, 1)));
    }
}

/// DRAGON-687's UX round: the per-tab scroll memory's exchange, and its clamps.
#[cfg(test)]
mod scroll_memory_tests {
    use super::*;
    use PanelTab::{Harmonies, Palettes};

    /// THE round trip (the owner's ask): scroll the palettes, visit Harmonies, come
    /// back, and the palettes are where you left them; the live value always lands in
    /// the departing tab's slot and comes back out of the arriving one.
    #[test]
    fn switching_away_and_back_restores_the_offset() {
        let mem = [0.0, 0.0];
        // On Palettes, scrolled to 210. Leave for Harmonies (whose max is 0 today).
        let (mem, live) = scroll_exchange(mem, 210.0, Palettes, Harmonies, 0.0);
        assert_eq!(live, 0.0, "harmonies has nowhere to scroll");
        assert_eq!(mem[Palettes.index()], 210.0, "the palettes offset is remembered");
        // Come back: the palettes resume at 210.
        let (mem, live) = scroll_exchange(mem, live, Harmonies, Palettes, 500.0);
        assert_eq!(live, 210.0, "no re-scrolling (the owner's ask)");
        assert_eq!(mem[Palettes.index()], 210.0);
    }

    /// The restore CLAMPS to the tab's CURRENT extent: groups can be deleted or
    /// re-sorted while the other tab was showing, and a stale offset lands at the
    /// nearest valid position (never past the end, never negative), with the clamped
    /// value written back so the mirror and the widget agree.
    #[test]
    fn a_stale_offset_clamps_to_the_current_extent() {
        let mem = [0.0, 400.0];
        // The list shrank while Harmonies was showing: max is 120 now.
        let (mem, live) = scroll_exchange(mem, 0.0, Harmonies, Palettes, 120.0);
        assert_eq!(live, 120.0, "the nearest valid position");
        assert_eq!(mem[Palettes.index()], 120.0, "the memory heals to the clamp");
        // Junk defends: a negative remembered value (or a negative max) answers zero.
        let (_, live) = scroll_exchange([0.0, -5.0], 0.0, Harmonies, Palettes, 120.0);
        assert_eq!(live, 0.0);
        let (_, live) = scroll_exchange([0.0, 50.0], 0.0, Harmonies, Palettes, -1.0);
        assert_eq!(live, 0.0);
    }

    /// The drag's transient switch and its revert are the SAME exchange, paired: going
    /// live restores the palettes' remembered offset, the auto-scroll moves the live
    /// value, and the revert stores where the drag ENDED, so the next visit resumes
    /// there while the prior tab comes back to its own place.
    #[test]
    fn the_drag_switch_and_revert_pair_up() {
        // On Harmonies (offset 0), palettes remembered at 60. A drag goes live:
        let (mem, live) = scroll_exchange([0.0, 60.0], 0.0, Harmonies, Palettes, 300.0);
        assert_eq!(live, 60.0, "the drag opens the palettes where they were");
        // The auto-scroll carries the drag to 240; the drop lands; the revert:
        let (mem, live) = scroll_exchange(mem, 240.0, Palettes, Harmonies, 0.0);
        assert_eq!(live, 0.0, "harmonies back at its own place");
        assert_eq!(
            mem[Palettes.index()],
            240.0,
            "the next palettes visit resumes where the drag ended"
        );
        let _ = mem;
    }

    /// THE drag-start contract's exchange half (the owner: this tab must never move
    /// unless the user scrolls it or drags to its edges): a switch to the tab ALREADY
    /// showing is a structural no-op, no store, no restore and NO CLAMP, even when the
    /// live offset sits past our computed max (where a clamp would be the "moves up
    /// some pixels" bug) and even when the memory holds something stale (where a
    /// restore would be the "moves to the top" one).
    #[test]
    fn switching_to_the_active_tab_moves_nothing() {
        for (mem, live, max) in [
            ([0.0, 0.0], 300.0, 120.0),  // live past our max: a clamp would yank it up
            ([0.0, 0.0], 300.0, 500.0),  // stale zero memory: a restore would jump to top
            ([50.0, 75.0], 0.0, 0.0),    // and nothing leaks between the slots
        ] {
            let (out_mem, out_live) =
                scroll_exchange(mem, live, Palettes, Palettes, max);
            assert_eq!(out_live, live, "the live offset is untouched");
            assert_eq!(out_mem, mem, "and the memory is untouched");
            let (out_mem, out_live) =
                scroll_exchange(mem, live, Harmonies, Harmonies, max);
            assert_eq!((out_mem, out_live), (mem, live));
        }
    }

    /// The per-tab maxima the restore clamps against: harmonies derives to ZERO today
    /// (the spacing round made it fill its viewport) and the palettes' is the real
    /// overflow, so the memory is per-tab machinery rather than a palettes special case.
    #[test]
    fn the_maxima_are_per_tab() {
        assert_eq!(harmonies_max_scroll(), 0.0, "harmonies fills its viewport exactly");
        let h = color_window_size().1;
        assert_eq!(panel_max_scroll_for(Harmonies, h, 20), 0.0);
        assert_eq!(panel_max_scroll_for(Palettes, h, 20), palettes_max_scroll(h, 20));
        assert!(panel_max_scroll_for(Palettes, h, 20) > 0.0);
    }
}

/// DRAGON-687 (the owner's addendum): the drag auto-scroll's velocity ramp.
#[cfg(test)]
mod autoscroll_tests {
    use super::*;

    fn shape() -> PanelShape {
        PanelShape { palettes: true, scroll: 0.0, groups: vec![1; 20] }
    }
    fn panel_x() -> f32 {
        WINDOW_BORDER + picker_column_w() + panel_w() / 2.0
    }
    fn column_mid() -> f32 {
        WINDOW_BORDER + picker_column_w() / 2.0
    }

    /// The two bands, their signs, and the ramp: zero at the band's inner edge, the full
    /// (slow) speed at the viewport's own edge, monotone in between.
    #[test]
    fn the_bands_ramp_toward_the_edges() {
        let w = color_window_size_expanded();
        let sh = shape();
        let (top, bottom) = (palettes_scroll_top(), panel_scroll_bottom(w.1));
        // Dead middle: no scrolling.
        let mid = (top + bottom) / 2.0;
        assert_eq!(drag_autoscroll_velocity((panel_x(), mid), w, &sh), 0.0);
        // The top band scrolls UP (negative), harder nearer the edge.
        let shallow = drag_autoscroll_velocity((panel_x(), top + AUTOSCROLL_BAND - 1.0), w, &sh);
        let deep = drag_autoscroll_velocity((panel_x(), top + 1.0), w, &sh);
        assert!(shallow < 0.0 && deep < 0.0);
        assert!(deep < shallow, "closer to the edge scrolls faster: {deep} vs {shallow}");
        assert!(
            drag_autoscroll_velocity((panel_x(), top), w, &sh) <= -AUTOSCROLL_MAX_SPEED + 0.01,
            "the viewport's edge is full speed"
        );
        // The bottom band scrolls DOWN (positive), same ramp mirrored.
        let low = drag_autoscroll_velocity((panel_x(), bottom - 1.0), w, &sh);
        assert!(low > AUTOSCROLL_MAX_SPEED * 0.9);
        // The band's inner edges are exactly zero, so grazing them cannot jerk.
        assert_eq!(drag_autoscroll_velocity((panel_x(), top + AUTOSCROLL_BAND), w, &sh), 0.0);
    }

    /// THE drag-start contract's band half (the owner's bug): a drag that BEGINS inside
    /// a band must not scroll — the topmost visible title sits wholly inside the top
    /// band, so grabbing it to reorder used to scroll to the top under a stationary
    /// pointer. The auto-scroll arms only once a live sample lands where the ramp is
    /// zero, and stays armed after.
    #[test]
    fn a_drag_born_in_a_band_scrolls_nothing_until_it_leaves() {
        let w = color_window_size_expanded();
        let sh = shape();
        let top = palettes_scroll_top();
        // The first group's TITLE, the owner's grab point: wholly inside the top band.
        let title = (panel_x(), top + PALETTE_TITLE_ROW_H / 2.0);
        assert!(
            drag_autoscroll_velocity(title, w, &sh) != 0.0,
            "the premise: the first title really is inside the band"
        );
        // Born there: not armed, sample after sample, however long it sits.
        let mut armed = false;
        for _ in 0..3 {
            armed = autoscroll_arms(armed, title, w, &sh);
            assert!(!armed, "a drag born in the band must not arm in it");
        }
        // One sample in the dead middle arms it...
        let mid = (panel_x(), (top + panel_scroll_bottom(w.1)) / 2.0);
        armed = autoscroll_arms(armed, mid, w, &sh);
        assert!(armed);
        // ...and it STAYS armed back inside the band, which is what lets the sanctioned
        // gesture (moving a drag TO the edge) scroll.
        armed = autoscroll_arms(armed, title, w, &sh);
        assert!(armed);
        // A drag born on the PICKER COLUMN (a recents, harmony or active-swatch drag)
        // arms immediately: it begins where the ramp is zero.
        let column = (WINDOW_BORDER + picker_column_w() / 2.0, title.1);
        assert!(autoscroll_arms(false, column, w, &sh));
    }

    /// Where it must NOT engage: the picker column, the chrome above the scroll area,
    /// past the frame, and any state that is not the palettes tab. Ordinary hovering
    /// never reaches this function at all (the caller gates on a LIVE drag).
    #[test]
    fn nothing_scrolls_outside_the_palettes_scroll_area() {
        let w = color_window_size_expanded();
        let sh = shape();
        let top = palettes_scroll_top();
        // The picker column's own top edge is inside the band's y range but not the
        // panel's x range.
        assert_eq!(drag_autoscroll_velocity((column_mid(), top + 1.0), w, &sh), 0.0);
        // The tab strip and create row above the viewport.
        assert_eq!(drag_autoscroll_velocity((panel_x(), top - 1.0), w, &sh), 0.0);
        // Past the window's bottom edge.
        assert_eq!(drag_autoscroll_velocity((panel_x(), w.1), w, &sh), 0.0);
        // The Harmonies tab (or a collapsed panel): the shape says no palettes.
        assert_eq!(
            drag_autoscroll_velocity((panel_x(), top + 1.0), w, &PanelShape::default()),
            0.0
        );
    }
}

/// DRAGON-687 (the owner's second addendum): Ctrl+Tab cycling over the panel's tabs.
#[cfg(test)]
mod panel_tab_cycle_tests {
    use super::*;

    /// Forward and backward both wrap, over the two tabs that exist today, and the walk
    /// is `keynav::step`'s so a third tab joins it for free.
    #[test]
    fn the_cycle_wraps_both_ways() {
        assert_eq!(
            panel_tab_after_cycle(PanelTab::Harmonies, true, true),
            Some(PanelTab::Palettes)
        );
        assert_eq!(
            panel_tab_after_cycle(PanelTab::Palettes, true, true),
            Some(PanelTab::Harmonies),
            "forward off the end wraps"
        );
        assert_eq!(
            panel_tab_after_cycle(PanelTab::Harmonies, false, true),
            Some(PanelTab::Palettes),
            "backward off the start wraps"
        );
        assert_eq!(
            panel_tab_after_cycle(PanelTab::Palettes, false, true),
            Some(PanelTab::Harmonies)
        );
    }

    /// With the panel collapsed the chord does NOTHING: a tab switch nobody can see is
    /// state moving behind the user's back.
    #[test]
    fn a_collapsed_panel_cycles_nothing() {
        for tab in PanelTab::ALL {
            for forward in [true, false] {
                assert_eq!(panel_tab_after_cycle(tab, forward, false), None);
            }
        }
    }
}

/// DRAGON-687: the palette menus' gates, targets and panel arithmetic.
#[cfg(test)]
mod palette_menu_tests {
    use super::*;

    /// THE page invariant (the owner's stale-submenu report): a menu OPENS on its root,
    /// whatever page a prior menu was closed on and however it was closed. Exhaustive
    /// over the page vocabulary, so a page added later is covered the day it exists.
    #[test]
    fn every_menu_open_starts_at_the_root() {
        for prior in [
            MenuPage::Root,
            MenuPage::AddTo,
            MenuPage::MoveTo,
            MenuPage::CopyTo,
        ] {
            assert_eq!(menu_page_on_open(prior), MenuPage::Root, "left on {prior:?}");
        }
    }

    /// The owner's two gates, exactly: ANY palette offers "Add to palette ›" on the
    /// harmony and recents swatches, and MORE THAN ONE offers move/copy on the palette
    /// swatches.
    #[test]
    fn the_submenu_gates_are_the_owners() {
        assert!(!offers_add_to_palette(0));
        assert!(offers_add_to_palette(1));
        assert!(offers_add_to_palette(5));
        assert!(!offers_move_copy_to_palette(0));
        assert!(!offers_move_copy_to_palette(1));
        assert!(offers_move_copy_to_palette(2));
    }

    /// The move/copy pages list every group but the colour's own, in display order.
    #[test]
    fn the_target_list_excludes_the_colours_own_group() {
        assert_eq!(palette_targets(3, Some(1)), vec![0, 2]);
        assert_eq!(palette_targets(3, None), vec![0, 1, 2]);
        assert_eq!(palette_targets(1, Some(0)), Vec::<usize>::new());
    }

    /// The generalised panel height is the fixed menus' own arithmetic, so the flyout
    /// offsets cannot drift between the constant menus and the data-sized ones.
    #[test]
    fn the_generalised_height_matches_the_fixed_menus() {
        assert!((menu_panel_h_for(7) - mode_menu_panel_h()).abs() < 0.01);
        assert!((menu_panel_h_for(3) - harmony_menu_panel_h()).abs() < 0.01);
        assert!((menu_panel_h_for(2) - recents_menu_panel_h()).abs() < 0.01);
    }

    /// A menu sized from user text CAPS at the panel's content width, so an absurd
    /// palette name cannot push its own menu off the window.
    #[test]
    fn a_menu_of_names_caps_at_the_panel() {
        let absurd = "x".repeat(400);
        let w = menu_width_for_labels([absurd.as_str()].into_iter());
        assert!(w <= panel_content_w() + 0.01, "{w} must cap at {}", panel_content_w());
        let short = menu_width_for_labels(["ab"].into_iter());
        assert!(short < w);
    }

    /// The sort submenu's LANGUAGE (the owner's intuitive-language follow-up): every
    /// label states the resulting order in plain words, the entries run in pairs with the
    /// most-used sort first, and no label carries an em or en dash.
    #[test]
    fn the_sort_labels_say_the_outcome_in_pairs() {
        let labels: Vec<&str> = PaletteSort::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels,
            vec![
                "Name (A to Z)",
                "Name (Z to A)",
                "Lightest first",
                "Darkest first",
                "Warmest first",
                "Coolest first",
            ],
            "pairs together: name, then brightness, then temperature"
        );
        for l in &labels {
            assert!(!l.contains('\u{2014}') && !l.contains('\u{2013}'), "{l}: no dashes");
        }
        // The words still mean the formulas they always did: "Darkest first" IS the
        // ascending-luminance sort and "Coolest first" the ascending-warmth one, so the
        // rewording moved no maths.
        assert_eq!(PaletteSort::Luminance.label(), "Darkest first");
        assert_eq!(PaletteSort::CoolToWarm.label(), "Coolest first");
        assert_eq!(SORT_GROUPS_LABEL, "Sort palettes");
    }
}

/// DRAGON-687 follow-up: a page-swapping menu re-fits itself to the window from the
/// CURRENT page's size. The four edges the owner's report is about, pinned.
#[cfg(test)]
mod menu_fit_tests {
    use super::*;

    fn window() -> (f32, f32) {
        color_window_size_expanded()
    }

    /// Plenty of room: byte-identical to the historical upward placement (bottom flush
    /// with the anchor's top, the caller's own horizontal wish honoured).
    #[test]
    fn a_fitting_page_keeps_the_historical_up_placement() {
        let anchor = (500.0, 400.0);
        let panel = (120.0, 90.0);
        let (dx, dy) = menu_fit(anchor, PANEL_SWATCH, 480.0, panel, window());
        assert_eq!(dy, -panel.1, "up, bottom flush with the anchor top");
        assert_eq!(dx, 480.0 - anchor.0, "the column rule's own left edge");
    }

    /// Near the TOP (a first group's heading, a first harmony bar): the page flips DOWN
    /// instead of clipping above the frame.
    #[test]
    fn a_page_near_the_top_flips_down() {
        let anchor = (500.0, 60.0);
        let panel = (120.0, 190.0);
        let (dx, dy) = menu_fit(anchor, PANEL_HEADING_H, 500.0, panel, window());
        assert_eq!(dy, PANEL_HEADING_H, "top flush with the anchor bottom");
        let top = anchor.1 + dy;
        assert!(top >= WINDOW_BORDER, "inside the frame");
        assert!(top + panel.1 <= window().1 - WINDOW_BORDER, "and inside the bottom");
        let _ = dx;
    }

    /// Near the BOTTOM with no room above either (a tall target list from a low anchor):
    /// the page SLIDES into the window rather than clipping either edge.
    #[test]
    fn a_page_too_tall_for_both_directions_slides_inside() {
        let w = window();
        let anchor = (500.0, w.1 - 40.0);
        let panel = (120.0, w.1 - 60.0);
        let (_, dy) = menu_fit(anchor, RECENT_SWATCH, 500.0, panel, w);
        let top = anchor.1 + dy;
        assert!(top >= WINDOW_BORDER + MENU_FIT_MARGIN - 0.01, "not off the top: {top}");
        assert!(
            top + panel.1 <= w.1 - WINDOW_BORDER - MENU_FIT_MARGIN + 0.01,
            "not off the bottom"
        );
        // A page taller than the whole band pins at the top, so its head (and the pointer
        // path to the rest) stays reachable.
        let (_, dy) = menu_fit(anchor, RECENT_SWATCH, 500.0, (120.0, w.1 + 50.0), w);
        assert_eq!(anchor.1 + dy, WINDOW_BORDER + MENU_FIT_MARGIN);
    }

    /// Near the RIGHT edge: the final window clamp slides the panel left of the frame
    /// whatever the column rule wished for.
    #[test]
    fn a_page_near_the_right_edge_slides_left() {
        let w = window();
        let anchor = (w.0 - 30.0, 300.0);
        let panel = (200.0, 90.0);
        let (dx, _) = menu_fit(anchor, PANEL_SWATCH, anchor.0, panel, w);
        let left = anchor.0 + dx;
        assert!(left + panel.0 <= w.0 - WINDOW_BORDER - MENU_FIT_MARGIN + 0.01);
        assert!(left >= WINDOW_BORDER + MENU_FIT_MARGIN - 0.01);
    }

    /// The anchors the fit reads are the layout's own numbers, through the scroll.
    #[test]
    fn the_anchors_track_the_scroll() {
        let a0 = palette_heading_anchor(0, 0.0);
        assert_eq!(a0.1, palettes_scroll_top());
        let scrolled = palette_heading_anchor(0, 25.0);
        assert_eq!(scrolled.1, palettes_scroll_top() - 25.0);
        let bar = palette_swatch_anchor(0, 0, 3, 0.0);
        assert_eq!(bar.1, a0.1 + PALETTE_TITLE_ROW_H + PANEL_HEADING_GAP);
        let h = harmony_swatch_anchor(1, 0, 5, 0.0);
        assert!(h.1 > 0.0);
        // The history's first swatch sits under the divider band, at the content's left.
        let r = history_swatch_anchor(0);
        assert_eq!(r.0, WINDOW_BORDER + WINDOW_PADDING);
        assert_eq!(r.1, divider_band_top() + DIVIDER_BAND_H + SECTION_GAP);
        // ...and the second row is one swatch and one grid gap further down.
        let r9 = history_swatch_anchor(RECENTS_PER_ROW);
        assert_eq!(r9.1, r.1 + RECENT_SWATCH + recents_gap());
    }
}
