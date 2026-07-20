//! Pure `image::RgbaImage` compositing and manipulation helpers shared by the
//! capture and app layers: corner rounding, drop shadows, borders, padding, and
//! straight-alpha compositing. Nothing here touches Wayland, screencopy, or
//! ffmpeg; it's all image-in / image-out math.

use image::RgbaImage;

/// Force every pixel opaque (drop the window's own transparency).
pub(crate) fn flatten_opaque(img: &mut RgbaImage) {
    for px in img.pixels_mut() {
        px[3] = 255;
    }
}

/// Mask the four corners to a rounded rectangle (alpha → 0 outside the radius,
/// anti-aliased), matching the corner rounding cosmic-comp draws on windows.
/// `radius` is in this image's pixels. No-op for radius 0.
pub(crate) fn round_corners(img: &mut RgbaImage, radius: u32) {
    let (w, h) = (img.width(), img.height());
    let r = radius.min(w / 2).min(h / 2);
    if r == 0 {
        return;
    }
    let rf = r as f32;
    // (region origin x, region origin y, circle-center x, circle-center y)
    let corners = [
        (0, 0, rf, rf),
        (w - r, 0, (w - r) as f32, rf),
        (0, h - r, rf, (h - r) as f32),
        (w - r, h - r, (w - r) as f32, (h - r) as f32),
    ];
    for (ox, oy, ccx, ccy) in corners {
        for y in oy..oy + r {
            for x in ox..ox + r {
                let dx = x as f32 + 0.5 - ccx;
                let dy = y as f32 + 0.5 - ccy;
                let dist = (dx * dx + dy * dy).sqrt();
                // 1 inside the radius, 0 outside, 1px anti-aliased edge.
                let cov = (rf - dist + 0.5).clamp(0.0, 1.0);
                let p = img.get_pixel_mut(x, y);
                p[3] = (p[3] as f32 * cov).round() as u8;
            }
        }
    }
}

/// Post-process a captured window to match cosmic's on-screen look: optionally
/// flatten its transparency, then round the corners (`radius` in image pixels).
pub fn finish_window(mut img: RgbaImage, radius: u32, keep_transparency: bool) -> RgbaImage {
    if !keep_transparency {
        flatten_opaque(&mut img);
    }
    round_corners(&mut img, radius);
    img
}

/// Opacify a captured window's translucent BODY while PRESERVING its native rounded
/// corner outline (DRAGON-268). SCK delivers a window's native rounded corners as
/// transparent corner pixels (alpha ramping 0 -> body alpha across the squircle arc),
/// and a translucent window carries a sub-255 body alpha across the interior. Flattening
/// EVERY pixel to opaque (the old `flatten_opaque` here) squared the corners AND colored
/// the corner pixels, so the concentric border (which dilates this alpha) traced a
/// rectangle and the squared corner pixels read as a second, boxy border.
///
/// Instead: fill every pixel to opaque (so a translucent body gets an opaque backing,
/// which is what "Preserve transparency OFF" means), THEN re-carve the four native corner
/// arcs, bounded to a corner block of the detected corner radius. Inside each block the
/// original arc alpha is NORMALISED by the body alpha, so a filled corner-block pixel
/// (orig == body) stays fully opaque while the arc + its anti-aliasing keep their exact
/// native squircle shape (orig 0 -> 0, half-covered -> 128). The border still hugs the
/// real rounded corner and the corners stay rounded; the downstream `on_black` /
/// `composite_over_wallpaper` supplies the opaque interior colour behind the now-opaque
/// body.
///
/// A window with no readable rounded corner (square / opaque-cornered) has nothing to
/// restore, so it is simply flattened opaque (identical to the old behaviour there).
#[cfg(target_os = "macos")]
fn opacify_body_keep_corners(img: &mut RgbaImage) {
    let (w, h) = (img.width(), img.height());
    // The corner span (physical px) from the captured alpha arc, read while the window
    // still carries its native corner alpha. None -> square / opaque corners: nothing to
    // restore, so a plain opaque flatten (the old behaviour) is correct there.
    let Some(r) = crate::decoration::corner_radius_from_alpha(img) else {
        flatten_opaque(img);
        return;
    };
    let r = (r.round() as u32).min(w).min(h);
    if r == 0 {
        flatten_opaque(img);
        return;
    }
    // The BODY alpha is the largest alpha present (opaque body -> 255; a uniformly
    // translucent body -> its own value). Inside each corner block the native arc ramps
    // 0 -> body across the squircle; NORMALISING by the body alpha turns that ramp into a
    // 0 -> 255 coverage mask, so a filled corner-block pixel (orig == body) becomes fully
    // opaque (the translucency is filled) while the arc + its anti-aliasing keep their
    // exact shape (orig 0 -> 0, a half-covered AA pixel -> 128). This is what keeps the
    // native squircle outline AND opacifies the interior at once.
    let body = img.pixels().map(|p| p[3]).max().unwrap_or(255).max(1);
    let orig_alpha: Vec<u8> = img.pixels().map(|p| p[3]).collect();
    let body_f = body as f32;
    flatten_opaque(img);
    let mut restore = |ox: u32, oy: u32| {
        for y in oy..(oy + r).min(h) {
            for x in ox..(ox + r).min(w) {
                let a = orig_alpha[(y * w + x) as usize] as f32;
                img.get_pixel_mut(x, y)[3] = ((a / body_f) * 255.0).round().min(255.0) as u8;
            }
        }
    };
    restore(0, 0);
    restore(w.saturating_sub(r), 0);
    restore(0, h.saturating_sub(r));
    restore(w.saturating_sub(r), h.saturating_sub(r));
}

