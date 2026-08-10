//! Portable Windows cursor-capture geometry (DRAGON-567): the PURE decisions behind
//! the still path's cursor overlay, with no Win32 in sight so all of it is unit-tested
//! on Linux, the `win_diag` pattern. The effectful reads (`GetCursorInfo`,
//! `GetIconInfo`, `GetDpiForMonitor`) stay in `platform/windows/cursor.rs`.
//!
//! # The invariant these functions guard
//!
//! A captured cursor must land in the still at exactly its ON-SCREEN physical size.
//! What the screen shows IS the cursor bitmap the system currently has loaded, at that
//! bitmap's own pixel size: Windows bakes every size decision into the LOADED image:
//! the display scale of the pointer's monitor (a 150% display loads the arrow at 48px,
//! 200% at 64px, reloading as the pointer crosses monitors of different scale) and the
//! accessibility pointer-size slider (1-15) on top of that. So the `GetIconInfo` bitmap
//! our sprite is rendered from (`DrawIconEx` at the bitmap's native dims) is the
//! on-screen truth, already in physical pixels, on every monitor of a mixed-DPI setup
//! and at every accessibility size. Drawing it 1:1 onto the physical capture canvas is
//! correct by construction; there is nothing to rescale.
//!
//! # The dead end this replaces (the DRAGON-567 bug)
//!
//! `platform/windows/cursor.rs` used to stamp the `CursorSprite` tuple's backing scale
//! as `96/dpi` of the pointer's monitor, on the premise that the sprite was a 96-DPI
//! "base" asset the composite must upscale by `dpi/96` to reach physical size. The
//! premise is false: under Per-Monitor-Aware-V2 (set before any measurement,
//! `platform/windows/dpi.rs`) nothing is virtualized and the bitmap is already
//! display-scaled. The capture canvases are physical with scale ~1.0 (`monitors.rs`:
//! `scale = captured_px / logical_size ≈ 1.0`), so `cursor_for_canvas` multiplied an
//! already-physical sprite by `dpi/96` a SECOND time: a 150% display captured the
//! pointer 1.5x too large per axis, 200% doubled it (4x the area, the reported
//! "incredibly large"), and an accessibility-enlarged pointer overscaled on top of its
//! honest size. At 100% (`dpi == 96`) the factor is 1.0, which is why a dev box never
//! saw it. The on-screen overlay indicator met the same wrong premise from the other
//! side (DRAGON-448, `app/overlay/mod.rs`'s Windows `cursor_sprite_scale` arm): it
//! already hardcodes `1.0` and ignores the tuple's scale field, so it agreed with the
//! honest stamping before the producer did; with the producer fixed, the mac-style
//! pass-through reading would now agree too.
//!
//! Why the DPI parameter stays: [`sprite_backing_scale`] takes the pointer monitor's
//! DPI, the very input the old code mixed in, and the tests pin that it does NOT
//! participate in the answer. If the owner's live mixed-DPI verification ever proves a
//! correction is needed after all, the seam already carries the reading.

/// The backing scale (pixels-per-point) stamped into a captured Windows
/// `CursorSprite`.
///
/// Pure, unit-tested on every platform. Always `1.0`: in the Windows physical-pixel
/// model one sprite pixel IS one point, whatever the pointer monitor's DPI, because
/// the `GetIconInfo` bitmap is already loaded at its on-screen physical size (see the
/// module doc). `pointer_monitor_dpi` is accepted, and deliberately unused, so the
/// no-double-scale decision is pinned against the input the old `96/dpi` stamping
/// consumed.
#[cfg_attr(not(windows), allow(dead_code))] // producer (platform/windows/cursor.rs) is cfg(windows); the tests run everywhere
pub fn sprite_backing_scale(pointer_monitor_dpi: u32) -> f32 {
    // Measured, available, and decided against; see the module doc.
    let _ = pointer_monitor_dpi;
    1.0
}

/// The draw geometry for a captured cursor sprite on a capture canvas: the sprite's
/// draw size in canvas pixels and the hotspot offset inside it, the numbers
/// `cursor_for_canvas` (platform/windows/screenshot.rs) resamples to.
///
/// Pure, unit-tested on every platform. `sprite` is the sprite's pixel size, `hotspot`
/// in sprite pixels, `sprite_scale` its backing scale ([`sprite_backing_scale`] on
/// Windows), `canvas_scale` the canvas's physical-per-logical scale (~1.0 in the
/// Windows physical model). The resample factor is `canvas_scale / sprite_scale`, so
/// with the honest `1.0` stamping every real capture is a 1:1 no-op; a degenerate
/// `sprite_scale <= 0` means "unknown" and leaves the sprite alone (the tolerance the
/// mac twin's `cursor_resize_factor` established, DRAGON-156).
#[cfg_attr(not(windows), allow(dead_code))] // consumer (platform/windows/screenshot.rs) is cfg(windows); the tests run everywhere
pub fn draw_placement(
    sprite: (u32, u32),
    hotspot: (i32, i32),
    sprite_scale: f32,
    canvas_scale: f32,
) -> ((u32, u32), (i32, i32)) {
    let factor = if sprite_scale <= 0.0 { 1.0 } else { canvas_scale / sprite_scale };
    let w = ((sprite.0 as f32 * factor).round() as u32).max(1);
    let h = ((sprite.1 as f32 * factor).round() as u32).max(1);
    let hx = (hotspot.0 as f32 * factor).round() as i32;
    let hy = (hotspot.1 as f32 * factor).round() as i32;
    ((w, h), (hx, hy))
}

