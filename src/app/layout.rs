//! Overlay / toolbar geometry: placement helpers and shared sizing constants.
//!
//! All constants here are the single source of truth for toolbar button/group
//! footprints; `toolbar_layout` (which sizes the placement box and click-through
//! input zone) and `capture_button_layer` (which builds the widgets) both derive
//! from these, so the input zone can never drift from what's drawn.

use cosmic::iced::{Background, Length};
use super::Msg;
use super::OutputState;

// ── Toolbar geometry constants ────────────────────────────────────────────────

/// Gap (logical px) between the toolbar and the selection it's placed beside.
pub(crate) const BADGE_GAP: f32 = 8.0;

/// Icon glyph box inside a toolbar button.
pub(crate) const ICON_BOX: f32 = 22.0;
/// A button's padding around its icon.
pub(crate) const BTN_PAD: f32 = 8.0;
/// A group container's padding around its button row.
pub(crate) const GROUP_PAD: f32 = 3.0;
/// Outer height of a standard group (an icon button in a padded group).
pub(crate) const GROUP_H_BASE: f32 = ICON_BOX + 2.0 * BTN_PAD + 2.0 * GROUP_PAD;

// ── Audio meter ───────────────────────────────────────────────────────────────

/// The meter's red line: the mic test's "too loud" threshold (-6 dBFS on the shared
/// `(dBFS+60)/60` meter scale). Below it the fill is green; at or above it the whole
/// fill turns red — same semantics as the mic-test waveform, no yellow in between.
pub(crate) const METER_RED_ZONE: f32 = 0.90;

/// Volume-meter fill for an armed audio button: a single-hue FADE anchored at the
/// BOTTOM and rising to `level` (0..1), transparent above. Green while the level is
/// healthy; the fill turns red only when the level crosses the mic test's red zone
/// (`METER_RED_ZONE`). Half-transparent so the icon on top stays readable. Empty
/// when silent.
pub(crate) fn meter_background(level: f32) -> Background {
    use cosmic::iced::{gradient::Linear, Color, Radians};
    // Canonical semantic palette (shared with the mic test + status captions).
    let l = level.clamp(0.0, 1.0);
    let hue = if l >= METER_RED_ZONE {
        crate::app::theme::DANGER
    } else {
        crate::app::theme::SUCCESS
    };
    let clear = Color::from_rgba(0.0, 0.0, 0.0, 0.0);
    // Angle 0 points up, so stop 0.0 is at the BOTTOM (where the fill starts) and
    // the bar rises toward the top.
    let mut g = Linear::new(Radians(0.0));
    if l <= 0.02 {
        // Effectively silent — no visible fill.
        return Background::from(g.add_stop(0.0, clear).add_stop(1.0, clear));
    }
    // Fade: half-alpha at the bottom easing out toward the fill's leading edge, so
    // the level reads as a rising tint under the icon rather than a colour block.
    let a = |alpha: f32| Color { a: alpha, ..hue };
    g = g.add_stop(0.0, a(0.5));
    g = g.add_stop(l, a(0.18));
    if l < 0.999 {
        // Cut to transparent just past the fill's leading edge.
        g = g.add_stop((l + 0.02).min(0.9999), clear).add_stop(1.0, clear);
    }
    Background::from(g)
}

// ── Overlay positioning helpers ───────────────────────────────────────────────

/// Place `content` at local (`lx`,`ly`) on an output overlay by padding a fill
/// container (the same trick the toolbar uses). For the detection-mark outlines +
/// tooltips, which sit at specific positions over the region.
pub(crate) fn positioned_mark<'a>(lx: f32, ly: f32, content: super::Element<'a, Msg>) -> super::Element<'a, Msg> {
    cosmic::widget::container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(cosmic::iced::Alignment::Start)
        .align_y(cosmic::iced::Alignment::Start)
        .padding(cosmic::iced::Padding {
            top: ly,
            left: lx,
            right: 0.0,
            bottom: 0.0,
        })
        .into()
}

/// Width (logical px) of the selection's accent outline — matches the line drawn
/// by `RegionSelection`. A region capture is inset by this so only what's visibly
/// inside the outline is grabbed.
pub(crate) const SELECTION_LINE_W: i32 = 2;

