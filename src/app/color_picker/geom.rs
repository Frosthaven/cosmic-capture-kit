//! The colour picker's pure decisions (DRAGON-582): which SOURCE PIXEL the cursor is
//! over, what the magnifier disc looks like, where the hex label goes, how big the
//! result window is, and what may write the recent-colours list.
//!
//! Everything here is a plain function over plain data. No `App`, no iced widget, no
//! platform: the picker's correctness lives in this file and the Linux gate proves all
//! of it on any host. The overlay and the window only feed these numbers in and apply
//! the answers.

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
// The owner's brief was "half the size of mac's permission window", refined on review to
// "half width but not strict, use best judgement". So the WIDTH starts from half of the
// permission window's 629pt (= 314.5) and is then checked against what a row actually
// needs; the HEIGHT is simply the sum of the window's parts. Nothing here scrolls: a
// scrollbar would be a way of not answering the sizing question.

/// Padding inside the window, per side.
pub const WINDOW_PADDING: f32 = 16.0;
/// The notation label column ("OKLCH" is the widest).
pub const ROW_LABEL_W: f32 = 52.0;
/// The editable value box.
///
/// Sized for the LONGEST value a row can hold plus room to type: `oklch(75.6% 0.176
/// 60.7)` is 23 characters, a hand-typed value can reasonably run to 27 with the caret,
/// and 8pt per character at the body text size gives 216pt, plus the text input's own
/// ~24pt of horizontal padding. That arithmetic asks for 240.
///
/// It is 262 because of the RECENTS ROW (DRAGON-587). That row is now ten swatches plus the
/// pick-again pipette, which needs more width than a value row does, and the window is a
/// single fixed width for both. So the recents row sets the width and the value box takes the
/// surplus rather than leaving a ragged gap at the end of every row; the rows stay flush and
/// the OKLCH one gets a couple more characters of typing room. See [`color_window_size`].
pub const ROW_INPUT_W: f32 = 262.0;
/// The per-row copy button (a 16pt icon in a standard icon button).
pub const ROW_COPY_W: f32 = 32.0;
/// Gap between a row's label, input and copy button.
pub const ROW_SPACING: f32 = 8.0;
/// One value row's height.
pub const ROW_H: f32 = 34.0;
/// Gap between value rows.
pub const ROW_GAP: f32 = 6.0;
/// The full-width swatch at the top.
pub const SWATCH_H: f32 = 72.0;
/// Gap between the window's sections (swatch, rows, recents).
pub const SECTION_GAP: f32 = 12.0;
/// One recent-colour swatch.
pub const RECENT_SWATCH: f32 = 28.0;
/// Gap between recent swatches.
pub const RECENT_GAP: f32 = 6.0;
/// The pick-again pipette that shares the recents row, right-aligned (DRAGON-587).
///
/// Square, and exactly a swatch tall, so the bottom row's height is still [`RECENT_SWATCH`]
/// and the window's height arithmetic is unchanged by adding it.
pub const PICK_AGAIN_W: f32 = RECENT_SWATCH;

/// How many recent colours the row holds. Beyond this the OLDEST is dropped.
///
/// Ten, and it still fits now that the pipette shares the row: `10 * 28 + 9 * 6 = 334pt` of
/// swatches, plus [`ROW_SPACING`] and the [`PICK_AGAIN_W`] pipette, is the 370pt of content
/// the window is now sized FROM (see [`color_window_size`]). So the row never wraps and never
/// scrolls at a full ten. It is also about as many as anyone can pick out by eye.
pub const RECENTS_CAP: usize = 10;
/// The client-side header bar's height (the same chrome the settings and preview windows
/// draw), which the content has to sit below.
pub const HEADER_H: f32 = 44.0;
/// A little slack on the width, so the widest row is not flush against the padding.
pub const WINDOW_SLACK: f32 = 8.0;

