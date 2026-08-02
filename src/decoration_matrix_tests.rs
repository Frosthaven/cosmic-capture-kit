//! The SINGLE-WINDOW half of the capture-extras behaviour matrix (DRAGON-463).
//!
//! WHAT THIS IS FOR. "Preserve transparency", "Show wallpaper behind the window", "Window
//! borders", "Window padding", "Window shadow" and "Preserve mouse cursor" are toggles a
//! user sees. What they DO to the pixels was, until the shared decoration seam landed,
//! written out three times (`platform/linux/native/screenshot.rs`,
//! `platform/mac/screenshot.rs`, `platform/windows/screenshot.rs`). The copies had drifted
//! only slightly, which is worse than drifting a lot, because nobody could see it.
//!
//! These rows drive [`crate::decoration`] directly: `decorate`, `backdrop_for`,
//! `apply_backdrop`, `cursor_offset` and `cursor_over_window` are the whole portable
//! assembly, so one shared module covers every platform that mounts them. They are the
//! PARITY CHECKLIST for a new compositor: adding one means RUNNING these, not rewriting
//! them. A row that fails names exactly what the new backend does differently.
//!
//! HOW THE ROWS ARE WRITTEN. Every row builds synthetic inputs and asserts OUTPUT PIXELS
//! at named sample points. A test that only asserted a decision function returned the
//! right enum would restate the code and keep passing while the picture silently went
//! wrong. [`backdrop_for`] is the single exception where the enum IS the behaviour, and
//! even there the row is paired with pixel rows that run each arm through
//! [`apply_backdrop`]. Colours are deliberately vivid (pure red window, magenta wallpaper,
//! blue ring, green cursor) and the assertions sample named points (a margin, the
//! outermost ring pixel, the innermost, the first pixel past it, a carved corner) instead
//! of hashing the image, so a failure says WHAT moved.
//!
//! WHAT THESE ROWS DELIBERATELY DO NOT COVER, because it is genuinely native:
//!
//! - WHERE the pixels come from. Each platform grabs its own window pixels, its own
//!   wallpaper pixels and its own corner shape. [`Backdrop::Behind`] takes prepared
//!   wallpaper pixels, so the rows assert what is DONE with them, never how they were
//!   cropped (each platform's `composite_over_wallpaper` computes its own crop).
//! - FROSTED GLASS, which only the Linux composite reproduces.
//! - The `min_border_px` DIVERGENCE is not smoothed over: macOS floors the scaled ring at
//!   one physical pixel and the other two do not, so
//!   [`the_scaled_ring_vanishes_below_one_pixel_unless_the_platform_floors_it`] pins BOTH
//!   answers and names which platform picks which.

use crate::decoration::{
    Backdrop, BackdropKind, BorderSpec, CornerStyle, DecorationOpts, Decorated, apply_backdrop,
    backdrop_for, cursor_offset, cursor_over_window, decorate,
};
use image::RgbaImage;