/// Inset a region selection by the outline width so a capture grabs only what's
/// visibly inside the line. Window/monitor selections are returned unchanged (they
/// capture the full target, with the outline drawn outside it).
///
/// DRAGON-418: the inset is applied PER AXIS and never past the point where that axis
/// would VANISH. The flat `width - 2 * SELECTION_LINE_W` drove any selection 4 logical
/// points or thinner straight to ZERO — and `normalized_region` (the one gate every
/// region commit passes through) admits anything from 1 point up, so a small but
/// perfectly legitimate drag committed a zero-extent capture. Nothing downstream can
/// survive that: a zero-extent rect intersects no display, so ALL FOUR region branches
/// in `do_pixel_capture` come back empty — `crop_frozen` and the live `region()` both
/// bottom out in `stitch_region`, whose `parts.is_empty()` guard returns `None`, and the
/// two `region_windows*` branches composite nothing. `None` there means
/// "native pixel capture returned no image": a `log::warn!` to a stderr no packaged app
/// has, then `finish_session()`. The user gets an instant self-close with no file, no
/// preview and no message.
///
/// A capture the user legitimately asked for must never be able to produce nothing, so
/// keep at least one point on each axis. Selections 5 points and wider are UNAFFECTED
/// (`(extent - 1) / 2 >= 2` there, so the inset is the full `SELECTION_LINE_W` exactly as
/// before) — every real capture is byte-identical, and only the range that was
/// guaranteed to fail behaves differently.
pub(crate) fn inset_region(sel: super::Selection) -> super::Selection {
    use super::Selection;
    if sel.window_id.is_some() || sel.output.is_some() {
        return sel;
    }
    let ix = axis_inset(sel.width);
    let iy = axis_inset(sel.height);
    Selection {
        x: sel.x + ix,
        y: sel.y + iy,
        width: sel.width.saturating_sub((2 * ix) as u32),
        height: sel.height.saturating_sub((2 * iy) as u32),
        ..sel
    }
}

/// The per-side inset for one axis of `extent` logical points: the full outline width
/// when the axis can spare it, otherwise the most that still leaves a point standing.
/// Split out so the "never empties an axis" invariant is unit-testable on its own.
fn axis_inset(extent: u32) -> i32 {
    SELECTION_LINE_W.min(((extent as i32) - 1) / 2).max(0)
}

/// Where to place an action box (`bw`×`bh`) for the selection on output `o`, in
/// that output's surface-local coords. Prefers just *outside* the selection —
/// bottom-centred, then top-centred (both **horizontal**), then left/right
/// (**vertical**) — falling back inside-top-right (horizontal) when nothing
/// outside fits (e.g. a whole-monitor selection). Returns `(rect, horizontal)`,
/// or `None` when the selection doesn't touch this output.
/// Place a control box just outside the region, preferring below/above (where it
/// lays out *horizontally* with dims `hbw`×`hbh`) and falling back to left/right
/// (where it lays out *vertically* with dims `vbw`×`vbh`). The returned bool is
/// true for a horizontal placement. The box is centred on the region along its
/// free axis and clamped on-screen.
///
/// **Units (DRAGON-448)**: `sx`/`sy`/`sw`/`sh` are the selection in CAPTURE space (what
/// `normalized_region` and every `Selection` carry); the box dims and the returned
/// rectangle are POINTS, because the rectangle becomes iced `Padding` and an input zone.
/// The crossing happens once, here, through the output's [`OverlayUnits`] — nothing above
/// or below this function scales anything.
#[allow(clippy::too_many_arguments)]
// OutputState is private to app; placement is pub(crate) only to satisfy the
// re-export in mod.rs; callers are always within app.
#[allow(private_interfaces)]
pub(crate) fn placement(
    o: &OutputState,
    sx: i32,
    sy: i32,
    sw: u32,
    sh: u32,
    hbw: f32,
    hbh: f32,
    vbw: f32,
    vbh: f32,
) -> Option<(cosmic::iced::Rectangle, bool)> {
    placement_in(o.units(), o.point_size(), (sx, sy, sw, sh), hbw, hbh, vbw, vbh)
}