/// Post-process a captured window whose corners are ALREADY native (delivered
/// pre-rounded as alpha — the macOS SCK single-window grab), so it skips
/// [`round_corners`] entirely. Rounding a natively-rounded window a second time would
/// eat a ring of real pixels at the corners; on macOS the corners come free
/// (DRAGON-186 Phase 5). When `keep_transparency` is false, the window's translucent
/// BODY is opacified but the native rounded corner OUTLINE is PRESERVED
/// (DRAGON-268) via [`opacify_body_keep_corners`] — the downstream `on_black` /
/// `composite_over_wallpaper` supplies the opaque interior backing, and the border
/// still traces the real rounded corners instead of squaring them.
/// `#[cfg(macos)]` keeps the Linux build byte-identical (Linux screencopy delivers
/// SQUARE corners, so it always rounds via [`finish_window`]).
#[cfg(target_os = "macos")]
pub fn finish_window_native_corners(mut img: RgbaImage, keep_transparency: bool) -> RgbaImage {
    if !keep_transparency {
        opacify_body_keep_corners(&mut img);
    }
    img
}

/// Wrap a decorated window with cosmic's drop shadow plus a `margin`-px transparent
/// border (room for the shadow and the wallpaper gap). Returns a canvas of size
/// (win + 2*margin); the shadow is painted with the window's rounded footprint cut
/// out (cosmic draws no shadow *under* the window, so a translucent window shows the
/// wallpaper, not the shadow), then `win` is composited on top. `radius` is win's
/// outer corner radius in image px; `scale` converts cosmic's logical shadow params
/// to image px. Based on cosmic-comp's ShadowShader (softness/spread/offset), but a
/// touch heavier (larger spread/sigma, higher opacity) since the box-blur + carve
/// renders lighter than cosmic's analytic shadow.
pub fn with_shadow(win: RgbaImage, margin: u32, radius: u32, scale: f32, dark: bool) -> RgbaImage {
    let (ww, wh) = (win.width(), win.height());
    let (cw, ch) = (ww + 2 * margin, wh + 2 * margin);
    let spread = (8.0 * scale).round().max(0.0) as u32;
    let sigma = (16.0 * scale).max(0.5);
    // Small downward offset so the shadow sits more centred on the window (was 6).
    let offset_y = (3.0 * scale).round() as i64;
    // Lightened a bit more, but kept above the first (0.45/0.35) opacity.
    let max_a = if dark { 0.5 } else { 0.4 };

    // Black rounded box = window box grown by `spread`, rounded at radius+spread,
    // laid into the canvas at the window position shifted by the downward offset.
    let mut box_img =
        RgbaImage::from_pixel(ww + 2 * spread, wh + 2 * spread, image::Rgba([0, 0, 0, 255]));
    round_corners(&mut box_img, radius + spread);
    let mut shadow = RgbaImage::new(cw, ch);
    let bx = margin as i64 - spread as i64;
    let by = margin as i64 - spread as i64 + offset_y;
    image::imageops::overlay(&mut shadow, &box_img, bx, by);
    // fast_blur (box-blur approximation) instead of the true Gaussian `blur`, which
    // is orders of magnitude slower on a full-window canvas.
    let mut shadow = image::imageops::fast_blur(&shadow, sigma);

    // Cut the window's rounded footprint out of the blurred halo and apply the
    // shadow opacity; force the colour to black (blur leaves it black already).
    let mut footprint = RgbaImage::new(cw, ch);
    let mut fp_box = RgbaImage::from_pixel(ww, wh, image::Rgba([0, 0, 0, 255]));
    round_corners(&mut fp_box, radius);
    image::imageops::overlay(&mut footprint, &fp_box, margin as i64, margin as i64);
    for (sp, fp) in shadow.pixels_mut().zip(footprint.pixels()) {
        let keep = 1.0 - fp[3] as f32 / 255.0;
        sp[0] = 0;
        sp[1] = 0;
        sp[2] = 0;
        sp[3] = (sp[3] as f32 * keep * max_a).round() as u8;
    }

    image::imageops::overlay(&mut shadow, &win, margin as i64, margin as i64);
    shadow
}

