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
//! The one theming read that survives is the RESOLVED ACCENT colour, used ONLY as the
//! DEFAULT for the Active border when the user hasn't pinned a custom colour
//! (`active_border_color == None`). That is a theming read (the same accent the app's
//! own UI paints), NOT a border reconstruction — it's behind [`accent_rgba`], one
//! portable function reading the applied theme's accent, so the border follows the
//! full appearance chain (System Default -> manual accent -> fallback) on every OS
//! (DRAGON-239).

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

/// The app's RESOLVED ACCENT colour as RGBA bytes — the DEFAULT for the Active border
/// when the user hasn't pinned a custom colour. ONE portable function on EVERY OS: the
/// live applied theme's accent (`cosmic::theme::active()` through the app's accent
/// seam), i.e. exactly the colour the app's own UI paints (General -> Appearance).
/// Because [`apply_appearance`](crate::app::theme::apply_appearance) has already
/// resolved the whole appearance CHAIN into the applied theme, this follows it by
/// construction on all platforms and all modes (DRAGON-239):
///   1. System Default  -> the system accent (Windows registry / Linux COSMIC / macOS
///      libcosmic default);
///   2. else a manually-pinned accent -> that colour;
///   3. else -> the base theme's default accent.
///
/// Previously this branched per-OS and, off macOS, read the raw SYSTEM accent (Linux)
/// or a hardcoded lavender (Windows/other), so the border ignored a manual accent
/// override. Reading the applied theme fixes every platform at once.
///
/// A headless CLI process never applies the global theme, so `active()` there is the
/// default; the composite diagnostic instead resolves the accent from persisted
/// settings via [`crate::app::theme::resolved_appearance_accent_rgba`] (the same value
/// this returns once the app is running).
pub fn accent_rgba() -> [u8; 4] {
    crate::app::theme::color_to_rgba(crate::app::theme::accent(&cosmic::theme::active()))
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

// ─── The portable decoration assembly (DRAGON-463) ───────────────────────────
//
// Until this ticket, the steps that turn a captured window into the picture the user
// asked for — keep-or-flatten transparency, ring, shadow-or-padding, the 3-way
// backdrop, the cursor overlay — were written out THREE times, once per platform
// (`platform/linux/native/screenshot.rs`, `platform/mac/screenshot.rs`,
// `platform/windows/screenshot.rs`). They had drifted only slightly, which is worse
// than drifting a lot: nobody could see it.
//
// That duplication is why the capture-extras behaviour could not be tested for real.
// A pixel test written against one platform proved nothing about the others, and a new
// compositor would have written a fourth copy that no test could see. Sharing the
// assembly is the precondition for the matrix tests, not a tidy-up.
//
// What deliberately did NOT move: how each platform obtains its window pixels, its
// wallpaper pixels, and its corner shape. Those are genuinely native.

/// How a captured window's corners are already shaped, and therefore what the
/// decoration must do about them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerStyle {
    /// The capture has SQUARE corners and must be rounded to this radius in physical
    /// px. Linux and Windows: the compositor hands back the full rectangle.
    Rounded(u32),
    /// The capture already carries its native rounded-corner alpha, so rounding again
    /// would carve a second ring off real pixels. macOS: SCK delivers the window's own
    /// squircle. The border is drawn by dilating that alpha instead of by a circle.
    ///
    /// Only macOS SELECTS this. Gated honestly rather than blanket-allowed: if a future
    /// compositor starts delivering native corners, deleting one attribute is the whole
    /// change. (The window-decoration matrix will construct it everywhere once it lands,
    /// which is the point of un-gating the native-corner helpers in `compose`.)
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Native,
}

/// What sits behind the window in the finished picture. The three arms ARE the
/// wallpaper/transparency matrix; see [`backdrop_for`].
pub enum Backdrop {
    /// Composite over these pixels. The platform prepares them (Linux re-renders the
    /// wallpaper file as its compositor placed it; macOS and Windows pass their own
    /// grab), because where wallpaper pixels come from is native. What to DO with them
    /// is not.
    ///
    /// No platform SELECTS this today: all three own a `composite_over_wallpaper` that
    /// computes its own crop and does its own `over`, so they never hand prepared pixels
    /// back. It is the seam's honest expression of the three-way rule and the matrix
    /// tests assert through it, so it is gated to test builds rather than deleted.
    #[cfg_attr(not(test), allow(dead_code))]
    Behind(image::RgbaImage),
    /// Opaque flat black.
    Black,
    /// Keep the window's own alpha, plus whatever halo the shadow drew.
    Transparent,
}