/// [`placement`]'s pure body: the same math with the output expressed as its units bridge
/// plus its POINT extent, so the placement rules can be unit-tested without an
/// `OutputState` (which owns a live surface id and an output handle). Split out for
/// DRAGON-448; the arithmetic below is unchanged, only its inputs are now converted.
#[allow(clippy::too_many_arguments)]
fn placement_in(
    units: crate::geometry::OverlayUnits,
    out_pt: (f32, f32),
    sel: (i32, i32, u32, u32),
    hbw: f32,
    hbh: f32,
    vbw: f32,
    vbh: f32,
) -> Option<(cosmic::iced::Rectangle, bool)> {
    let (sx, sy, sw, sh) = sel;
    let (ow, oh) = out_pt;
    // The selection is CAPTURE space; the box we are placing is POINTS. Cross once.
    let (lx, ly) = units.to_point((sx, sy));
    let lw = units.len_to_point(sw as f32);
    let lh = units.len_to_point(sh as f32);
    if lx + lw <= 0.0 || ly + lh <= 0.0 || lx >= ow || ly >= oh {
        return None;
    }
    let cx = (lx + (lw - hbw) / 2.0).clamp(0.0, (ow - hbw).max(0.0));
    let cy = (ly + (lh - vbh) / 2.0).clamp(0.0, (oh - vbh).max(0.0));
    let below = ly + lh + BADGE_GAP;
    let above = ly - BADGE_GAP - hbh;
    let left = lx - BADGE_GAP - vbw;
    let right = lx + lw + BADGE_GAP;
    // Prefer below, then above; only when neither horizontal slot fits do we go to
    // a side — left or right, whichever has more clear space. Last resort: inside
    // the top-right corner (selection fills the output).
    let left_room = lx;
    let right_room = ow - (lx + lw);
    let (x, y, bw, bh, horizontal) = if below + hbh <= oh {
        (cx, below, hbw, hbh, true)
    } else if above >= 0.0 {
        (cx, above, hbw, hbh, true)
    } else {
        let left_fits = left >= 0.0;
        let right_fits = right + vbw <= ow;
        match (left_fits, right_fits) {
            (true, true) if left_room >= right_room => (left, cy, vbw, vbh, false),
            (true, true) => (right, cy, vbw, vbh, false),
            (true, false) => (left, cy, vbw, vbh, false),
            (false, true) => (right, cy, vbw, vbh, false),
            (false, false) => (
                (lx + lw - hbw - BADGE_GAP).clamp(0.0, (ow - hbw).max(0.0)),
                (ly + BADGE_GAP).clamp(0.0, (oh - hbh).max(0.0)),
                hbw,
                hbh,
                true,
            ),
        }
    };
    Some((cosmic::iced::Rectangle { x, y, width: bw, height: bh }, horizontal))
}

#[cfg(test)]
mod placement_tests {
    use super::*;
    use crate::geometry::OverlayUnits;

    /// A toolbar-shaped box: the real horizontal footprint of the non-active toolbar
    /// (`W_KIND + W_MODE + W_UTIL` plus gaps ≈ 420) and its stacked twin.
    const H: (f32, f32) = (420.0, 44.0);
    const V: (f32, f32) = (148.0, 140.0);

    /// One output: its units bridge plus its POINT extent, the two things `placement_in`
    /// takes. `px` is the monitor's CAPTURE size.
    fn out(origin: (i32, i32), px: (u32, u32), factor: f32) -> (OverlayUnits, (f32, f32)) {
        let u = OverlayUnits::new(origin, factor);
        (u, u.size_to_point(px))
    }

    fn place(
        o: (OverlayUnits, (f32, f32)),
        sel: (i32, i32, u32, u32),
    ) -> Option<(cosmic::iced::Rectangle, bool)> {
        placement_in(o.0, o.1, sel, H.0, H.1, V.0, V.1)
    }