/// Add a transparent margin of `pad` px on every side, centring `img`. The downstream
/// background (wallpaper / black / kept-transparent) fills the margin. No-op for 0.
pub fn pad_transparent(img: RgbaImage, pad: u32) -> RgbaImage {
    if pad == 0 {
        return img;
    }
    let (w, h) = (img.width(), img.height());
    let mut canvas = RgbaImage::from_pixel(w + 2 * pad, h + 2 * pad, image::Rgba([0, 0, 0, 0]));
    image::imageops::overlay(&mut canvas, &img, pad as i64, pad as i64);
    canvas
}

/// Wrap a finished window with cosmic's active-window hint: a `border`-px ring of
/// `color` around it, rounded concentrically (`outer_radius` = window radius +
/// border) so it hugs the window's rounding. No-op for border 0.
pub fn add_border(win: RgbaImage, border: u32, color: [u8; 4], outer_radius: u32) -> RgbaImage {
    if border == 0 {
        return win;
    }
    let (w, h) = (win.width(), win.height());
    let mut canvas = RgbaImage::from_pixel(w + 2 * border, h + 2 * border, image::Rgba(color));
    round_corners(&mut canvas, outer_radius);
    // Cut the window's footprint out of the coloured fill so ONLY the ring is
    // coloured. Otherwise a translucent (or transparency-multiplied) window body
    // reveals the border colour beneath it instead of the wallpaper. The footprint
    // is rounded at the window's own radius (outer_radius - border).
    let inner_r = outer_radius.saturating_sub(border);
    let mut footprint = RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 0, 255]));
    round_corners(&mut footprint, inner_r);
    for y in 0..h {
        for x in 0..w {
            let cov = footprint.get_pixel(x, y)[3] as f32 / 255.0; // 1 inside, AA at corners
            let p = canvas.get_pixel_mut(x + border, y + border);
            p[3] = (p[3] as f32 * (1.0 - cov)).round() as u8;
        }
    }
    // Window on top of the now-hollow ring; its translucent body sits over
    // transparency here, so the wallpaper (added later) shows through it.
    image::imageops::overlay(&mut canvas, &win, border as i64, border as i64);
    canvas
}