#[cfg(test)]
mod sprite_backing_scale_tests {
    use super::*;

    #[test]
    fn unity_at_100_percent() {
        assert_eq!(sprite_backing_scale(96), 1.0);
    }

    #[test]
    fn dpi_does_not_participate_at_150_and_200_percent() {
        // The old stamping answered 96/144 and 96/192 here, which is exactly the
        // double-scale: the bitmap is already display-scaled.
        assert_eq!(sprite_backing_scale(144), 1.0);
        assert_eq!(sprite_backing_scale(192), 1.0);
    }

    #[test]
    fn mixed_dpi_monitors_agree() {
        // Every monitor of a mixed-DPI setup stamps the same scale: the system reloads
        // the pointer bitmap per monitor, so the bitmap is the on-screen truth
        // wherever the pointer sits.
        assert_eq!(sprite_backing_scale(96), sprite_backing_scale(144));
        assert_eq!(sprite_backing_scale(192), sprite_backing_scale(120));
    }
}

#[cfg(test)]
mod draw_placement_tests {
    use super::*;

    /// The whole production pipeline for a pointer on a `dpi` monitor: the sprite the
    /// system loaded (`bitmap` px), stamped by [`sprite_backing_scale`], drawn onto
    /// the physical canvas (scale 1.0).
    fn captured(bitmap: (u32, u32), hotspot: (i32, i32), dpi: u32) -> ((u32, u32), (i32, i32)) {
        draw_placement(bitmap, hotspot, sprite_backing_scale(dpi), 1.0)
    }

    #[test]
    fn at_100_percent_the_draw_rect_is_the_bitmap() {
        assert_eq!(captured((32, 32), (2, 3), 96), ((32, 32), (2, 3)));
    }

    #[test]
    fn at_150_and_200_percent_nothing_double_scales() {
        // 150%: the system loads the pointer at 48px; captured at 48px, not the old
        // stamping's 72.
        assert_eq!(captured((48, 48), (6, 9), 144), ((48, 48), (6, 9)));
        // 200%: loaded at 64px; captured at 64px, not 128.
        assert_eq!(captured((64, 64), (8, 12), 192), ((64, 64), (8, 12)));
    }

    #[test]
    fn mixed_dpi_is_one_to_one_in_both_directions() {
        // Pointer on the scaled monitor of a 100% + 200% pair: the reloaded 64px
        // bitmap draws 64px.
        assert_eq!(captured((64, 64), (8, 12), 192), ((64, 64), (8, 12)));
        // Pointer on the 100% monitor of the same pair: the reloaded 32px bitmap
        // draws 32px; the other monitor's scale never leaks in.
        assert_eq!(captured((32, 32), (4, 6), 96), ((32, 32), (4, 6)));
    }

    #[test]
    fn an_accessibility_enlarged_cursor_keeps_its_true_size() {
        // Pointer size 3 at 100%: a ~96px bitmap, honestly big on screen, captured
        // at 96px.
        assert_eq!(captured((96, 96), (12, 18), 96), ((96, 96), (12, 18)));
        // Pointer size 3 on a 150% display: ~144px on screen, captured at 144px (the
        // old stamping made this 216px).
        assert_eq!(captured((144, 144), (18, 27), 144), ((144, 144), (18, 27)));
    }

    #[test]
    fn a_genuine_scale_mismatch_still_resamples_with_the_hotspot() {
        // The machinery keeps the mac-shaped general contract: a sprite whose backing
        // scale really is half the canvas doubles, the hotspot riding the same factor.
        assert_eq!(draw_placement((16, 16), (2, 3), 0.5, 1.0), ((32, 32), (4, 6)));
    }

    #[test]
    fn a_degenerate_sprite_scale_means_unknown_and_leaves_the_sprite_alone() {
        assert_eq!(draw_placement((32, 32), (2, 3), 0.0, 1.5), ((32, 32), (2, 3)));
        assert_eq!(draw_placement((32, 32), (2, 3), -1.0, 1.5), ((32, 32), (2, 3)));
    }

    #[test]
    fn the_draw_size_never_collapses_to_zero() {
        assert_eq!(draw_placement((1, 1), (0, 0), 4.0, 1.0).0, (1, 1));
    }
}