    /// THE regression pin: at factor 1.0 the placement is bit-for-bit what it always was —
    /// the point extent IS the capture extent and the selection needs no conversion, so
    /// every existing platform and every 96-DPI Windows box is untouched.
    #[test]
    fn factor_one_placement_is_unchanged() {
        let o = out((0, 0), (1920, 1080), 1.0);
        // A 600x400 region at (100, 200): the toolbar goes just BELOW it, centred.
        let (r, horizontal) = place(o, (100, 200, 600, 400)).unwrap();
        assert!(horizontal);
        assert_eq!(r.y, 200.0 + 400.0 + BADGE_GAP);
        assert_eq!(r.x, 100.0 + (600.0 - H.0) / 2.0);
        assert_eq!((r.width, r.height), H);
        // A whole-monitor selection has no outside room at all: the inside-top-right
        // fallback, still horizontal.
        let (r, horizontal) = place(o, (0, 0, 1920, 1080)).unwrap();
        assert!(horizontal);
        assert_eq!(r.x, 1920.0 - H.0 - BADGE_GAP);
        assert_eq!(r.y, BADGE_GAP);
    }

    /// The DRAGON-448 defect, in numbers. A 5120x2880 monitor at 150% is a 3413x1920
    /// VIEWPORT; a region drawn across its right half sits at capture x≈2560, which the old
    /// code fed straight in as a point coordinate. That is past the viewport's right edge,
    /// so iced handed the toolbar whatever few points were left and it compressed instead of
    /// moving — the owner's "started squishing instead of repositioning". Converted, the box
    /// lands inside the viewport at its NATURAL size.
    #[test]
    fn a_scaled_monitor_places_the_toolbar_inside_its_viewport_at_full_size() {
        let o = out((0, 0), (5120, 2880), 1.5);
        assert_eq!(o.1, (3413.3333, 1920.0));
        let (r, _) = place(o, (3000, 200, 2000, 800)).unwrap();
        // Full natural footprint — the box is never squeezed.
        assert_eq!((r.width, r.height), H);
        // ...and entirely inside the viewport iced actually laid out.
        assert!(r.x >= 0.0 && r.x + r.width <= o.1.0, "x {} escapes {}", r.x, o.1.0);
        assert!(r.y >= 0.0 && r.y + r.height <= o.1.1, "y {} escapes {}", r.y, o.1.1);
        // The un-converted number the old code produced was past the viewport's right edge,
        // which is what left the toolbar only a sliver of width to lay out in.
        let raw_cx = 3000.0 + (2000.0 - H.0) / 2.0;
        assert!(raw_cx > o.1.0, "the pre-fix x {raw_cx} was past the {}pt viewport", o.1.0);
        assert!(r.x + r.width <= raw_cx, "the converted x must be well left of the old one");
    }

    /// The placement never leaves the viewport, at any scale, for any selection anywhere on
    /// the monitor — the property the toolbar's reachability depends on.
    #[test]
    fn the_placed_box_never_escapes_the_viewport() {
        for factor in [1.0f32, 1.25, 1.5, 2.0, 3.0] {
            let o = out((0, 0), (3840, 2160), factor);
            let (ow, oh) = o.1;
            for x in [0i32, 500, 1920, 3000, 3839] {
                for y in [0i32, 500, 1080, 2159] {
                    for (w, h) in [(1u32, 1u32), (400, 300), (2000, 1200), (3840, 2160)] {
                        let Some((r, _)) = place(o, (x, y, w, h)) else {
                            continue;
                        };
                        assert!(r.x >= 0.0, "factor {factor} sel {x},{y} {w}x{h}: x {}", r.x);
                        assert!(r.y >= 0.0, "factor {factor} sel {x},{y} {w}x{h}: y {}", r.y);
                        // The box itself may legitimately be wider than a tiny viewport;
                        // otherwise it must fit entirely inside.
                        if r.width <= ow {
                            assert!(
                                r.x + r.width <= ow + 0.001,
                                "factor {factor} sel {x},{y} {w}x{h}: right {} > {ow}",
                                r.x + r.width
                            );
                        }
                        if r.height <= oh {
                            assert!(
                                r.y + r.height <= oh + 0.001,
                                "factor {factor} sel {x},{y} {w}x{h}: bottom {} > {oh}",
                                r.y + r.height
                            );
                        }
                    }
                }
            }
        }
    }