/// Wrap a window whose corners are ALREADY native (a real per-pixel alpha corner
/// baked in by SCK, the macOS single-window grab) with a `border`-px ring of `color`
/// that is CONCENTRIC with the window's REAL corner shape, whatever that shape is.
///
/// Unlike [`add_border`] (which rounds the ring's outer edge with a CIRCULAR
/// `round_corners`, correct for the Linux path where the window's own corners are a
/// circle of the theme radius), macOS windows use a CONTINUOUS-curvature *squircle*
/// corner, not a circle — so a circular outer arc bulges away from the window in the
/// middle of the corner (the DRAGON-188 Bug 2 "ring dips / sweeps wrong at the
/// corner"). Instead of guessing a radius, this DILATES the window's own alpha mask
/// outward by `border` px: the ring is exactly the set of pixels within `border` of
/// an opaque window pixel that the window itself does not cover. That hugs any corner
/// shape (circle, squircle, square) by construction and is `border` px thick on every
/// straight edge, matching the live JankyBorders overlay to ~1px. No-op for border 0.
///
/// `#[cfg(macos)]` keeps the Linux build byte-identical — Linux never calls this
/// (its screencopy corners are circular, so [`add_border`] is exactly right there).
#[cfg(target_os = "macos")]
pub fn add_border_native_corners(win: RgbaImage, border: u32, color: [u8; 4]) -> RgbaImage {
    if border == 0 {
        return win;
    }
    let (w, h) = (win.width(), win.height());
    let b = border as i32;
    let (cw, ch) = (w + 2 * border, h + 2 * border);
    // Window opaque coverage (alpha as f32 0..1), indexed in window space.
    let cov = |x: i32, y: i32| -> f32 {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            0.0
        } else {
            win.get_pixel(x as u32, y as u32)[3] as f32 / 255.0
        }
    };
    // Precompute a circular offset kernel (dx, dy) with dx*dx+dy*dy <= border*border,
    // so the dilation grows the mask by a ROUND `border`, matching a stroke's uniform
    // outward extent (a square kernel would over-extend the diagonals).
    let mut kernel: Vec<(i32, i32)> = Vec::new();
    for dy in -b..=b {
        for dx in -b..=b {
            if dx * dx + dy * dy <= b * b {
                kernel.push((dx, dy));
            }
        }
    }
    let mut canvas = RgbaImage::new(cw, ch);
    for cy in 0..ch as i32 {
        for cx in 0..cw as i32 {
            // Canvas -> window space (the window sits at offset (border, border)).
            let (wx, wy) = (cx - b, cy - b);
            let self_cov = cov(wx, wy);
            // The dilated (grown) coverage: the MAX window coverage over the kernel
            // neighbourhood — 1 wherever any opaque window pixel is within `border`.
            let mut grown = self_cov;
            for &(dx, dy) in &kernel {
                let c = cov(wx + dx, wy + dy);
                if c > grown {
                    grown = c;
                    if grown >= 1.0 {
                        break;
                    }
                }
            }
            // Ring coverage = grown minus the window's own coverage (so the ring is
            // ONLY outside the window; the window is composited on top below and keeps
            // its own translucency over the wallpaper). Clamp at 0.
            let ring = (grown - self_cov).max(0.0);
            if ring > 0.0 {
                let p = canvas.get_pixel_mut(cx as u32, cy as u32);
                p[0] = color[0];
                p[1] = color[1];
                p[2] = color[2];
                p[3] = (color[3] as f32 * ring).round() as u8;
            }
        }
    }
    // Window on top of the ring (its translucent body sits over transparency here, so
    // the wallpaper added later shows through it — same as `add_border`).
    image::imageops::overlay(&mut canvas, &win, border as i64, border as i64);
    canvas
}

/// Composite an image over opaque black (fills transparent corners/gaps),
/// yielding a fully opaque result.
pub fn on_black(img: RgbaImage) -> RgbaImage {
    let mut bg = RgbaImage::from_pixel(img.width(), img.height(), image::Rgba([0, 0, 0, 255]));
    image::imageops::overlay(&mut bg, &img, 0, 0);
    bg
}

/// Composite `top` over `bottom` (same size); returns `bottom` with `top` on it.
///
/// We blend with `image::imageops::overlay` (straight-alpha src-over on sRGB bytes,
/// i.e. gamma space). An earlier version blended in linear light to chase a "too
/// opaque" look, but that was actually `add_border` filling the whole box with the
/// border colour (making the body opaque); with that fixed, the plain gamma blend
/// matches how cosmic shows the translucent window.
///
/// Callers: the wallpaper-behind-window composite in `screenshot.rs` (Wayland
/// path), the macOS `platform/mac/screenshot.rs` window-over-wallpaper composite
/// (DRAGON-186 Phase 2), and the Windows `platform/windows/screenshot.rs` one
/// (DRAGON-229 M1). `#[cfg(any(linux, macos, windows))]` keeps the Linux/mac builds
/// byte-identical (the same fn, same body) while making it visible on Windows.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub fn over(mut bottom: RgbaImage, top: &RgbaImage) -> RgbaImage {
    image::imageops::overlay(&mut bottom, top, 0, 0);
    bottom
}