/// Pure, unit-tested: the colour-picker window's size in LOGICAL POINTS. Its ONLY size.
///
/// **This is no longer a spawn size with a floor under it: since DRAGON-587 the window is
/// exactly this and cannot be resized** (`min_size == max_size`, see
/// [`super::open_color_picker_window`]). The arithmetic below is kept because it is still
/// what the number MEANS, and it is now a constraint rather than a starting point: whatever
/// the widest row needs, the window is, and there is no user resize to absorb a mistake.
///
/// **Width**, spelled out: `2 * 16` padding + `52` label + `8` + `262` input + `8` +
/// `32` copy button + `8` slack = **402pt**.
///
/// That is wider than the ~315 half-of-the-permission-window the ticket started from,
/// and deliberately so. At 315 the value box would get about 175pt, which cannot hold a
/// typed `oklch(75.6% 0.176 60.7)` without truncating it under the caret, and the owner
/// asked for judgement rather than the number. Growing the width is the honest answer;
/// the alternatives were a squeezed row or a scrollbar, and both hide the problem.
///
/// It grew again, from 380 to 402, when the pick-again pipette joined the RECENTS ROW
/// (DRAGON-587). Two rows now compete for one fixed width, and the recents row is the wider:
/// ten swatches (`10 * 28 + 9 * 6 = 334`) plus `8` plus the `28` pipette is **370pt** of
/// content, where a value row wanted 348. Since the window can no longer be resized, that is
/// a hard constraint rather than something a user could work around, so the window is sized
/// to the wider row and [`ROW_INPUT_W`] absorbs the surplus, keeping every row flush.
///
/// **Height** is the sum of the parts, and is UNCHANGED by the pipette: it is a swatch tall,
/// so the bottom row is still [`RECENT_SWATCH`] high. Header + padding + swatch + gap + the
/// seven value rows + gap + the recents row + padding.
pub fn color_window_size() -> (f32, f32) {
    let rows = crate::color::ColorFormat::ALL.len() as f32;
    let w = 2.0 * WINDOW_PADDING
        + ROW_LABEL_W
        + ROW_SPACING
        + ROW_INPUT_W
        + ROW_SPACING
        + ROW_COPY_W
        + WINDOW_SLACK;
    let h = HEADER_H
        + 2.0 * WINDOW_PADDING
        + SWATCH_H
        + SECTION_GAP
        + rows * ROW_H
        + (rows - 1.0) * ROW_GAP
        + SECTION_GAP
        + RECENT_SWATCH;
    (w, h)
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
pub fn writes_recents(source: ColorSource) -> bool {
    matches!(source, ColorSource::Pick)
}

/// Pure, unit-tested: `list` with `color` pushed to the FRONT, de-duplicated, capped.
///
/// An exact duplicate MOVES its existing entry to the front rather than adding a second
/// copy: two identical swatches carry no information, and the conventional behaviour
/// everywhere else that keeps a recents list is to promote. The oldest entry falls off
/// the end at [`RECENTS_CAP`].
///
/// A caller must still ask [`writes_recents`] first; this function is the WHAT, that one
/// is the WHETHER.
pub fn push_recent(list: &[Srgb], color: Srgb, cap: usize) -> Vec<Srgb> {
    let mut out = Vec::with_capacity(list.len() + 1);
    out.push(color);
    out.extend(list.iter().copied().filter(|c| *c != color));
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

#[cfg(test)]
mod window_size_tests {
    use super::*;

    /// The permission window's width (`app::permissions::open_permissions_window`), the
    /// reference the owner named. A test-only constant: production code derives the
    /// picker's width from its own rows, and the only thing this is for is checking that
    /// the result sits where the brief said it should.
    const PERMISSIONS_WINDOW_W: f32 = 629.0;

    /// The width is the ROW's own arithmetic, and it is wider than the ~315 the ticket
    /// started from on purpose (see the function's doc). Pinned so a later tweak to any
    /// column has to be a deliberate one.
    #[test]
    fn the_width_is_what_a_row_actually_needs() {
        let (w, _) = color_window_size();
        assert_eq!(w, 402.0);
        let row = ROW_LABEL_W + ROW_SPACING + ROW_INPUT_W + ROW_SPACING + ROW_COPY_W;
        assert_eq!(w, 2.0 * WINDOW_PADDING + row + WINDOW_SLACK);
        assert!(w > PERMISSIONS_WINDOW_W / 2.0, "honest layout maths grew it past the start point");
        assert!(w < PERMISSIONS_WINDOW_W, "and it is still smaller than the window it derives from");
    }

    /// DRAGON-587: the RECENTS row is what the width is really sized from now, because the
    /// pipette shares it. The two rows must come out to the same content width, or one of
    /// them is either clipped or trailing empty space in a window nobody can resize.
    #[test]
    fn both_rows_want_exactly_the_content_width() {
        let (w, _) = color_window_size();
        let content = w - 2.0 * WINDOW_PADDING;
        let strip = RECENTS_CAP as f32 * RECENT_SWATCH + (RECENTS_CAP as f32 - 1.0) * RECENT_GAP;
        assert_eq!(strip + ROW_SPACING + PICK_AGAIN_W, content, "the recents row fits exactly");
        let value_row =
            ROW_LABEL_W + ROW_SPACING + ROW_INPUT_W + ROW_SPACING + ROW_COPY_W + WINDOW_SLACK;
        assert_eq!(value_row, content, "and so does a value row");
    }

    /// The height is the sum of its parts, so the seven rows and the recents strip fit
    /// with no scrolling. Recomputed here from the same constants, which is what would
    /// catch a part being dropped from the sum.
    #[test]
    fn the_height_is_the_sum_of_the_parts() {
        let (_, h) = color_window_size();
        let rows = crate::color::ColorFormat::ALL.len() as f32;
        let want = HEADER_H
            + 2.0 * WINDOW_PADDING
            + SWATCH_H
            + SECTION_GAP
            + rows * ROW_H
            + (rows - 1.0) * ROW_GAP
            + SECTION_GAP
            + RECENT_SWATCH;
        assert_eq!(h, want);
        assert_eq!(h, 474.0);
    }

    /// The recents row FITS the content width at the cap, WITH the pipette beside it: no
    /// wrapping, no horizontal scrolling, and the pipette never pushed off the edge.
    #[test]
    fn the_recents_row_fits_at_the_cap() {
        let (w, _) = color_window_size();
        let content = w - 2.0 * WINDOW_PADDING;
        let strip = RECENTS_CAP as f32 * RECENT_SWATCH + (RECENTS_CAP as f32 - 1.0) * RECENT_GAP;
        assert!(strip + ROW_SPACING + PICK_AGAIN_W <= content, "{strip} + the pipette in {content}");
        // And one more swatch would NOT fit, so the cap is the real limit rather than an
        // arbitrary round number.
        let one_more = strip + RECENT_GAP + RECENT_SWATCH + ROW_SPACING + PICK_AGAIN_W;
        assert!(one_more > content, "the cap is what the width allows");
    }

    /// The cap is TEN, and the eleventh pick drops the oldest. The owner asked to have this
    /// confirmed rather than changed, so it is pinned here as a number rather than left to
    /// the recents tests' use of the constant.
    #[test]
    fn the_cap_is_ten_and_the_eleventh_pick_drops_the_oldest() {
        assert_eq!(RECENTS_CAP, 10);
        let ten: Vec<Srgb> = (0..10u8).map(|i| Srgb::new(i, 0, 0)).collect();
        let after = push_recent(&ten, Srgb::new(200, 0, 0), RECENTS_CAP);
        assert_eq!(after.len(), 10, "still ten");
        assert_eq!(after[0], Srgb::new(200, 0, 0), "the newest leads");
        assert_eq!(after.last(), Some(&Srgb::new(8, 0, 0)), "the list ends one earlier");
        assert!(!after.contains(&Srgb::new(9, 0, 0)), "and the oldest fell off entirely");
    }
}

#[cfg(test)]
mod recents_tests {
    use super::*;

    fn c(r: u8) -> Srgb {
        Srgb::new(r, 0, 0)
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
        let full: Vec<Srgb> = (0..RECENTS_CAP as u8).map(c).collect();
        let after = push_recent(&full, c(200), RECENTS_CAP);
        assert_eq!(after.len(), RECENTS_CAP);
        assert_eq!(after[0], c(200));
        assert_eq!(after.last(), Some(&c(RECENTS_CAP as u8 - 2)), "the oldest fell off");
        assert_eq!(push_recent(&full, c(200), 0), vec![c(200)], "a zero cap still keeps one");
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
