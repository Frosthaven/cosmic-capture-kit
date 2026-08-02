//! The REGION / MONITOR half of the capture-extras behaviour matrix (DRAGON-463).
//!
//! WHAT THIS IS FOR. "Preserve transparency", "Window borders", "Preserve mouse cursor"
//! and the placement/scale/trim maths behind them are the toggles a user sees. Until now
//! nothing pinned what they DO to the pixels: the composite lived in three near-identical
//! per-platform bodies (`platform/linux/native/screenshot.rs`, `platform/mac/screenshot.rs`,
//! `platform/windows/screenshot.rs`), so a test written against one proved nothing about
//! the others, and a new compositor would have added a fourth copy no test could see.
//!
//! WHY IT CAN BE ONE SHARED FILE. `crate::screenshot` is a `#[path]` mount (see
//! `src/main.rs`) and `region_windows_frozen` has the SAME signature on all three
//! platforms. So this module compiles everywhere and exercises whichever implementation
//! the host platform ships. Run the suite on each OS and parity becomes a diff of test
//! results instead of a code review of three files. Add a compositor, run these, and the
//! rows that fail ARE the checklist of what the new backend does differently.
//!
//! HOW THE ROWS ARE WRITTEN. Every row builds synthetic inputs and asserts OUTPUT PIXELS.
//! A test that only asserted a decision function returned the right enum would restate the
//! code and keep passing while the picture silently went wrong. Colours are deliberately
//! vivid and unambiguous (pure red window, pure green window, blue and yellow rings,
//! magenta cursor) and assertions sample NAMED points (a centre, a ring, a gap, a corner)
//! rather than hashing the image, so a failure says WHAT moved, not merely that something
//! did.
//!
//! WHAT THE ROWS DELIBERATELY DO NOT COVER, because the platforms genuinely disagree and a
//! loose assertion would hide it:
//!
//! - CORNER ROUNDING. Linux and Windows round each window to `radius_logical * scale` with
//!   a circular mask and draw a circular ring; macOS ignores `radius_logical` entirely
//!   (SCK bakes the window's real squircle in as alpha) and dilates that alpha for the
//!   ring. Every row here therefore passes radius 0 and samples mid-edges, never corners.
//! - FROSTED GLASS. Only the Linux composite reproduces cosmic-comp's blur behind a
//!   frosted window. The synthetic windows here are fully opaque, which `glass::looks_frosted`
//!   rejects, so the Linux glass branch stays inert and the three platforms compare like
//!   with like.
//! - CURSOR RESCALE. macOS and Windows carry a `sprite_scale` in `CursorSprite` and
//!   resample the sprite onto the canvas; Linux's 3-tuple has no such field and never
//!   rescales. [`cursor_sprite`] hands the non-Linux platforms `sprite_scale == the canvas
//!   scale`, which makes their resample an exact no-op, so the POSITION maths (the shared
//!   part) is what these rows measure.
//! - Z-ORDER INPUT ORDER. See [`CAPTURED_IS_FRONT_TO_BACK`]. This one is a real divergence
//!   in the shared caller's contract, and the row below names it rather than weakening.

use crate::decoration::{BorderSpec, WindowBorders};
use crate::selection::Selection;
use image::RgbaImage;