/// Trim runs of FULLY-transparent (alpha == 0) rows/columns off the OUTER edges of a
/// captured window, returning `(cropped, (left, top, width, height))` where the
/// tuple is the kept rect in the INPUT image's pixels (so a caller can shift a
/// left/top trim into its geometry).
///
/// DRAGON-190: some app windows (Electron/CEF/Java-AWT/game windows, or a window
/// left with a transparent gutter after a horizontal resize) report an
/// `SCWindow.frame` WIDER than their visible content, with an invisible fully
/// transparent backing region on one edge. SCK renders that whole frame, so the
/// captured window carries a dead transparent gutter. This scans each edge inward,
/// counting rows/columns whose EVERY pixel is `alpha == 0`, and crops those runs
/// away so the border hugs the real content and the wallpaper aligns to it.
///
/// GUARDS (never eat legitimate transparency):
/// - Only FULLY-transparent (`alpha == 0`) rows/columns count. A translucent window
///   BODY (alpha ~128) is never fully transparent, so it is never trimmed.
/// - A native rounded corner makes only the CORNER pixels of an edge row/column
///   transparent, so that row/column is not fully transparent and is not counted —
///   rounded corners survive by construction. As belt-and-suspenders, an edge run is
///   only trimmed when it is STRICTLY WIDER than `corner_radius` (the window's own
///   corner span, from [`crate::decoration::corner_radius_from_alpha`]), so any small
///   transparent fringe at/below the corner size is left alone; only a genuine dead
///   gutter (many fully-transparent rows/columns) is removed.
///
/// Never trims the whole image away (a fully-transparent window keeps at least a 1px
/// span). Platform-agnostic: applied on BOTH macOS (SCK native-alpha corners) and Linux
/// (screencopy CSD shadow margins) window captures + picker thumbnails. Only
/// FULLY-transparent runs STRICTLY wider than the corner radius are removed, so a capture
/// with no dead gutter (e.g. an opaque server-side-decorated window) is returned
/// unchanged. Pure pixel math, unit-testable on a synthetic image.
pub fn trim_transparent_gutter(img: &RgbaImage, corner_radius: u32) -> (RgbaImage, (u32, u32, u32, u32)) {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return (img.clone(), (0, 0, w, h));
    }
    let col_transparent = |x: u32| (0..h).all(|y| img.get_pixel(x, y)[3] == 0);
    let row_transparent = |y: u32| (0..w).all(|x| img.get_pixel(x, y)[3] == 0);

    // Count fully-transparent runs at each edge, scanning inward. Cap each run so the
    // opposite edge always survives (never trim the whole axis away).
    let mut left = (0..w).take_while(|&x| col_transparent(x)).count() as u32;
    let mut right = (0..w).rev().take_while(|&x| col_transparent(x)).count() as u32;
    let mut top = (0..h).take_while(|&y| row_transparent(y)).count() as u32;
    let mut bottom = (0..h).rev().take_while(|&y| row_transparent(y)).count() as u32;

    // Guard: only trim an edge run that is genuinely a gutter — STRICTLY WIDER than the
    // window's own corner radius (a small transparent fringe or a rounded corner's span
    // is left intact). A run at/below the radius is not trimmed.
    let gutter = |run: u32| if run > corner_radius { run } else { 0 };
    left = gutter(left);
    right = gutter(right);
    top = gutter(top);
    bottom = gutter(bottom);

    // Never collapse an axis: keep at least 1px if a fully-transparent window made the
    // opposite runs cover (or overshoot) the whole span. Clamp `left`/`top` so the
    // opposite edge always has room, then trim the far edge to what's left.
    if left + right >= w {
        left = left.min(w.saturating_sub(1));
        right = w.saturating_sub(left + 1);
    }
    if top + bottom >= h {
        top = top.min(h.saturating_sub(1));
        bottom = h.saturating_sub(top + 1);
    }

    let kx = left;
    let ky = top;
    let kw = w - left - right;
    let kh = h - top - bottom;
    if left == 0 && top == 0 && kw == w && kh == h {
        return (img.clone(), (0, 0, w, h));
    }
    let cropped = image::imageops::crop_imm(img, kx, ky, kw, kh).to_image();
    (cropped, (kx, ky, kw, kh))
}

#[cfg(all(test, target_os = "macos"))]
mod native_border_tests {
    use super::*;

