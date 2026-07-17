//! Window-decoration seam: how a captured single window is bordered to look the way
//! the user wants ON SCREEN.
//!
//! DRAGON-191 GREATLY simplified this. The prior model read the environment's own
//! window-border config to RECONSTRUCT it (JankyBorders' `bordersrc` on macOS, the
//! COSMIC theme's active/inactive hint on Linux) — a lot of per-desktop-environment
//! code. That is gone. The border is now TWO explicit user-configured borders (an
//! Active one for the focused / single-window capture and an Inactive one for
//! unfocused windows in a region/monitor composite), each a colour + width, drawn by
//! the portable alpha-dilation mechanism (`compose::add_border` /
//! `compose::add_border_native_corners`). Adding a new desktop (Windows, another
//! Linux) needs NO platform-specific border logic now.
//!
//! The one theming read that survives is the system ACCENT colour, used ONLY as the
//! DEFAULT for the Active border when the user hasn't pinned a custom colour
//! (`active_border_color == None`). That is a theming read (the same accent the app
//! already reads elsewhere), NOT a border reconstruction — it's behind [`accent_rgba`],
//! one portable function with per-OS branches.

/// One border's draw parameters: a `width` in LOGICAL px (0 = no border) and an RGBA
/// `color`. Both active and inactive captures use this shape; the compose path scales
/// `width` by the captured image's own per-display backing scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BorderSpec {
    /// Border width in LOGICAL px (0-10; 0 = no border).
    pub width: u32,
    /// Border colour, RGBA bytes (the order `image::Rgba` uses).
    pub color: [u8; 4],
}

impl BorderSpec {
    /// As the `Option<(f32, [u8;4])>` shape the compose path takes: `None` for a
    /// zero-width border (draw nothing), else the width (as `f32` logical px) + colour.
    pub fn to_compose(self) -> Option<(f32, [u8; 4])> {
        if self.width == 0 {
            None
        } else {
            Some((self.width as f32, self.color))
        }
    }
}

/// The Active and Inactive border specs a capture draws: the focused window (and every
/// single-window capture) gets [`Self::active`]; unfocused windows in a region/monitor
/// composite get [`Self::inactive`]. Resolved from the persisted user config, with the
/// Active colour following the system accent when the user hasn't pinned one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowBorders {
    pub active: BorderSpec,
    pub inactive: BorderSpec,
}

impl WindowBorders {
    /// Resolve the two borders from the persisted config:
    /// - `active_color`: `None` means "follow the system accent" (resolved HERE via
    ///   [`accent_rgba`]); `Some` is a user-pinned custom colour.
    /// - Widths are clamped to 0-10 px (0 = no border) for safety against a
    ///   hand-edited config.
    pub fn resolve(
        active_color: Option<[u8; 4]>,
        active_width: u32,
        inactive_color: [u8; 4],
        inactive_width: u32,
    ) -> Self {
        WindowBorders {
            active: BorderSpec {
                width: active_width.min(10),
                color: active_color.unwrap_or_else(accent_rgba),
            },
            inactive: BorderSpec {
                width: inactive_width.min(10),
                color: inactive_color,
            },
        }
    }

    /// The border for a window given its focus state: Active when `active`, else
    /// Inactive. A single-window capture passes `true` (the captured window is the
    /// active one).
    pub fn for_active(&self, active: bool) -> BorderSpec {
        if active {
            self.active
        } else {
            self.inactive
        }
    }
}