/// One captured window: its pixels, its global logical rect, and whether it is focused.
/// Structurally the per-platform `CapturedWindow` alias.
type Captured = (RgbaImage, (i32, i32, i32, i32), bool);
/// One monitor's `(logical_pos, logical_size)`.
type OutRect = ((i32, i32), (i32, i32));

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const YELLOW: [u8; 4] = [255, 255, 0, 255];
const MAGENTA: [u8; 4] = [255, 0, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// Does `FrozenWindows::captured` arrive FRONT-to-back (topmost first) on this platform?
///
/// A REAL DIVERGENCE, named rather than smoothed over. The shared caller
/// (`app::capture_flow::frozen_captured`) hands the composite its window list in whatever
/// order the platform's own window enumeration produced, and the two orders are opposite:
///
/// - Linux (cctk toplevels) and macOS (SCK window list) deliver BACK-to-front, and their
///   composites paint in list order, so a later entry lands on top.
/// - Windows (`EnumWindows`) delivers FRONT-to-back and is kept that way on purpose for
///   the picker grid, so `platform/windows/screenshot.rs` REVERSES the list before
///   painting (DRAGON-257). There, an EARLIER entry lands on top.
///
/// So "later in the list wins" is not a portable statement. What IS portable is "the
/// window the platform's contract calls topmost is the one you see", and that is what the
/// row asserts.
#[cfg(windows)]
const CAPTURED_IS_FRONT_TO_BACK: bool = true;
#[cfg(not(windows))]
const CAPTURED_IS_FRONT_TO_BACK: bool = false;

/// A selection in global logical coordinates (never a named output or window: these rows
/// drive the region/monitor composite).
fn sel(x: i32, y: i32, width: u32, height: u32) -> Selection {
    Selection { x, y, width, height, output: None, window_id: None }
}

/// A synthetic captured window: a flat `color` rectangle whose pixel size is its logical
/// rect times `scale`, which is exactly how the composite re-derives the backing scale
/// (image width / rect width).
fn win(color: [u8; 4], rect: (i32, i32, i32, i32), scale: f32, active: bool) -> Captured {
    let w = ((rect.2 as f32 * scale).round() as u32).max(1);
    let h = ((rect.3 as f32 * scale).round() as u32).max(1);
    (RgbaImage::from_pixel(w, h, image::Rgba(color)), rect, active)
}

/// A cursor sprite in the host platform's own `CursorSprite` shape.
///
/// Linux's is a 3-tuple; macOS and Windows carry a trailing `sprite_scale`. Passing the
/// CANVAS scale as the sprite scale makes their resample factor exactly 1.0, i.e. a no-op,
/// so all three platforms overlay the same sprite at the same place and the rows measure
/// the shared position maths.
#[cfg(target_os = "linux")]
fn cursor_sprite(
    img: RgbaImage,
    pos: (i32, i32),
    hotspot: (i32, i32),
    _canvas_scale: f32,
) -> crate::screenshot::CursorSprite {
    (img, pos, hotspot)
}
#[cfg(not(target_os = "linux"))]
fn cursor_sprite(
    img: RgbaImage,
    pos: (i32, i32),
    hotspot: (i32, i32),
    canvas_scale: f32,
) -> crate::screenshot::CursorSprite {
    (img, pos, hotspot, canvas_scale)
}

/// No ring at all (both focus states width 0).
fn no_borders() -> WindowBorders {
    WindowBorders {
        active: BorderSpec { width: 0, color: BLUE },
        inactive: BorderSpec { width: 0, color: YELLOW },
    }
}

/// Run the real composite. `radius_logical` is fixed at 0 on purpose (see the module doc:
/// corner rounding is the one step the three platforms do differently).
fn composite(
    captured: Vec<Captured>,
    outs: &[OutRect],
    selection: &Selection,
    keep_transparency: bool,
    borders: WindowBorders,
    cursor: Option<&crate::screenshot::CursorSprite>,
    fallback_scale: f32,
) -> Option<RgbaImage> {
    crate::screenshot::region_windows_frozen(
        crate::screenshot::FrozenWindows {
            captured,
            out_rects: outs.to_vec(),
            fallback_scale,
        },
        selection,
        0.0,
        keep_transparency,
        borders,
        cursor,
    )
}

/// One monitor big enough to hold every selection these rows use.
fn one_monitor() -> Vec<OutRect> {
    vec![((0, 0), (800, 600))]
}

#[track_caller]
fn assert_px(img: &RgbaImage, x: u32, y: u32, want: [u8; 4], what: &str) {
    let got = img.get_pixel(x, y).0;
    assert_eq!(got, want, "{what}: pixel ({x},{y}) is {got:?}, expected {want:?}");
}

// ── The matrix ────────────────────────────────────────────────────────────────────

#[test]
fn transparency_off_makes_every_pixel_opaque_and_the_gaps_black() {
    // Two windows with a gap between them, "Preserve transparency" OFF.
    let s = sel(100, 50, 200, 120);
    let out = composite(
        vec![
            win(RED, (120, 70, 40, 30), 1.0, true),
            win(GREEN, (200, 70, 40, 30), 1.0, false),
        ],
        &one_monitor(),
        &s,
        false,
        no_borders(),
        None,
        1.0,
    )
    .expect("an on-screen selection composites");
    // Nothing may be see-through: the capture is going to be pasted somewhere that has no
    // idea what to put behind it.
    let leak = out.enumerate_pixels().find(|(_, _, p)| p.0[3] != 255);
    assert!(leak.is_none(), "transparency OFF must leave no see-through pixel, found {leak:?}");
    // The desktop between the windows is not shown; it is black.
    assert_px(&out, 80, 35, BLACK, "the gap between two windows");
    assert_px(&out, 0, 0, BLACK, "the empty margin of the selection");
    // The windows themselves are untouched.
    assert_px(&out, 20, 20, RED, "the active window");
    assert_px(&out, 100, 20, GREEN, "the inactive window");
}

#[test]
fn transparency_on_keeps_the_gaps_between_windows_fully_transparent() {
    // The same scene with "Preserve transparency" ON: everything that is not a window
    // keeps alpha 0, so the capture can be dropped onto any background.
    let s = sel(100, 50, 200, 120);
    let out = composite(
        vec![
            win(RED, (120, 70, 40, 30), 1.0, true),
            win(GREEN, (200, 70, 40, 30), 1.0, false),
        ],
        &one_monitor(),
        &s,
        true,
        no_borders(),
        None,
        1.0,
    )
    .expect("an on-screen selection composites");
    assert_px(&out, 80, 35, CLEAR, "the gap between two windows");
    assert_px(&out, 0, 0, CLEAR, "the top-left margin of the selection");
    assert_px(&out, 199, 119, CLEAR, "the bottom-right margin of the selection");
    // The windows keep their own (here opaque) pixels.
    assert_px(&out, 20, 20, RED, "the active window");
    assert_px(&out, 100, 20, GREEN, "the inactive window");
}

#[test]
fn the_active_window_gets_the_active_ring_and_the_inactive_window_the_inactive_one() {
    // Focus decides BOTH the colour and the width of the ring (DRAGON-191): a 4px blue
    // ring on the focused window, a 2px yellow one on the other.
    let s = sel(100, 50, 200, 120);
    let borders = WindowBorders {
        active: BorderSpec { width: 4, color: BLUE },
        inactive: BorderSpec { width: 2, color: YELLOW },
    };
    let out = composite(
        vec![
            win(RED, (120, 70, 40, 30), 1.0, true),
            win(GREEN, (200, 70, 40, 30), 1.0, false),
        ],
        &one_monitor(),
        &s,
        true,
        borders,
        None,
        1.0,
    )
    .expect("an on-screen selection composites");
    // Sampled at the windows' mid-height, far from any corner (see the module doc).
    // Active window: canvas x 20..=59, so its 4px ring occupies x 16..=19 and 60..=63.
    assert_px(&out, 16, 35, BLUE, "the outermost pixel of the active ring");
    assert_px(&out, 19, 35, BLUE, "the innermost pixel of the active ring");
    assert_px(&out, 15, 35, CLEAR, "one pixel beyond the active ring");
    assert_px(&out, 20, 35, RED, "the active window's own first pixel");
    assert_px(&out, 63, 35, BLUE, "the active ring on the far side");
    assert_px(&out, 64, 35, CLEAR, "one pixel beyond the active ring's far side");
    // Inactive window: canvas x 100..=139, so its 2px ring occupies x 98..=99.
    assert_px(&out, 98, 35, YELLOW, "the outermost pixel of the inactive ring");
    assert_px(&out, 99, 35, YELLOW, "the innermost pixel of the inactive ring");
    assert_px(&out, 97, 35, CLEAR, "one pixel beyond the inactive ring (it is only 2px)");
    assert_px(&out, 100, 35, GREEN, "the inactive window's own first pixel");
}

#[test]
fn a_zero_width_border_draws_no_ring_and_does_not_move_the_window() {
    // Width 0 means "no border", not "a 0px border drawn somewhere": the pixel outside the
    // window is background, and the window still starts exactly at its own offset (a ring
    // shifts the paste origin outward, so this catches a stray shift too).
    let s = sel(100, 50, 200, 120);
    let out = composite(
        vec![
            win(RED, (120, 70, 40, 30), 1.0, true),
            win(GREEN, (200, 70, 40, 30), 1.0, false),
        ],
        &one_monitor(),
        &s,
        true,
        no_borders(),
        None,
        1.0,
    )
    .expect("an on-screen selection composites");
    assert_px(&out, 19, 35, CLEAR, "the pixel where an active ring would be");
    assert_px(&out, 20, 35, RED, "the active window still starts at its own offset");
    assert_px(&out, 99, 35, CLEAR, "the pixel where an inactive ring would be");
    assert_px(&out, 100, 35, GREEN, "the inactive window still starts at its own offset");
}

#[test]
fn overlapping_windows_paint_the_topmost_over_the_one_behind_it() {
    // Two windows sharing a 20x20 patch. Which one wins is decided by the platform's list
    // contract, NOT by the position in the vec: see `CAPTURED_IS_FRONT_TO_BACK`.
    let s = sel(100, 50, 200, 120);
    let captured = vec![
        win(RED, (120, 70, 40, 40), 1.0, false),
        win(GREEN, (140, 90, 40, 40), 1.0, true),
    ];
    let (topmost, behind) =
        if CAPTURED_IS_FRONT_TO_BACK { (RED, GREEN) } else { (GREEN, RED) };
    let out =
        composite(captured, &one_monitor(), &s, true, no_borders(), None, 1.0)
            .expect("an on-screen selection composites");
    // Red covers canvas 20..=59 x 20..=59, green 40..=79 x 40..=79; they share 40..=59.
    assert_px(&out, 50, 50, topmost, "the overlap shows the topmost window");
    assert_ne!(
        out.get_pixel(50, 50).0,
        behind,
        "the window behind must not paint over the one in front"
    );
    // Away from the overlap each window is still itself.
    assert_px(&out, 25, 25, RED, "the part of the first window nothing covers");
    assert_px(&out, 75, 75, GREEN, "the part of the second window nothing covers");
}

#[test]
fn a_window_lands_at_its_own_offset_inside_the_selection() {
    // The window sits 30 logical px right and 20 down from the selection's origin, so at
    // scale 1 its pixels start at exactly (30, 20) and end at (69, 44). This is the row
    // that catches an off-by-scale or off-by-origin error in the placement maths.
    let s = sel(100, 50, 200, 120);
    let out = composite(
        vec![win(RED, (130, 70, 40, 25), 1.0, true)],
        &one_monitor(),
        &s,
        true,
        no_borders(),
        None,
        1.0,
    )
    .expect("an on-screen selection composites");
    assert_eq!((out.width(), out.height()), (200, 120), "canvas is the selection at scale 1");
    assert_px(&out, 30, 20, RED, "the window's top-left pixel");
    assert_px(&out, 29, 20, CLEAR, "one pixel left of the window");
    assert_px(&out, 30, 19, CLEAR, "one pixel above the window");
    assert_px(&out, 69, 44, RED, "the window's bottom-right pixel");
    assert_px(&out, 70, 44, CLEAR, "one pixel right of the window");
    assert_px(&out, 69, 45, CLEAR, "one pixel below the window");
}

#[test]
fn a_backing_scale_of_two_doubles_the_canvas_the_placement_and_the_ring() {
    // Scale is not passed in; it is DERIVED from the first capture (image width / logical
    // rect width), exactly as the composite does it. An 80px-wide grab of a 40pt-wide
    // window means scale 2, and then every logical quantity must double: the canvas, the
    // window's offset and size, and the ring width.
    let s = sel(100, 50, 200, 120);
    let borders = WindowBorders {
        active: BorderSpec { width: 3, color: BLUE },
        inactive: BorderSpec { width: 0, color: YELLOW },
    };
    let out = composite(
        vec![win(RED, (130, 70, 40, 25), 2.0, true)],
        &one_monitor(),
        &s,
        true,
        borders,
        None,
        1.0,
    )
    .expect("an on-screen selection composites");
    assert_eq!((out.width(), out.height()), (400, 240), "canvas is the selection at scale 2");
    // Window: offset (30, 20) logical -> (60, 40) physical, size 40x25 -> 80x50.
    assert_px(&out, 60, 40, RED, "the window's top-left pixel at scale 2");
    assert_px(&out, 139, 89, RED, "the window's bottom-right pixel at scale 2");
    // Ring: 3 logical px -> 6 physical px, so x 134..=139 is window and 140..=145 is ring.
    assert_px(&out, 140, 65, BLUE, "the innermost ring pixel at scale 2");
    assert_px(&out, 145, 65, BLUE, "the outermost ring pixel at scale 2");
    assert_px(&out, 146, 65, CLEAR, "one pixel beyond a 6px-wide scaled ring");
    assert_px(&out, 54, 65, BLUE, "the scaled ring on the near side");
    assert_px(&out, 53, 65, CLEAR, "one pixel beyond the scaled ring on the near side");
}

#[test]
fn the_cursor_sprite_lands_on_the_pointer_position_when_one_is_supplied() {
    // "Preserve mouse cursor": the windows-only composite has no pointer of its own, so
    // the launch-locked sprite is overlaid at the pointer's global position minus its
    // hotspot. Pointer (150, 90) in a selection at (100, 50) with hotspot (2, 3) puts the
    // sprite's top-left at (48, 37).
    let s = sel(100, 50, 200, 120);
    let sprite = cursor_sprite(
        RgbaImage::from_pixel(8, 8, image::Rgba(MAGENTA)),
        (150, 90),
        (2, 3),
        1.0,
    );
    let out = composite(
        vec![win(RED, (110, 60, 60, 40), 1.0, true)],
        &one_monitor(),
        &s,
        true,
        no_borders(),
        Some(&sprite),
        1.0,
    )
    .expect("an on-screen selection composites");
    assert_px(&out, 48, 37, MAGENTA, "the sprite's top-left, hotspot subtracted");
    assert_px(&out, 55, 44, MAGENTA, "the sprite's bottom-right");
    assert_px(&out, 47, 37, RED, "one pixel left of the sprite is still the window");
    assert_px(&out, 56, 37, RED, "one pixel right of the sprite is still the window");
}

#[test]
fn without_a_cursor_sprite_nothing_is_drawn_where_the_pointer_was() {
    // The same scene with the toggle off: not a single cursor pixel anywhere.
    let s = sel(100, 50, 200, 120);
    let out = composite(
        vec![win(RED, (110, 60, 60, 40), 1.0, true)],
        &one_monitor(),
        &s,
        true,
        no_borders(),
        None,
        1.0,
    )
    .expect("an on-screen selection composites");
    assert_px(&out, 48, 37, RED, "where the sprite would have been");
    let stray = out.enumerate_pixels().find(|(_, _, p)| p.0 == MAGENTA);
    assert!(stray.is_none(), "no cursor supplied means no cursor drawn, found {stray:?}");
}

#[test]
fn a_selection_dragged_off_the_right_edge_is_trimmed_to_the_on_screen_union() {
    // A region dragged past the screen edge must not keep the empty void beyond it: the
    // result is cropped to where the selection actually overlaps a monitor.
    let s = sel(700, 550, 200, 120);
    let out = composite(
        vec![win(RED, (700, 550, 60, 40), 1.0, true)],
        &one_monitor(),
        &s,
        true,
        no_borders(),
        None,
        1.0,
    )
    .expect("a partly on-screen selection still composites");
    assert_eq!((out.width(), out.height()), (100, 50), "trimmed to the 100x50 on-screen part");
    assert_px(&out, 0, 0, RED, "the window still starts at the selection's origin");
    assert_px(&out, 59, 39, RED, "the window's far corner survives the trim");
    assert_px(&out, 60, 39, CLEAR, "past the window, inside the kept area");
}

#[test]
fn a_selection_hanging_off_the_top_left_keeps_only_its_on_screen_remainder() {
    // The other trim direction, which also has to SHIFT the crop origin: the first 50
    // columns and 20 rows of the selection are off-screen, so the kept area starts there.
    let s = sel(-50, -20, 200, 120);
    let out = composite(
        vec![win(RED, (0, 0, 60, 40), 1.0, true)],
        &one_monitor(),
        &s,
        true,
        no_borders(),
        None,
        1.0,
    )
    .expect("a partly on-screen selection still composites");
    assert_eq!((out.width(), out.height()), (150, 100), "trimmed to the 150x100 on-screen part");
    assert_px(&out, 0, 0, RED, "the window at the origin lands at the crop's origin");
    assert_px(&out, 59, 39, RED, "the window's far corner");
    assert_px(&out, 60, 0, CLEAR, "past the window");
}

#[test]
fn a_selection_entirely_off_every_monitor_produces_no_image() {
    // Nothing overlaps a monitor, so there is no honest picture to return.
    let s = sel(2000, 2000, 100, 100);
    assert!(
        composite(
            vec![win(RED, (2000, 2000, 50, 50), 1.0, true)],
            &one_monitor(),
            &s,
            true,
            no_borders(),
            None,
            1.0,
        )
        .is_none(),
        "a selection beyond every monitor has nothing to show"
    );
    // Same answer when there are no monitors at all to intersect.
    let s = sel(100, 50, 200, 120);
    assert!(
        composite(vec![], &[], &s, true, no_borders(), None, 1.0).is_none(),
        "no monitors means no on-screen union"
    );
}

#[test]
fn a_selection_with_no_area_produces_no_image() {
    // A zero-width (or zero-height) drag cannot make a canvas.
    let s = sel(100, 50, 0, 120);
    assert!(composite(vec![], &one_monitor(), &s, true, no_borders(), None, 1.0).is_none());
    let s = sel(100, 50, 200, 0);
    assert!(composite(vec![], &one_monitor(), &s, true, no_borders(), None, 1.0).is_none());
}

#[test]
fn an_empty_window_set_still_yields_a_canvas_sized_by_the_fallback_scale() {
    // Nothing intersected the selection (an empty patch of desktop). With no capture to
    // derive the scale from, the caller's fallback scale sizes the canvas, and the
    // wallpaper-OFF promise is still kept: an empty black rectangle, not the desktop.
    let s = sel(100, 50, 200, 120);
    let out = composite(vec![], &one_monitor(), &s, false, no_borders(), None, 2.0)
        .expect("an empty selection still composites a background");
    assert_eq!((out.width(), out.height()), (400, 240), "sized by the fallback scale");
    assert_px(&out, 200, 120, BLACK, "an empty wallpaper-off region is black");
    // And transparent instead when transparency is ON.
    let out = composite(vec![], &one_monitor(), &s, true, no_borders(), None, 2.0)
        .expect("an empty selection still composites a background");
    assert_px(&out, 200, 120, CLEAR, "an empty transparency-on region is empty");
}