    /// A synthetic window with a rounded top-left alpha corner of `radius` (a circle
    /// centred at (radius, radius)), otherwise opaque — the SCK native-corner shape.
    fn rounded_window(w: u32, h: u32, radius: u32) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(w, h, image::Rgba([200, 100, 50, 255]));
        let rf = radius as f32;
        for y in 0..radius {
            for x in 0..radius {
                let dx = rf - x as f32;
                let dy = rf - y as f32;
                if dx * dx + dy * dy > rf * rf {
                    img.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
                }
            }
        }
        img
    }

    // DRAGON-188 Bug 1: the ring is exactly `border` px wide on a STRAIGHT edge (all
    // outside the window), matching the live JankyBorders overlay measured at 6 phys px.
    #[test]
    fn dilated_ring_is_border_px_on_straight_edges() {
        let win = RgbaImage::from_pixel(40, 40, image::Rgba([10, 20, 30, 255])); // square
        let bw = 6u32;
        let color = [151, 125, 236, 255];
        let out = add_border_native_corners(win, bw, color);
        // Canvas grew by 2*border on each axis.
        assert_eq!((out.width(), out.height()), (52, 52));
        // Along the top-edge midpoint column (x = border + 20), the ring occupies the
        // `border` rows ABOVE the window (y in 0..border) and the window starts at y=bw.
        let x = bw + 20;
        for y in 0..bw {
            assert_eq!(out.get_pixel(x, y).0, color, "ring pixel at y={y}");
        }
        // The window body starts exactly at y = border (no straddle inside).
        assert_eq!(out.get_pixel(x, bw).0, [10, 20, 30, 255], "window body at y=border");
        // Just outside the ring (above it) is transparent.
        assert_eq!(out.get_pixel(x, 0).0[3], 255, "outermost ring row still opaque");
    }

    // The ring hugs a ROUNDED corner concentrically: the ring exists diagonally outside
    // the window's rounded corner, and NO ring pixel intrudes where the window body is.
    #[test]
    fn dilated_ring_hugs_a_rounded_corner_without_intruding() {
        let radius = 12u32;
        let win = rounded_window(60, 60, radius);
        let bw = 6u32;
        let color = [151, 125, 236, 255];
        let out = add_border_native_corners(win.clone(), bw, color);
        // Where the window is OPAQUE (offset by border), the output must be the window
        // body, never the ring colour (the ring is only OUTSIDE the window).
        for y in 0..60u32 {
            for x in 0..60u32 {
                if win.get_pixel(x, y).0[3] == 255 {
                    let p = out.get_pixel(x + bw, y + bw).0;
                    assert_ne!(p, color, "ring intruded into window body at ({x},{y})");
                }
            }
        }
        // The ring is present diagonally outside the corner arc: a pixel a few px out
        // along the 45deg line from the window corner carries the ring colour.
        // Window corner arc meets the diagonal ~radius*(1-1/sqrt2) in; just outside the
        // window (small canvas coords) is ring.
        let has_ring = out.pixels().any(|p| p.0 == color);
        assert!(has_ring, "the dilated ring must draw the colour somewhere");
    }

    // border 0 is a no-op (returns the window unchanged, same dimensions).
    #[test]
    fn dilated_ring_border_zero_is_noop() {
        let win = RgbaImage::from_pixel(10, 10, image::Rgba([1, 2, 3, 255]));
        let out = add_border_native_corners(win.clone(), 0, [9, 9, 9, 255]);
        assert_eq!((out.width(), out.height()), (10, 10));
        assert_eq!(out.as_raw(), win.as_raw());
    }

    // A fully-transparent window yields no ring (nothing to dilate) — the canvas stays
    // transparent so a downstream composite over black/wallpaper is unaffected.
    #[test]
    fn dilated_ring_all_transparent_window_has_no_ring() {
        let win = RgbaImage::from_pixel(20, 20, image::Rgba([0, 0, 0, 0]));
        let out = add_border_native_corners(win, 4, [151, 125, 236, 255]);
        assert!(out.pixels().all(|p| p.0[3] == 0), "no ring around an empty window");
    }

    // DRAGON-268: transparency OFF must NOT square the native rounded corner. Finish an
    // opaque-body window (transparent rounded top-left corner) with keep_transparency
    // false, then draw the border and composite on black; the corner region must stay
    // rounded (the outline follows the alpha, the extreme corner pixel is NOT the ring
    // colour), while the interior is opaque and the border traces the corner.
    #[test]
    fn transparency_off_keeps_native_rounded_corner_outline() {
        let radius = 12u32;
        let body = [200u8, 100, 50, 255];
        let win = rounded_window(60, 60, radius); // opaque body, transparent TL corner
        // Finish with transparency OFF: the corner alpha (rounded outline) must survive.
        let fin = finish_window_native_corners(win.clone(), false);
        // The extreme top-left corner pixel is OUTSIDE the arc, so it stays transparent
        // (NOT flattened to opaque). This is the regression: the old flatten made it 255.
        assert_eq!(fin.get_pixel(0, 0)[3], 0, "native rounded corner NOT squared to opaque");
        // A deep-interior pixel is opaque (body unchanged; opaque body stays opaque).
        assert_eq!(fin.get_pixel(40, 40)[3], 255, "interior stays opaque");

        // Now the border + on_black, as run() does for wallpaper OFF + transparency OFF.
        let bw = 4u32;
        let ring = [151u8, 125, 236, 255];
        let bordered = add_border_native_corners(fin, bw, ring);
        let out = on_black(bordered);
        // The whole result is opaque after on_black (opaque interior backing).
        assert!(out.pixels().all(|p| p.0[3] == 255), "on_black yields a fully opaque result");
        // The extreme outer corner of the canvas is OUTSIDE the concentric ring's arc, so
        // it must be black (the on_black backing), NOT the ring colour — i.e. the ring is
        // rounded, not a square box. (Canvas is 60+2*bw square; (0,0) is the far corner.)
        assert_eq!(out.get_pixel(0, 0).0, [0, 0, 0, 255], "outer corner is black, ring is rounded not boxed");
        // The ring colour IS drawn somewhere (the border exists and traces the window).
        assert!(out.pixels().any(|p| p.0 == ring), "the rounded border ring is present");
        // Deep interior (window body offset by the border) shows the body colour, not the
        // ring and not black — the interior is intact and opaque.
        assert_eq!(out.get_pixel(bw + 40, bw + 40).0, body, "interior body intact and opaque");
    }

    // DRAGON-268: a TRANSLUCENT-body window (alpha 128 body, transparent rounded corner)
    // with transparency OFF has its body FILLED to opaque (that is what the toggle means)
    // while the native rounded corner outline is still preserved.
    #[test]
    fn transparency_off_opacifies_translucent_body_but_keeps_corner() {
        let radius = 10u32;
        // Opaque-ALPHA body would be 255; make it translucent 128 and round the TL corner
        // to a fraction of that body alpha (the native ramp is proportional to the body).
        let mut win = RgbaImage::from_pixel(50, 50, image::Rgba([10, 180, 60, 128]));
        let rf = radius as f32;
        for y in 0..radius {
            for x in 0..radius {
                let dx = rf - x as f32;
                let dy = rf - y as f32;
                if dx * dx + dy * dy > rf * rf {
                    // Outside the arc: fully transparent corner.
                    win.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
                }
            }
        }
        let fin = finish_window_native_corners(win, false);
        // A CENTRE body pixel (away from every corner block) was 128 -> filled to 255
        // (opacified: the translucency is filled, which is what transparency OFF means).
        assert_eq!(fin.get_pixel(25, 25)[3], 255, "translucent body filled to opaque");
        // The native corner is still transparent (rounded outline preserved).
        assert_eq!(fin.get_pixel(0, 0)[3], 0, "rounded corner preserved on a translucent window");
        // A pixel INSIDE the corner block but INSIDE the arc (covered body, orig 128) is
        // ALSO filled opaque -- the fill reaches the corner-block interior, it does not
        // leave a translucent patch in the corners.
        assert_eq!(fin.get_pixel(9, 9)[3], 255, "corner-block interior filled opaque, no translucent patch");
    }

    // Regression guard: transparency ON leaves the finished image byte-identical to the
    // input (no opacify, no rounding) — the ON path is unchanged from before DRAGON-268.
    #[test]
    fn transparency_on_is_unchanged() {
        let win = rounded_window(40, 40, 8);
        let out = finish_window_native_corners(win.clone(), true);
        assert_eq!(out.as_raw(), win.as_raw(), "transparency ON returns the window untouched");
    }
}