const RED: [u8; 4] = [255, 0, 0, 255];
/// The same red at half alpha: a translucent window body.
const RED_TRANSLUCENT: [u8; 4] = [255, 0, 0, 128];
const MAGENTA: [u8; 4] = [255, 0, 255, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// What "no see-through pixel" means in practice, and why it is not literally 255.
///
/// Compositing over an opaque backdrop is `bg_a + fg_a - bg_a*fg_a`, which for `bg_a == 1`
/// is mathematically 1.0 for any foreground. The `image` crate evaluates it in f32 and then
/// TRUNCATES the cast back to `u8`, so a value that lands one ulp short comes out as 254
/// instead of 255. Every partially-transparent pixel put over black or over wallpaper is
/// therefore 254, not 255: an invisible 1/255 rounding artifact in the dependency, not a
/// hole in our picture. Rows assert against this floor and separately pin the fully-opaque
/// interior at exactly 255, so a REAL leak (a pixel that is actually see-through) still
/// fails loudly.
const OPAQUE_ENOUGH: u8 = 254;

/// A flat captured window, already in PHYSICAL pixels (the seam never sees logical sizes).
fn window(w: u32, h: u32, color: [u8; 4]) -> RgbaImage {
    RgbaImage::from_pixel(w, h, image::Rgba(color))
}

/// A window delivered with its corners ALREADY shaped as alpha, i.e. what macOS SCK hands
/// back: a flat body with each corner carved to a quarter circle of `radius`. This is the
/// input [`CornerStyle::Native`] exists for.
fn native_corner_window(w: u32, h: u32, radius: u32, color: [u8; 4]) -> RgbaImage {
    let mut img = window(w, h, color);
    let rf = radius as f32;
    let corners = [
        (0, 0, rf, rf),
        (w - radius, 0, (w - radius) as f32, rf),
        (0, h - radius, rf, (h - radius) as f32),
        (w - radius, h - radius, (w - radius) as f32, (h - radius) as f32),
    ];
    for (ox, oy, ccx, ccy) in corners {
        for y in oy..oy + radius {
            for x in ox..ox + radius {
                let dx = x as f32 + 0.5 - ccx;
                let dy = y as f32 + 0.5 - ccy;
                if dx * dx + dy * dy > rf * rf {
                    img.put_pixel(x, y, image::Rgba(CLEAR));
                }
            }
        }
    }
    img
}

/// Prepared wallpaper pixels for [`Backdrop::Behind`]: one vivid flat colour, sized to the
/// decorated canvas, so "the wallpaper is visible here" is a colour comparison and not a
/// judgement call.
fn wallpaper(w: u32, h: u32) -> RgbaImage {
    window(w, h, MAGENTA)
}

/// Every toggle off: keep the window's own alpha, no rounding of our own, no ring, no
/// shadow, no padding, 1x display. Each row switches on only what it is about.
fn opts() -> DecorationOpts {
    DecorationOpts {
        keep_transparency: true,
        corners: CornerStyle::Rounded(0),
        border: None,
        shadow: false,
        dark: true,
        pad_logical: 0.0,
        scale: 1.0,
        min_border_px: 0,
    }
}

/// The ring as the settings page configures it: a width in LOGICAL px plus a colour, taken
/// through [`BorderSpec::to_compose`] exactly as every caller does, so a row also pins that
/// width 0 means "draw nothing" rather than "draw a 0px ring".
fn ring(width: u32) -> Option<(f32, [u8; 4])> {
    BorderSpec { width, color: BLUE }.to_compose()
}

/// The cursor tail every platform's single-window capture performs: overlay the sprite at
/// [`cursor_offset`], but ONLY when the launch-locked pointer was over the window itself
/// (DRAGON-213). Returns the offset it drew at, or `None` when the gate refused.
fn overlay_cursor(
    canvas: &mut RgbaImage,
    sel: (i32, i32, u32, u32),
    dec: &Decorated,
    scale: f32,
    sprite: &RgbaImage,
    pointer: (i32, i32),
    hotspot: (i32, i32),
) -> Option<(i64, i64)> {
    if !cursor_over_window(pointer.0, pointer.1, sel.0, sel.1, sel.2, sel.3) {
        return None;
    }
    let off = cursor_offset(pointer, (sel.0, sel.1), dec.total_margin_logical, scale, hotspot);
    image::imageops::overlay(canvas, sprite, off.0, off.1);
    Some(off)
}

#[track_caller]
fn assert_px(img: &RgbaImage, x: u32, y: u32, want: [u8; 4], what: &str) {
    let got = img.get_pixel(x, y).0;
    assert_eq!(got, want, "{what}: pixel ({x},{y}) is {got:?}, expected {want:?}");
}

/// Corner carving clears ALPHA and leaves the colour bytes alone, so a carved pixel reads
/// `[body colour, 0]` rather than [`CLEAR`]. Rows about the corner therefore assert the
/// alpha, which is the part that decides what shows through.
#[track_caller]
fn assert_alpha(img: &RgbaImage, x: u32, y: u32, want: u8, what: &str) {
    let got = img.get_pixel(x, y).0;
    assert_eq!(got[3], want, "{what}: pixel ({x},{y}) is {got:?}, expected alpha {want}");
}

#[track_caller]
fn assert_no_pixel(img: &RgbaImage, unwanted: [u8; 4], what: &str) {
    let hit = img.enumerate_pixels().find(|(_, _, p)| p.0 == unwanted);
    assert!(hit.is_none(), "{what}: found {unwanted:?} at {:?}", hit.map(|(x, y, _)| (x, y)));
}

// ── Transparency and the backdrop ────────────────────────────────────────────────────

#[test]
fn transparency_off_leaves_no_see_through_pixel_and_the_black_backdrop_fills_the_gaps() {
    // A translucent window, "Preserve transparency" OFF, rounded corners and padding, so
    // the canvas has three kinds of hole to fill: the flattened body, the carved corner,
    // and the margin.
    let mut o = opts();
    o.keep_transparency = false;
    o.corners = CornerStyle::Rounded(10);
    o.pad_logical = 8.0;
    let dec = decorate(window(40, 40, RED_TRANSLUCENT), &o);
    // Wallpaper OFF + transparency OFF is the black arm; run it, as a real capture does.
    let out = apply_backdrop(dec.img, Backdrop::Black);
    // Nothing may be see-through: the capture is going somewhere that has no idea what to
    // put behind it. (See OPAQUE_ENOUGH for why the floor is 254 and not 255.)
    let leak = out.enumerate_pixels().find(|(_, _, p)| p.0[3] < OPAQUE_ENOUGH);
    assert!(leak.is_none(), "transparency OFF must leave no see-through pixel, found {leak:?}");
    // The body kept its colour and simply lost its translucency (not blended toward black),
    // and it is opaque outright, not merely opaque enough.
    assert_px(&out, 28, 28, RED, "the flattened window body");
    // The corner is still carved; the backdrop is what fills it.
    assert_px(&out, 8, 8, BLACK, "the carved corner shows the backdrop");
    assert_px(&out, 0, 0, BLACK, "the padding margin shows the backdrop");
}

#[test]
fn transparency_on_keeps_the_windows_own_alpha_and_the_margin_stays_clear() {
    let mut o = opts();
    o.corners = CornerStyle::Rounded(10);
    o.pad_logical = 8.0;
    let dec = decorate(window(40, 40, RED_TRANSLUCENT), &o);
    let out = apply_backdrop(dec.img, Backdrop::Transparent);
    // The window's OWN alpha survives the whole assembly, byte for byte.
    assert_px(&out, 28, 28, RED_TRANSLUCENT, "the translucent window body");
    // Fully carved, and here truly [0,0,0,0]: the padding overlay skips a source pixel
    // whose alpha is 0, so what remains is the transparent canvas underneath it.
    assert_px(&out, 8, 8, CLEAR, "the carved corner");
    assert_px(&out, 0, 0, CLEAR, "the padding margin");
}

#[test]
fn the_wallpaper_shows_through_a_translucent_window_body() {
    let mut o = opts();
    o.pad_logical = 6.0;
    let dec = decorate(window(40, 40, RED_TRANSLUCENT), &o);
    let (w, h) = (dec.img.width(), dec.img.height());
    let out = apply_backdrop(dec.img, Backdrop::Behind(wallpaper(w, h)));
    // Outside the window the wallpaper is simply itself.
    assert_px(&out, 0, 0, MAGENTA, "the padding margin is pure wallpaper");
    // Through the body, the wallpaper's blue channel is measurably present. Magenta is the
    // only source of blue in this scene: the window has none.
    let body = out.get_pixel(26, 26).0;
    assert!(body[2] > 100, "the wallpaper must be visible through the body, got {body:?}");
    assert!(body[3] >= OPAQUE_ENOUGH, "the wallpaper backing leaves nothing see-through");
}

#[test]
fn an_opaque_window_hides_the_wallpaper_behind_it() {
    // The control for the row above: same wallpaper, same geometry, opaque window. If the
    // blue channel appears HERE, the transparency toggle is not doing the work.
    let mut o = opts();
    o.pad_logical = 6.0;
    let dec = decorate(window(40, 40, RED), &o);
    let (w, h) = (dec.img.width(), dec.img.height());
    let out = apply_backdrop(dec.img, Backdrop::Behind(wallpaper(w, h)));
    assert_px(&out, 0, 0, MAGENTA, "the padding margin is still pure wallpaper");
    assert_px(&out, 26, 26, RED, "an opaque body shows no wallpaper at all");
}

#[test]
fn wallpaper_off_with_transparency_on_stays_transparent_and_never_black() {
    // Turning the wallpaper off only says "do not show the desktop". With transparency ON
    // the honest answer to "then what?" is nothing at all.
    let mut o = opts();
    o.pad_logical = 6.0;
    let dec = decorate(window(40, 40, RED), &o);
    let out = apply_backdrop(dec.img, Backdrop::Transparent);
    assert_px(&out, 0, 0, CLEAR, "the margin stays transparent");
    assert_px(&out, 3, 26, CLEAR, "beside the window is transparent, not filled");
    assert_no_pixel(&out, BLACK, "nothing may be painted opaque black");
    assert_px(&out, 26, 26, RED, "the window itself is untouched");
}

#[test]
fn wallpaper_off_with_transparency_off_is_black_not_transparent() {
    // With transparency OFF there is no alpha to keep, so the capture needs an opaque
    // backing and gets black.
    let mut o = opts();
    o.keep_transparency = false;
    o.pad_logical = 6.0;
    let dec = decorate(window(40, 40, RED), &o);
    let out = apply_backdrop(dec.img, Backdrop::Black);
    assert_px(&out, 0, 0, BLACK, "the margin is opaque black");
    assert_px(&out, 3, 26, BLACK, "beside the window is opaque black");
    assert_no_pixel(&out, CLEAR, "no pixel may be left transparent");
}

#[test]
fn the_backdrop_rule_is_wallpaper_first_then_black_when_flattened_else_transparent() {
    // The one row where the enum IS the behaviour: the two toggles are not independent
    // bits. The four pixel rows above are what each arm then produces.
    assert_eq!(backdrop_for(true, true), BackdropKind::Wallpaper, "wallpaper wins when on");
    assert_eq!(backdrop_for(true, false), BackdropKind::Wallpaper, "even with alpha flattened");
    assert_eq!(backdrop_for(false, false), BackdropKind::Black, "no wallpaper, no alpha: black");
    assert_eq!(backdrop_for(false, true), BackdropKind::Transparent, "no wallpaper, keep alpha");
}

// ── The ring ────────────────────────────────────────────────────────────────────────

#[test]
fn a_border_draws_its_colour_at_the_configured_width_inside_the_padding() {
    let mut o = opts();
    o.border = ring(3);
    o.pad_logical = 8.0;
    let dec = decorate(window(40, 40, RED), &o);
    // Total margin 8 logical px: a 3px ring with 5px of clear padding outside it.
    let out = dec.img;
    assert_eq!((out.width(), out.height()), (56, 56), "window + 2*(5 padding + 3 ring)");
    let y = 28; // mid-height, far from every corner
    assert_px(&out, 4, y, CLEAR, "one pixel beyond the ring, outward");
    assert_px(&out, 5, y, BLUE, "the outermost ring pixel");
    assert_px(&out, 7, y, BLUE, "the innermost ring pixel");
    assert_px(&out, 8, y, RED, "the first pixel beyond the ring, inward: the window");
    // The measurements the caller positions everything else from.
    assert_eq!(dec.content, (40, 40), "the window content keeps its own size");
    assert_eq!(dec.outer_radius, 3, "the outer shape the shadow would follow");
    assert!((dec.total_margin_logical - 8.0).abs() < f32::EPSILON, "ring + padding");
}

#[test]
fn a_zero_width_border_draws_no_ring_and_does_not_change_the_canvas() {
    let win = window(40, 40, RED);
    let mut o = opts();
    o.border = ring(0);
    assert!(o.border.is_none(), "width 0 means draw nothing, not draw a 0px ring");
    let dec = decorate(win.clone(), &o);
    assert_eq!((dec.img.width(), dec.img.height()), (40, 40), "the canvas is not grown");
    assert_no_pixel(&dec.img, BLUE, "no ring colour anywhere");
    assert_eq!(dec.img.as_raw(), win.as_raw(), "the window comes back untouched");
}

#[test]
fn turning_a_border_on_does_not_grow_a_canvas_whose_padding_can_hold_it() {
    // The `pad_logical - bw_logical` rule. The ring is drawn INSIDE the user's padding, so
    // switching it on must move the picture's edge nowhere. This is easy to regress into
    // "canvas grows by the ring", which silently changes every padded capture's size.
    let mut plain = opts();
    plain.pad_logical = 8.0;
    let mut ringed = opts();
    ringed.pad_logical = 8.0;
    ringed.border = ring(3);
    let a = decorate(window(40, 40, RED), &plain);
    let b = decorate(window(40, 40, RED), &ringed);
    assert_eq!(
        (a.img.width(), a.img.height()),
        (b.img.width(), b.img.height()),
        "the ring must come out of the padding, not out of the canvas"
    );
    assert!(
        (a.total_margin_logical - b.total_margin_logical).abs() < f32::EPSILON,
        "and the reported margin is the same 8 logical px either way"
    );
    // Same canvas, different pixels: where the plain one has clear padding the ringed one
    // has its ring.
    assert_px(&a.img, 5, 28, CLEAR, "no ring without a border");
    assert_px(&b.img, 5, 28, BLUE, "the ring occupies the inner 3px of that same padding");
}

#[test]
fn a_border_wider_than_the_padding_still_draws_and_grows_the_canvas_by_the_ring() {
    // The clamp's honest consequence: a 3px ring cannot fit inside 2px of padding, so the
    // margin goes to zero and the canvas grows by the ring alone. The alternative would be
    // silently thinning a ring the user configured.
    let mut o = opts();
    o.border = ring(3);
    o.pad_logical = 2.0;
    let dec = decorate(window(40, 40, RED), &o);
    assert_eq!((dec.img.width(), dec.img.height()), (46, 46), "grown by the ring only");
    assert_px(&dec.img, 0, 23, BLUE, "the ring starts at the canvas edge");
    assert_px(&dec.img, 3, 23, RED, "and the window starts right after it");
    assert!(
        (dec.total_margin_logical - 3.0).abs() < f32::EPSILON,
        "the reported margin is the ring, which is wider than the configured padding"
    );
}

#[test]
fn the_scaled_ring_vanishes_below_one_pixel_unless_the_platform_floors_it() {
    // The one DIVERGENCE the seam preserves rather than unifies. At a sub-1.0 backing
    // scale a 1px logical ring rounds to 0 physical px. macOS passes min_border_px 1 so a
    // configured border can never round away; Linux and Windows pass 0, so it can.
    let mut vanishing = opts();
    vanishing.border = ring(1);
    vanishing.scale = 0.4;
    let gone = decorate(window(20, 20, RED), &vanishing);
    assert_eq!((gone.img.width(), gone.img.height()), (20, 20), "no ring, no growth");
    assert_no_pixel(&gone.img, BLUE, "Linux and Windows let a sub-pixel ring disappear");

    let mut floored = vanishing;
    floored.min_border_px = 1;
    let kept = decorate(window(20, 20, RED), &floored);
    assert_eq!((kept.img.width(), kept.img.height()), (22, 22), "floored to a 1px ring");
    assert_px(&kept.img, 0, 11, BLUE, "macOS keeps the configured border visible");
}

// ── Padding, scale and the shadow ───────────────────────────────────────────────────

#[test]
fn padding_grows_the_canvas_by_twice_the_scaled_padding_and_the_margin_is_transparent() {
    let mut o = opts();
    o.pad_logical = 8.0;
    let dec = decorate(window(40, 30, RED), &o);
    assert_eq!((dec.img.width(), dec.img.height()), (56, 46), "40+2*8 by 30+2*8");
    assert_px(&dec.img, 0, 0, CLEAR, "the corner of the margin");
    assert_px(&dec.img, 7, 23, CLEAR, "the last margin pixel before the window");
    assert_px(&dec.img, 8, 23, RED, "the window starts exactly at the padding");
    // Scaled: the padding is logical, so a 1.5x display pays 12 physical px per side.
    let mut scaled = o;
    scaled.scale = 1.5;
    let hidpi = decorate(window(40, 30, RED), &scaled);
    assert_eq!((hidpi.img.width(), hidpi.img.height()), (64, 54), "40+2*12 by 30+2*12");
    assert_px(&hidpi.img, 11, 27, CLEAR, "the last margin pixel at 1.5x");
    assert_px(&hidpi.img, 12, 27, RED, "the window starts at 12 physical px");
}

#[test]
fn a_backing_scale_of_two_doubles_both_the_ring_and_the_margin() {
    let mut o = opts();
    o.border = ring(3);
    o.pad_logical = 8.0;
    o.scale = 2.0;
    let dec = decorate(window(40, 40, RED), &o);
    // 5 logical px of padding and a 3 logical px ring, both doubled.
    assert_eq!((dec.img.width(), dec.img.height()), (72, 72), "40 + 2*(10 + 6)");
    let y = 36;
    assert_px(&dec.img, 9, y, CLEAR, "one pixel beyond the doubled ring");
    assert_px(&dec.img, 10, y, BLUE, "the outermost ring pixel, at twice the offset");
    assert_px(&dec.img, 15, y, BLUE, "the innermost pixel of the 6px ring");
    assert_px(&dec.img, 16, y, RED, "the window, at twice the offset");
    assert!(
        (dec.total_margin_logical - 8.0).abs() < f32::EPSILON,
        "the reported margin stays LOGICAL, it is the caller that scales it"
    );
}

#[test]
fn a_shadow_paints_into_the_margin_outside_the_window_without_enlarging_the_canvas() {
    let mut o = opts();
    o.pad_logical = 8.0;
    o.shadow = true;
    let dec = decorate(window(40, 40, RED), &o);
    assert_eq!((dec.img.width(), dec.img.height()), (56, 56), "the shadow lives in the padding");
    // Just outside the window's left edge, mid-height: a dark, partly transparent halo.
    let halo = dec.img.get_pixel(7, 28).0;
    assert_eq!([halo[0], halo[1], halo[2]], [0, 0, 0], "the shadow is black");
    // Visible, not a whisper: this samples ~116/255 today. The floor is loose because the
    // exact value is the blur's business, but a shadow reduced to nothing still fails.
    assert!(halo[3] > 20, "the margin outside the window carries shadow, got alpha {}", halo[3]);
    assert!(halo[3] < 255, "a shadow, not a wall, got alpha {}", halo[3]);
    // The window itself is not shadowed over: cosmic draws no shadow UNDER the window.
    assert_px(&dec.img, 28, 28, RED, "the window body is untouched by its own shadow");
}

#[test]
fn without_a_shadow_the_margin_is_fully_transparent() {
    let mut o = opts();
    o.pad_logical = 8.0;
    let dec = decorate(window(40, 40, RED), &o);
    assert_eq!((dec.img.width(), dec.img.height()), (56, 56), "same canvas as with a shadow");
    assert_px(&dec.img, 7, 28, CLEAR, "no halo where the shadow would be");
    // Nothing at all outside the window rect: the whole margin is clear.
    for (x, y, p) in dec.img.enumerate_pixels() {
        let inside = (8..48).contains(&x) && (8..48).contains(&y);
        if !inside {
            assert_eq!(p.0[3], 0, "shadow OFF leaves the margin clear, but ({x},{y}) is {:?}", p.0);
        }
    }
}

#[test]
fn a_shadow_with_no_padding_is_clipped_away_rather_than_growing_the_canvas() {
    // The shadow draws into whatever margin exists. With padding off there is none, so it
    // is clipped entirely: the picture is exactly the window. Growing the canvas to fit a
    // shadow the user did not ask room for would change every unpadded capture's size.
    let win = window(40, 40, RED);
    let mut o = opts();
    o.shadow = true;
    let dec = decorate(win.clone(), &o);
    assert_eq!((dec.img.width(), dec.img.height()), (40, 40), "the canvas is not grown");
    assert_eq!(dec.img.as_raw(), win.as_raw(), "and nothing of the shadow survives");
}

// ── Corner style: the macOS path, testable from any host ────────────────────────────

#[test]
fn rounded_corners_carve_the_captured_corner_away() {
    // Linux and Windows: the compositor hands back the full rectangle, so the radius is
    // ours to draw.
    let mut o = opts();
    o.corners = CornerStyle::Rounded(10);
    let dec = decorate(window(40, 40, RED), &o);
    assert_eq!((dec.img.width(), dec.img.height()), (40, 40), "rounding never resizes");
    // Carving clears the ALPHA and leaves the colour bytes where they were, so the corner
    // reads [255,0,0,0]: invisible, but not zeroed. Anything compositing this (the
    // backdrops, the padding overlay) keys off the alpha, so the leftover colour never
    // shows. Asserting the whole RGBA here would pin a detail nothing depends on.
    assert_alpha(&dec.img, 0, 0, 0, "the extreme corner is carved out");
    assert_alpha(&dec.img, 39, 0, 0, "every corner, not just the first");
    assert_alpha(&dec.img, 0, 39, 0, "including the bottom pair");
    assert_alpha(&dec.img, 39, 39, 0, "including the bottom pair");
    assert_px(&dec.img, 20, 20, RED, "the body is untouched");
    assert_px(&dec.img, 20, 0, RED, "and so is the middle of each edge");
}

#[test]
fn native_corners_leave_the_captured_alpha_exactly_as_delivered() {
    // macOS: SCK bakes the window's real squircle in as alpha, so rounding again would
    // carve a second ring off real pixels. The same square input that Rounded(10) carves
    // above comes back whole here.
    let win = window(40, 40, RED);
    let mut o = opts();
    o.corners = CornerStyle::Native;
    let dec = decorate(win.clone(), &o);
    assert_px(&dec.img, 0, 0, RED, "a square-cornered capture stays square");
    assert_eq!(dec.img.as_raw(), win.as_raw(), "nothing at all is done to the alpha");
    // And a capture that ALREADY carries its corner keeps exactly that corner.
    let native = native_corner_window(40, 40, 10, RED);
    let kept = decorate(native.clone(), &o);
    assert_px(&kept.img, 0, 0, CLEAR, "the delivered corner is preserved");
    assert_eq!(kept.img.as_raw(), native.as_raw(), "byte for byte");
}

#[test]
fn native_corners_survive_transparency_off_while_the_body_is_filled_opaque() {
    // Transparency OFF must fill a translucent body without squaring the native corner
    // (DRAGON-268): flattening every pixel would colour the corner and the ring would then
    // trace a rectangle.
    let mut o = opts();
    o.corners = CornerStyle::Native;
    o.keep_transparency = false;
    let dec = decorate(native_corner_window(40, 40, 10, RED_TRANSLUCENT), &o);
    assert_px(&dec.img, 20, 20, RED, "the translucent body is filled opaque");
    assert_px(&dec.img, 0, 0, CLEAR, "the native rounded corner is NOT squared");
    assert_px(&dec.img, 39, 39, CLEAR, "on every corner");
}

#[test]
fn a_native_corner_ring_follows_the_windows_own_alpha_instead_of_a_circle() {
    // The ring is drawn by dilating the window's alpha, so it hugs whatever corner shape
    // arrived (circle, squircle, square) and never boxes it in.
    let mut o = opts();
    o.corners = CornerStyle::Native;
    o.border = ring(3);
    let dec = decorate(native_corner_window(40, 40, 10, RED), &o);
    assert_eq!((dec.img.width(), dec.img.height()), (46, 46), "grown by the ring");
    let y = 23;
    assert_px(&dec.img, 0, y, BLUE, "the outermost ring pixel on a straight edge");
    assert_px(&dec.img, 2, y, BLUE, "the innermost ring pixel");
    assert_px(&dec.img, 3, y, RED, "and the window right after it");
    // Diagonally outside the carved corner there is no window to dilate, so there is no
    // ring: the border is rounded, not a box.
    assert_px(&dec.img, 0, 0, CLEAR, "no ring where the window's alpha says there is none");
}

// ── The cursor ──────────────────────────────────────────────────────────────────────

#[test]
fn the_cursor_lands_at_the_pointer_offset_by_the_margin_and_its_hotspot() {
    // The window occupies logical (100,200)-(140,240); the pointer sat 10 right and 5 down
    // of its origin, with a hotspot 2px into a 4px sprite.
    let sel = (100, 200, 40u32, 40u32);
    let mut o = opts();
    o.pad_logical = 8.0;
    let dec = decorate(window(40, 40, RED), &o);
    let mut out = apply_backdrop(dec.img.clone(), Backdrop::Transparent);
    let sprite = window(4, 4, GREEN);
    let off = overlay_cursor(&mut out, sel, &dec, 1.0, &sprite, (110, 205), (2, 2))
        .expect("a pointer over the window draws");
    // The canvas extends 8px past the window, so the sprite is pushed by exactly that.
    assert_eq!(off, (16, 11), "pointer offset + margin - hotspot");
    assert_px(&out, 18, 13, GREEN, "the sprite's HOTSPOT sits on the pointer position");
    assert_px(&out, 16, 11, GREEN, "the sprite's own top-left corner");
    assert_px(&out, 19, 14, GREEN, "and its bottom-right");
    assert_px(&out, 15, 11, RED, "nothing is drawn left of the sprite");
    assert_px(&out, 20, 13, RED, "nor right of it");

    // On a 2x display the same pointer is twice as far into twice as large a canvas.
    let mut hidpi = o;
    hidpi.scale = 2.0;
    let dec2 = decorate(window(80, 80, RED), &hidpi);
    let mut out2 = apply_backdrop(dec2.img.clone(), Backdrop::Transparent);
    let off2 = overlay_cursor(&mut out2, sel, &dec2, 2.0, &sprite, (110, 205), (2, 2))
        .expect("still over the window");
    assert_eq!(off2, (34, 24), "the offset and the margin both scale");
    assert_px(&out2, 36, 26, GREEN, "the hotspot lands on the scaled pointer position");
}

#[test]
fn a_pointer_outside_the_window_draws_no_cursor_even_though_the_canvas_reaches_it() {
    // DRAGON-213. The decorated canvas extends past the window content, so a pointer just
    // OUTSIDE the window still maps to a real pixel of the canvas. Without the gate the
    // sprite floats in the margin beside the window, which is what users saw.
    let sel = (100, 200, 40u32, 40u32);
    let mut o = opts();
    o.pad_logical = 8.0;
    let dec = decorate(window(40, 40, RED), &o);
    let mut out = apply_backdrop(dec.img.clone(), Backdrop::Transparent);
    let sprite = window(4, 4, GREEN);
    let drawn = overlay_cursor(&mut out, sel, &dec, 1.0, &sprite, (99, 220), (2, 2));
    assert!(drawn.is_none(), "a pointer one pixel left of the window must not draw");
    assert_no_pixel(&out, GREEN, "no cursor anywhere on the canvas");
    // It is not that the position was off-canvas: the ungated offset lands inside it.
    let would_be = cursor_offset((99, 220), (100, 200), dec.total_margin_logical, 1.0, (2, 2));
    assert_eq!(would_be, (5, 26), "the gate, not clipping, is what suppresses it");
}

#[test]
fn the_pointer_is_over_the_window_inside_its_rect_and_not_one_pixel_past_any_edge() {
    // The rect is half-open: the top-left is included, the bottom-right edge is not.
    let (x, y, w, h) = (100, 200, 40u32, 30u32);
    assert!(cursor_over_window(100, 200, x, y, w, h), "the top-left corner is inside");
    assert!(cursor_over_window(120, 215, x, y, w, h), "the middle is inside");
    assert!(cursor_over_window(139, 229, x, y, w, h), "the last pixel inside each axis");
    assert!(!cursor_over_window(99, 215, x, y, w, h), "one pixel left is outside");
    assert!(!cursor_over_window(140, 215, x, y, w, h), "one pixel right is outside");
    assert!(!cursor_over_window(120, 199, x, y, w, h), "one pixel above is outside");
    assert!(!cursor_over_window(120, 230, x, y, w, h), "one pixel below is outside");
}

// ── The reported measurements ───────────────────────────────────────────────────────

#[test]
fn the_reported_margin_locates_the_window_content_within_a_pixel() {
    // `total_margin_logical` is what the cursor overlay and the wallpaper crop position
    // from, so it has to agree with where the content ACTUALLY starts.
    //
    // It is exact at integer scales. At a fractional one it can be a pixel short, because
    // the assembly rounds the ring and the padding SEPARATELY while this reports their
    // fused sum rounded once: at 1.5x a 3px ring inside 8px of padding draws at
    // round(4.5)=5 plus round(7.5)=8, so the content starts at 13, while the report is
    // round(8*1.5)=12. The visible effect is the cursor sprite sitting one physical pixel
    // up and left of true on such a display. The slack is pinned here at one pixel rather
    // than left to be rediscovered; a fix that makes it exact keeps this row passing, a
    // drift to two pixels does not.
    for &(scale, exact) in &[(1.0f32, true), (2.0, true), (1.5, false)] {
        let mut o = opts();
        o.border = ring(3);
        o.pad_logical = 8.0;
        o.scale = scale;
        let dec = decorate(window(40, 40, RED), &o);
        let y = dec.img.height() / 2;
        let actual = (0..dec.img.width())
            .find(|&x| dec.img.get_pixel(x, y).0 == RED)
            .expect("the window content is somewhere on the mid row") as i64;
        let reported = (dec.total_margin_logical * scale).round() as i64;
        if exact {
            assert_eq!(reported, actual, "at {scale}x the reported margin IS the content edge");
        } else {
            assert!(
                (reported - actual).abs() <= 1,
                "at {scale}x the reported margin ({reported}) is within a pixel of the \
                 content edge ({actual})"
            );
        }
        assert_eq!(dec.content, (40, 40), "the content size is the window's own");
    }
}