/// Which backdrop the wallpaper + transparency toggles ask for.
///
/// The rule, and the reason it is not two independent bits: turning the wallpaper off
/// only asks "do not show the desktop behind this window". It does not say what to show
/// instead, and that answer depends on transparency. With transparency ON the honest
/// answer is nothing (the window keeps its own alpha); with it OFF there is no alpha to
/// keep, so the capture needs an opaque backing and gets black.
///
/// Pure, so the matrix can assert it directly and the pixel tests can assert what each
/// arm then produces.
#[must_use]
pub fn backdrop_for(wallpaper: bool, transparency: bool) -> BackdropKind {
    if wallpaper {
        BackdropKind::Wallpaper
    } else if !transparency {
        BackdropKind::Black
    } else {
        BackdropKind::Transparent
    }
}

/// [`backdrop_for`]'s answer, before the platform supplies any wallpaper pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackdropKind {
    Wallpaper,
    Black,
    Transparent,
}

/// Everything the decoration needs that is NOT platform-specific.
pub struct DecorationOpts {
    /// Keep the window's own alpha. False flattens it to opaque.
    pub keep_transparency: bool,
    /// See [`CornerStyle`].
    pub corners: CornerStyle,
    /// The ring: LOGICAL width + colour, already resolved to the active or inactive
    /// spec by the caller (`WindowBorders::for_active`). `None` draws no ring.
    pub border: Option<(f32, [u8; 4])>,
    /// Draw the reconstructed drop shadow into the margin.
    pub shadow: bool,
    /// Shadow tuning for a dark theme.
    pub dark: bool,
    /// TOTAL margin from the window edge, LOGICAL px. The ring lives INSIDE it, so the
    /// transparent padding is this minus the border width.
    pub pad_logical: f32,
    /// The captured display's backing scale, for converting the logical values above.
    pub scale: f32,
    /// Floor for the scaled border width, in physical px.
    ///
    /// A DIVERGENCE, preserved rather than unified (DRAGON-463 step 2 is byte-identity).
    /// macOS clamps the scaled ring to at least 1px so a configured border can never
    /// round away to nothing; Linux and Windows do not, so at a sub-1.0 scale their ring
    /// can vanish. Worth settling deliberately, but not inside a refactor.
    pub min_border_px: u32,
}

/// A decorated window plus the measurements its caller still needs.
pub struct Decorated {
    pub img: image::RgbaImage,
    /// Border + padding, LOGICAL px: the offset from the canvas edge to the window
    /// content. The cursor overlay and the wallpaper crop both position from this.
    pub total_margin_logical: f32,
    /// The window content's physical size inside the canvas, before margins.
    ///
    /// Only Linux reads it, for the frosted-glass footprint; the other platforms have no
    /// frosting, so they construct `Decorated` without ever consuming this. Gated to say
    /// exactly that rather than blanket-allowed.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub content: (u32, u32),
    /// Radius of the outer (post-ring) shape, for the shadow footprint.
    ///
    /// `decorate` uses it internally; no CALLER needs it yet, because each platform's
    /// wallpaper composite derives its own footprint. Kept because it is the only way a
    /// caller could reproduce the shadow's shape, and asserted by the matrix tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub outer_radius: u32,
}