/// DRAGON-190 trim tests — NOT macOS-gated: `trim_transparent_gutter` is platform-
/// agnostic (window captures + picker thumbnails on both macOS and Linux), so its
/// regression net runs everywhere. Pure synthetic-image logic, no platform types.
#[cfg(test)]
mod trim_tests {
    use super::*;

    /// A window with `content_w` opaque columns on the left and `gutter_w` FULLY
    /// transparent columns on the right — the dead-gutter shape the ticket describes.
    fn window_with_right_gutter(content_w: u32, gutter_w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(content_w + gutter_w, h, image::Rgba([200, 100, 50, 255]));
        for y in 0..h {
            for x in content_w..content_w + gutter_w {
                img.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
            }
        }
        img
    }

    /// A window whose ONLY transparency is its native rounded top-left corner
    /// (a quarter circle of `radius` at the origin), otherwise fully opaque.
    fn rounded_corner_window(w: u32, h: u32, radius: u32) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(w, h, image::Rgba([200, 100, 50, 255]));
        let rf = radius as f32;
        for y in 0..radius {
            for x in 0..radius {
                let dx = rf - x as f32;
                let dy = rf - y as f32;
                if dx * dx + dy * dy > rf * rf {
                    img.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
                }
            }
        }
        img
    }

    // A dead transparent gutter WIDER than the corner radius is trimmed to the content.
    #[test]
    fn trim_removes_a_wide_right_gutter() {
        let img = window_with_right_gutter(80, 20, 40); // 100x40, 20px right gutter
        let (cropped, rect) = trim_transparent_gutter(&img, 8);
        assert_eq!((cropped.width(), cropped.height()), (80, 40), "gutter columns removed");
        assert_eq!(rect, (0, 0, 80, 40), "kept rect is the content, no left/top shift");
        // Every remaining column has an opaque pixel (no dead space left).
        for x in 0..cropped.width() {
            assert!((0..cropped.height()).any(|y| cropped.get_pixel(x, y)[3] != 0), "col {x} not dead");
        }
    }

    // A left gutter is trimmed AND reported so the caller can shift the origin.
    #[test]
    fn trim_removes_a_left_gutter_and_reports_the_shift() {
        // 30 transparent left columns, then 70 opaque; radius guard 8.
        let mut img = RgbaImage::from_pixel(100, 40, image::Rgba([10, 20, 30, 255]));
        for y in 0..40 {
            for x in 0..30 {
                img.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
            }
        }
        let (cropped, rect) = trim_transparent_gutter(&img, 8);
        assert_eq!((cropped.width(), cropped.height()), (70, 40));
        assert_eq!(rect, (30, 0, 70, 40), "left trim reports its x offset for the origin shift");
    }

    // A normal fully-opaque window is UNCHANGED (the trim is a no-op).
    #[test]
    fn trim_leaves_an_opaque_window_untouched() {
        let img = RgbaImage::from_pixel(120, 80, image::Rgba([1, 2, 3, 255]));
        let (cropped, rect) = trim_transparent_gutter(&img, 10);
        assert_eq!((cropped.width(), cropped.height()), (120, 80));
        assert_eq!(rect, (0, 0, 120, 80));
        assert_eq!(cropped.as_raw(), img.as_raw(), "no-op returns identical pixels");
    }

    // A rounded-corner window (transparent CORNERS only) is NOT trimmed: no edge
    // row/column is fully transparent, so the corners are preserved.
    #[test]
    fn trim_preserves_rounded_corners() {
        let img = rounded_corner_window(60, 60, 12);
        let (cropped, rect) = trim_transparent_gutter(&img, 12);
        assert_eq!((cropped.width(), cropped.height()), (60, 60), "corners not eaten");
        assert_eq!(rect, (0, 0, 60, 60));
        // The native rounded corner's transparent pixel is still there.
        assert_eq!(cropped.get_pixel(0, 0)[3], 0, "rounded corner preserved");
    }

    // A TRANSLUCENT-body window (alpha ~128, never 0) is NEVER trimmed — the alpha==0
    // test only fires on FULLY transparent runs.
    #[test]
    fn trim_never_touches_a_translucent_body() {
        let img = RgbaImage::from_pixel(50, 50, image::Rgba([255, 0, 0, 128]));
        let (cropped, rect) = trim_transparent_gutter(&img, 6);
        assert_eq!((cropped.width(), cropped.height()), (50, 50), "translucent body kept whole");
        assert_eq!(rect, (0, 0, 50, 50));
    }

    // A transparent run NARROWER than the corner radius is NOT trimmed (the guard):
    // a thin fringe below the corner span is left intact.
    #[test]
    fn trim_guard_keeps_a_run_at_or_below_the_radius() {
        // 6px transparent right run, radius guard 8 -> 6 <= 8, so no trim.
        let img = window_with_right_gutter(94, 6, 40); // 100x40
        let (cropped, rect) = trim_transparent_gutter(&img, 8);
        assert_eq!((cropped.width(), cropped.height()), (100, 40), "narrow run below radius kept");
        assert_eq!(rect, (0, 0, 100, 40));
        // Exactly at the radius is also kept (strictly-wider guard).
        let at = window_with_right_gutter(92, 8, 40);
        let (c2, _) = trim_transparent_gutter(&at, 8);
        assert_eq!(c2.width(), 100, "run == radius is kept (strictly-wider guard)");
        // One px WIDER than the radius IS trimmed.
        let over = window_with_right_gutter(91, 9, 40);
        let (c3, _) = trim_transparent_gutter(&over, 8);
        assert_eq!(c3.width(), 91, "run > radius is trimmed");
    }

    // A fully-transparent window never collapses to zero width/height.
    #[test]
    fn trim_never_collapses_an_empty_window() {
        let img = RgbaImage::from_pixel(30, 30, image::Rgba([0, 0, 0, 0]));
        let (cropped, _) = trim_transparent_gutter(&img, 4);
        assert!(cropped.width() >= 1 && cropped.height() >= 1, "at least 1px survives");
    }
}