/// The app's ACCENT colour as RGBA bytes — the DEFAULT for the Active border when the
/// user hasn't pinned a custom colour. ONE portable function with per-OS branches, all
/// resolving to the SAME accent the app's own UI shows (General -> Appearance), NOT the
/// host OS's system accent:
///
/// - **macOS**: the live libcosmic theme accent (`cosmic::theme::active()` through the
///   app's accent seam), matching the colour the settings sliders/swatches paint with.
///   macOS has no on-disk COSMIC theme dir to read like Linux, and its
///   `NSColor.controlAccentColor` is a DIFFERENT colour from the app's theme accent, so
///   reading the active theme is what makes "follow accent" match what the user sees.
/// - **Linux**: the COSMIC theme accent read off disk — the SAME source the app
///   already reads for the window hint (`crate::app::theme::active_hint_color`, which
///   returns `window_hint` when set else the accent). No new dependency.
/// - **Other**: the app's historical lavender fallback.
pub fn accent_rgba() -> [u8; 4] {
    #[cfg(target_os = "macos")]
    {
        // The teal (or whatever) accent the UI actually paints, via the app's accent
        // seam over the live theme — never the macOS system accent.
        let c = crate::app::theme::accent(&cosmic::theme::active());
        [
            (c.r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (c.g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (c.b.clamp(0.0, 1.0) * 255.0).round() as u8,
            255,
        ]
    }
    #[cfg(target_os = "linux")]
    {
        crate::app::theme::active_hint_color()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        [151, 125, 236, 255]
    }
}

/// Derive a captured window's REAL corner radius (in the image's own PHYSICAL
/// pixels) from its top-left alpha corner. macOS SCK delivers each window with its
/// native rounded corners baked in as alpha (corner pixels transparent, curving to
/// opaque along an arc), so the radius is exactly where that arc meets the top edge:
/// walk the top row inward until alpha first crosses opaque, and independently walk
/// the left column down until it crosses opaque; the arc is a quarter-circle so both
/// give the same radius, and we take the max (robust to a single-axis nick).
///
/// Returns `None` when the window has no rounded corner to read (square corners:
/// the very first pixel is already opaque, so the walk yields 0) or the image is
/// too small - the caller then keeps its configured fallback radius. Pure pixel
/// math (a synthetic corner is unit-testable), used to make the reconstructed
/// shadow footprint hug the window's actual corner (DRAGON-186 Phase 5c). Platform-
/// agnostic: also the gutter-trim guard for Linux screencopy captures (DRAGON-190
/// extended); returns `None` for a square/opaque window, so a fallback of 0 is used.
pub fn corner_radius_from_alpha(img: &image::RgbaImage) -> Option<f32> {
    let (w, h) = (img.width(), img.height());
    if w < 4 || h < 4 {
        return None;
    }
    // Alpha is "opaque enough" at the half-way point of the anti-aliased edge.
    let opaque = |x: u32, y: u32| img.get_pixel(x, y)[3] >= 128;
    // The corner is transparent; the arc meets the top edge `radius` px in from the
    // left, and the left edge `radius` px down from the top. Scan a bounded window
    // (a corner radius never approaches half the window).
    let limit = (w.min(h) / 2).min(64);
    // Top edge: first opaque x along row 0.
    let top_run = (0..limit).take_while(|&x| !opaque(x, 0)).count() as u32;
    // Left edge: first opaque y along column 0.
    let left_run = (0..limit).take_while(|&y| !opaque(0, y)).count() as u32;
    let r = top_run.max(left_run);
    // A fully-opaque first pixel (square corners) yields 0 - no radius to hug.
    if r == 0 {
        None
    } else {
        Some(r as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn border_spec_to_compose_maps_zero_width_to_none() {
        // Width 0 = no border -> None (the compose path draws nothing).
        assert_eq!(BorderSpec { width: 0, color: [1, 2, 3, 255] }.to_compose(), None);
        // A real width carries its (f32) width + colour.
        assert_eq!(
            BorderSpec { width: 3, color: [9, 8, 7, 255] }.to_compose(),
            Some((3.0, [9, 8, 7, 255]))
        );
    }

    #[test]
    fn resolve_follows_accent_when_active_color_none() {
        // active_color None -> the Active border colour IS the resolved accent.
        let borders = WindowBorders::resolve(None, 3, [65, 69, 80, 255], 1);
        assert_eq!(borders.active.color, accent_rgba());
        assert_eq!(borders.active.width, 3);
        assert_eq!(borders.inactive.color, [65, 69, 80, 255]);
        assert_eq!(borders.inactive.width, 1);
    }

    #[test]
    fn resolve_uses_custom_active_color_when_some() {
        // A pinned custom colour overrides the accent.
        let custom = [10, 20, 30, 255];
        let borders = WindowBorders::resolve(Some(custom), 5, [1, 1, 1, 255], 2);
        assert_eq!(borders.active.color, custom);
        assert_ne!(borders.active.color, accent_rgba());
    }

    #[test]
    fn resolve_clamps_widths_to_ten() {
        // A hand-edited config with an out-of-range width is clamped to 0-10.
        let borders = WindowBorders::resolve(Some([0; 4]), 99, [0; 4], 42);
        assert_eq!(borders.active.width, 10);
        assert_eq!(borders.inactive.width, 10);
        // Zero stays zero (no border).
        let z = WindowBorders::resolve(Some([0; 4]), 0, [0; 4], 0);
        assert_eq!((z.active.width, z.inactive.width), (0, 0));
    }

    #[test]
    fn for_active_picks_active_or_inactive() {
        let borders = WindowBorders {
            active: BorderSpec { width: 3, color: [1, 2, 3, 255] },
            inactive: BorderSpec { width: 1, color: [4, 5, 6, 255] },
        };
        assert_eq!(borders.for_active(true), borders.active);
        assert_eq!(borders.for_active(false), borders.inactive);
    }

    #[test]
    fn accent_rgba_is_opaque() {
        // Whatever the platform resolves, the accent default is a full-alpha colour so
        // the drawn ring isn't accidentally see-through.
        assert_eq!(accent_rgba()[3], 255);
    }

    #[test]
    fn corner_radius_from_alpha_reads_a_synthetic_arc() {
        // Build a 60x60 window with a rounded-rect top-left corner of radius 12 (the
        // SCK native-corner alpha shape): a pixel is transparent iff it's OUTSIDE the
        // rounded rectangle, i.e. inside the top-left r×r corner box AND beyond the arc
        // centred at (r, r). The rounded rect meets its straight top edge at x = r, so
        // the top row is transparent for x in [0, r) and the derivation recovers 12.
        let r = 12u32;
        let mut img = image::RgbaImage::from_pixel(60, 60, image::Rgba([200, 100, 50, 255]));
        let rf = r as f32;
        for y in 0..r {
            for x in 0..r {
                let dx = rf - x as f32;
                let dy = rf - y as f32;
                if dx * dx + dy * dy > rf * rf {
                    img.put_pixel(x, y, image::Rgba([0, 0, 0, 0])); // outside the rounded rect
                }
            }
        }
        let got = corner_radius_from_alpha(&img).expect("a rounded corner has a radius");
        assert!((got - rf).abs() <= 1.0, "derived radius {got} within 1px of {r}");
    }

    #[test]
    fn corner_radius_from_alpha_none_for_square_corner() {
        // A fully-opaque (square-cornered) window has no arc: the first pixel is
        // already opaque, so the walk yields 0 -> None (caller keeps its fallback).
        let img = image::RgbaImage::from_pixel(40, 40, image::Rgba([1, 2, 3, 255]));
        assert_eq!(corner_radius_from_alpha(&img), None);
        // Too-small images can't carry a meaningful corner -> None.
        let tiny = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        assert_eq!(corner_radius_from_alpha(&tiny), None);
    }
}