/// Turn a captured window into a decorated canvas: transparency, ring, then shadow or
/// plain padding. The backdrop and the cursor are applied separately
/// ([`apply_backdrop`], [`overlay_cursor`]) because both need pixels the platform owns.
///
/// This is the shared half of what were three near-identical bodies. Read the module
/// note above for what stayed native and why.
pub fn decorate(img: image::RgbaImage, opts: &DecorationOpts) -> Decorated {
    let (fin, base_radius) = match opts.corners {
        CornerStyle::Rounded(r) => {
            (crate::compose::finish_window(img, r, opts.keep_transparency), r)
        }
        // No radius of our own: the alpha already carries the real corner shape.
        CornerStyle::Native => {
            (crate::compose::finish_window_native_corners(img, opts.keep_transparency), 0)
        }
    };
    let (content_w, content_h) = (fin.width(), fin.height());
    let bw_logical = opts.border.map(|(w, _)| w).unwrap_or(0.0);
    let (bordered, outer_radius) = match opts.border {
        Some((_, color)) => {
            let bw = ((bw_logical * opts.scale).round() as u32).max(opts.min_border_px);
            let ringed = match opts.corners {
                CornerStyle::Rounded(r) => {
                    crate::compose::add_border(fin, bw, color, r + bw)
                }
                // Dilate the window's own alpha so the ring hugs the real corner shape
                // concentrically; a circular ring bulges past a squircle's edge.
                CornerStyle::Native => {
                    crate::compose::add_border_native_corners(fin, bw, color)
                }
            };
            (ringed, base_radius + bw)
        }
        None => (fin, base_radius),
    };
    // The ring is drawn INSIDE the user's padding, so it subtracts from the margin.
    // Without this, turning a border on would silently grow the whole canvas.
    let margin_logical = (opts.pad_logical - bw_logical).max(0.0);
    let margin_px = (margin_logical * opts.scale).round() as u32;
    // The shadow draws into whatever margin exists and is clipped when there is not
    // room (padding off). The canvas is never grown to fit it.
    let out = if opts.shadow {
        crate::compose::with_shadow(bordered, margin_px, outer_radius, opts.scale, opts.dark)
    } else if margin_px > 0 {
        crate::compose::pad_transparent(bordered, margin_px)
    } else {
        bordered
    };
    Decorated {
        img: out,
        total_margin_logical: bw_logical + margin_logical,
        content: (content_w, content_h),
        outer_radius,
    }
}

/// Apply the backdrop chosen by [`backdrop_for`].
pub fn apply_backdrop(img: image::RgbaImage, backdrop: Backdrop) -> image::RgbaImage {
    match backdrop {
        Backdrop::Behind(bg) => crate::compose::over(bg, &img),
        Backdrop::Black => crate::compose::on_black(img),
        Backdrop::Transparent => img,
    }
}

/// Where the cursor sprite lands on a decorated canvas.
///
/// Pure, and separate from the drawing, because the offset is the part that was subtly
/// re-derived on each platform: the canvas extends past the window content by the total
/// margin, so a sprite positioned from the canvas origin drifts by exactly that much.
///
/// `pointer` is in GLOBAL logical coords, `sel_origin` is the window's logical origin,
/// and `hotspot` is the sprite's hotspot in physical px.
///
/// The three are taken as PAIRS rather than six scalars because that is how every caller
/// already holds them (`CursorSprite` carries `(i32, i32)` hotspots on all three
/// platforms), so the call site does no unpacking and no casting. Six loose `i32`s also
/// put this one argument over clippy's limit, which was a fair complaint about a
/// signature nobody could read.
#[must_use]
pub fn cursor_offset(
    pointer: (i32, i32),
    sel_origin: (i32, i32),
    total_margin_logical: f32,
    scale: f32,
    hotspot: (i32, i32),
) -> (i64, i64) {
    let off = (total_margin_logical * scale).round() as i64;
    let px = ((pointer.0 - sel_origin.0) as f32 * scale).round() as i64 + off - hotspot.0 as i64;
    let py = ((pointer.1 - sel_origin.1) as f32 * scale).round() as i64 + off - hotspot.1 as i64;
    (px, py)
}

/// Whether the launch-locked pointer lies within the picked window's logical rect.
///
/// The window-capture cursor overlay is gated on this (DRAGON-213): the decorated canvas
/// extends past the window content, so relying on canvas clipping alone floated a cursor
/// beside the window whenever the pointer merely happened to be nearby at launch.
#[must_use]
pub fn cursor_over_window(gx: i32, gy: i32, sel_x: i32, sel_y: i32, w: u32, h: u32) -> bool {
    gx >= sel_x && gx < sel_x + w as i32 && gy >= sel_y && gy < sel_y + h as i32
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