    /// A two-monitor desktop with DIFFERENT factors — the owner's 150% primary next to a
    /// 100% secondary — resolves EACH overlay against its own viewport. A selection wholly
    /// on one monitor places nothing on the other, and the same capture rect straddling both
    /// lands at a different point position on each.
    #[test]
    fn mixed_dpi_monitors_place_independently() {
        // Primary: 5120x2880 at 150% (3413x1920 points), at the origin.
        let hi = out((0, 0), (5120, 2880), 1.5);
        // Secondary: an 800x480 panel at 100%, abutting it at capture x=5120.
        let lo = out((5120, 0), (800, 480), 1.0);
        assert_eq!(lo.1, (800.0, 480.0));

        // A region entirely on the secondary: the primary declines it, the secondary places
        // it just below, in its own (unscaled) point space.
        let on_lo = (5200, 100, 300, 200);
        assert!(place(hi, on_lo).is_none(), "a region on the other monitor must not place");
        let (r, _) = place(lo, on_lo).unwrap();
        assert_eq!(r.y, 100.0 + 200.0 + BADGE_GAP);
        assert!(r.x >= 0.0 && r.x + r.width <= lo.1.0);

        // A region entirely on the primary: the secondary declines it.
        let on_hi = (200, 200, 600, 400);
        assert!(place(lo, on_hi).is_none());
        let (r, _) = place(hi, on_hi).unwrap();
        // Converted through 1.5: the region is at 133,133 points, 400x267 points.
        assert!((r.y - (400.0 / 1.5 + 200.0 / 1.5 + BADGE_GAP)).abs() < 0.01);

        // A region STRADDLING both gets a placement on each, each inside its own viewport —
        // and at different point coordinates, because the factors differ. Note the two
        // monitors even pick DIFFERENT SIDES: the same capture rect leaves room below it on
        // the tall 1920pt primary and none on the 480pt secondary, which puts it above
        // there. That is each output being judged on its own extent, which is the whole
        // point of a per-output factor.
        let both = (4800, 300, 800, 200);
        let (rh, _) = place(hi, both).unwrap();
        let (rl, _) = place(lo, both).unwrap();
        assert!(rh.x >= 0.0 && rh.x + rh.width <= hi.1.0 + 0.001);
        assert!(rl.x >= 0.0 && rl.x + rl.width <= lo.1.0 + 0.001);
        assert!(rh.y + rh.height <= hi.1.1 && rl.y + rl.height <= lo.1.1);
        assert_ne!(rh.y, rl.y, "one global scale would have given both the same y");
        // Primary: below the region, in ITS point space (300/1.5 + 200/1.5 + gap).
        assert!((rh.y - (300.0 / 1.5 + 200.0 / 1.5 + BADGE_GAP)).abs() < 0.01, "{}", rh.y);
        // Secondary: no room below on a 480pt-tall panel, so above it (300 - gap - 44).
        assert_eq!(rl.y, 300.0 - BADGE_GAP - H.1);
    }

