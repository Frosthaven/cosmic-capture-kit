//! Shared grayscale + computer-vision primitives for the in-region scanners.
//!
//! Both [`super::codes`] and [`super::text`] start from a luma view of the RGBA snapshot
//! and sample it with the same small kernels, so the conversion, the bounding-box helper,
//! and the Sobel/doubled-angle machinery live here once instead of being duplicated.

/// Derive a luma buffer (BT.601) from RGBA.
pub(crate) fn to_luma(img: &image::RgbaImage) -> Vec<u8> {
    let mut out = vec![0u8; (img.width() * img.height()) as usize];
    for (i, p) in img.pixels().enumerate() {
        let [r, g, b, _] = p.0;
        out[i] = ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8;
    }
    out
}

/// Axis-aligned bounding box `(x, y, w, h)` of a 4-corner outline.
pub(crate) fn quad_bbox(poly: &[(i32, i32); 4]) -> (i32, i32, i32, i32) {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for &(x, y) in poly {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    (x0, y0, (x1 - x0).max(4), (y1 - y0).max(4))
}

/// Bounds-checked luma sample (out-of-range reads as 0).
fn sample(lum: &[u8], iw: u32, ih: u32, x: i32, y: i32) -> i32 {
    if x < 0 || y < 0 || x >= iw as i32 || y >= ih as i32 {
        0
    } else {
        lum[(y as u32 * iw + x as u32) as usize] as i32
    }
}

/// 3x3 Sobel gradient `(gx, gy)` of the luma buffer at `(cx, cy)` (zero-padded edges).
pub(crate) fn sobel_at(lum: &[u8], iw: u32, ih: u32, cx: i32, cy: i32) -> (f32, f32) {
    let at = |x: i32, y: i32| sample(lum, iw, ih, x, y);
    let gx = (at(cx + 1, cy - 1) + 2 * at(cx + 1, cy) + at(cx + 1, cy + 1))
        - (at(cx - 1, cy - 1) + 2 * at(cx - 1, cy) + at(cx - 1, cy + 1));
    let gy = (at(cx - 1, cy + 1) + 2 * at(cx, cy + 1) + at(cx + 1, cy + 1))
        - (at(cx - 1, cy - 1) + 2 * at(cx, cy - 1) + at(cx + 1, cy - 1));
    (gx as f32, gy as f32)
}

/// Resolve a doubled-angle gradient accumulation (`Σ mag·cos2θ`, `Σ mag·sin2θ`) into a
/// unit direction. Doubling the angle makes opposite edges (dark→light / light→dark)
/// reinforce rather than cancel; the result is flipped to agree with `fallback`, which is
/// also returned verbatim when there was no signal.
pub(crate) fn doubled_angle_average(s2: f32, c2: f32, fallback: (f32, f32)) -> (f32, f32) {
    if s2 == 0.0 && c2 == 0.0 {
        return fallback;
    }
    let theta = 0.5 * s2.atan2(c2);
    let (mut ux, mut uy) = (theta.cos(), theta.sin());
    if ux * fallback.0 + uy * fallback.1 < 0.0 {
        (ux, uy) = (-ux, -uy);
    }
    (ux, uy)
}