    /// A selection that touches none of this output places nothing — the check that decides
    /// it now runs in POINT space, so it has to survive scaling too (DRAGON-207 then routes
    /// that output's toolbar to its own bottom-centre).
    #[test]
    fn a_selection_off_this_output_places_nothing_at_any_scale() {
        for factor in [1.0f32, 1.5, 2.0, 3.0] {
            let o = out((0, 0), (3840, 2160), factor);
            assert!(place(o, (-500, 100, 200, 200)).is_none(), "left of it, factor {factor}");
            assert!(place(o, (100, -500, 200, 200)).is_none(), "above it, factor {factor}");
            assert!(place(o, (4000, 100, 200, 200)).is_none(), "right of it, factor {factor}");
            assert!(place(o, (100, 3000, 200, 200)).is_none(), "below it, factor {factor}");
            // Just overlapping by one capture unit still counts.
            assert!(place(o, (-199, 100, 200, 200)).is_some(), "factor {factor}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Selection;

    fn region(w: u32, h: u32) -> Selection {
        Selection { x: 100, y: 200, width: w, height: h, output: None, window_id: None }
    }

    /// DRAGON-418, THE regression this fix exists for: a region capture must never be
    /// inset out of existence. `normalized_region` admits any selection from 1x1 up, and
    /// a zero-extent selection intersects no display — so every downstream grab returns
    /// `None`, which `do_pixel_capture` turns into a silent, file-less session exit.
    /// Non-empty in, non-empty out, for every extent the commit gate can produce.
    #[test]
    fn insetting_never_empties_a_committed_selection() {
        for w in 1..=64u32 {
            for h in [1u32, 2, 3, 4, 5, 8, 40] {
                let out = inset_region(region(w, h));
                assert!(
                    out.width >= 1 && out.height >= 1,
                    "{w}x{h} inset to {}x{} — a zero axis captures nothing at all",
                    out.width,
                    out.height
                );
            }
        }
    }

    /// The inset stays INSIDE the selection it came from: never wider than the original,
    /// and never starting before it. (A "fix" that padded the rect back out would move
    /// the capture off what the user drew.)
    #[test]
    fn the_inset_rect_stays_within_the_original() {
        for w in 1..=64u32 {
            let orig = region(w, w);
            let out = inset_region(orig.clone());
            assert!(out.width <= orig.width && out.height <= orig.height, "grew at {w}");
            assert!(out.x >= orig.x && out.y >= orig.y, "moved out at {w}");
            assert!(
                out.x + out.width as i32 <= orig.x + orig.width as i32,
                "right edge escaped at {w}"
            );
            assert!(
                out.y + out.height as i32 <= orig.y + orig.height as i32,
                "bottom edge escaped at {w}"
            );
        }
    }

    /// Every selection big enough to have survived the old flat inset is BYTE-IDENTICAL,
    /// so no real capture changes: only the 1..=4 range that was guaranteed to fail moves.
    #[test]
    fn selections_five_points_and_wider_are_unchanged() {
        let i = SELECTION_LINE_W;
        for w in 5..=4000u32 {
            let out = inset_region(region(w, w));
            assert_eq!(out.x, 100 + i, "x changed at {w}");
            assert_eq!(out.y, 200 + i, "y changed at {w}");
            assert_eq!(out.width, w - (2 * i) as u32, "width changed at {w}");
            assert_eq!(out.height, w - (2 * i) as u32, "height changed at {w}");
        }
    }

    /// The exact range the old code zeroed, spelled out: each of these committed a
    /// capture that could only ever return nothing.
    #[test]
    fn the_previously_fatal_range_now_yields_real_pixels() {
        for (w, want) in [(1u32, 1u32), (2, 2), (3, 1), (4, 2)] {
            let out = inset_region(region(w, 50));
            assert_eq!(out.width, want, "width for a {w}pt selection");
            // The old formula: `w.saturating_sub(4)` — zero for every one of them.
            assert_eq!(w.saturating_sub((2 * SELECTION_LINE_W) as u32), 0, "{w} used to zero");
        }
    }

    /// Window and monitor selections capture their whole target and are never inset —
    /// the outline is drawn OUTSIDE them, so there is nothing to trim.
    #[test]
    fn window_and_monitor_selections_pass_through_untouched() {
        let win = Selection { window_id: Some("w1".into()), ..region(3, 3) };
        assert_eq!(inset_region(win.clone()), win);
        let mon = Selection { output: Some("Display-1".into()), ..region(3, 3) };
        assert_eq!(inset_region(mon.clone()), mon);
    }

    /// The per-axis helper's contract, independent of `Selection`: never negative, never
    /// more than the outline width, and never enough to consume the axis.
    #[test]
    fn axis_inset_is_bounded_and_leaves_a_point_standing() {
        assert_eq!(axis_inset(0), 0);
        for e in 1..=2000u32 {
            let i = axis_inset(e);
            assert!((0..=SELECTION_LINE_W).contains(&i), "out of range at {e}: {i}");
            assert!(e as i32 - 2 * i >= 1, "axis emptied at {e}");
        }
    }
}
