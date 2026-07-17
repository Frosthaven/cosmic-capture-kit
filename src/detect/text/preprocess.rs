//! Image preprocessing for OCR: render at one polarity + upscale, estimate the text
//! skew, deskew, run the `tesseract` CLI, and hand the TSV to [`super::parse`].

use super::TextWord;
use super::parse;

/// Render the grayscale at one polarity, deskew by `skew` (radians, 0 = none), run
/// tesseract, and parse the words (mapping boxes back through the rotation).
#[allow(clippy::too_many_arguments)]
pub(super) fn run_ocr(
    gray: &[u8],
    iw: u32,
    ih: u32,
    invert: bool,
    region: (i32, i32, u32, u32),
    skew: f32,
    conf_thresh: f32,
) -> Vec<TextWord> {
    let mut ocr = render(gray, iw, ih, invert);
    let center = (ocr.width() as f32 / 2.0, ocr.height() as f32 / 2.0);
    if skew != 0.0 {
        // Rotate the upscaled image to straighten the text; boxes are rotated back by
        // `+skew` around `center` in parse_tsv_words.
        ocr = rotate_gray(&ocr, -skew);
    }
    let dir = crate::util::runtime_dir();
    let path = std::path::Path::new(&dir).join(format!(
        "cosmic-capture-kit.{}.{}.ocr.png",
        std::process::id(),
        if invert { "i" } else { "n" },
    ));
    if ocr.save(&path).is_err() {
        return Vec::new();
    }
    // psm 11 (sparse text) suits UI screenshots far better than psm 3's page-layout
    // analysis, which mis-segments icon-interspersed / multi-column UI and garbles text.
    let out = std::process::Command::new("tesseract")
        .arg(&path)
        .arg("stdout")
        .args(["--psm", "11", "--dpi", "300", "tsv"])
        .output();
    let _ = std::fs::remove_file(&path);
    let Ok(out) = out else {
        return Vec::new();
    };
    let tsv = String::from_utf8_lossy(&out.stdout);
    let (rx, ry, rw, rh) = region;
    let (sx, sy) = crate::detect::scale(ocr.width(), ocr.height(), rw, rh);
    parse::parse_tsv_words(&tsv, rx, ry, sx, sy, skew, center, conf_thresh)
}

/// Render the grayscale for OCR at one polarity: optionally invert (light-on-dark →
/// dark-on-light, which tesseract expects), then upscale (Lanczos) toward ~300dpi — up
/// to 3x, capped so a big region stays a fast pass. No contrast stretch: it crushes the
/// anti-aliased edges tesseract's LSTM reads best, and hurts more than it helps.
fn render(gray: &[u8], iw: u32, ih: u32, invert: bool) -> image::GrayImage {
    let out: Vec<u8> = if invert {
        gray.iter().map(|&g| 255 - g).collect()
    } else {
        gray.to_vec()
    };
    let base =
        image::GrayImage::from_raw(iw, ih, out).unwrap_or_else(|| image::GrayImage::new(iw, ih));
    let max_dim = iw.max(ih) as f32;
    let u = 3.0_f32.min(5000.0 / max_dim).max(1.0);
    if u > 1.01 {
        let (uw, uh) = ((iw as f32 * u).round() as u32, (ih as f32 * u).round() as u32);
        image::imageops::resize(&base, uw, uh, image::imageops::FilterType::Lanczos3)
    } else {
        base
    }
}

/// Estimate the text skew angle (radians) by the projection-profile method: for each
/// candidate angle, shear the *ink* (per-pixel deviation from the mean — robust to
/// either polarity and to a dominant background) into rotated rows and score how
/// concentrated it is (`Σ binSum²` — maximised when text rows align into sharp peaks).
/// Returns 0.0 unless a clear tilt > ~1° is found (so straight text is untouched and a
/// near-flat scan isn't rotated on noise).
pub(super) fn estimate_skew(gray: &[u8], iw: u32, ih: u32) -> f32 {
    if std::env::var_os("CCK_NODESKEW").is_some() {
        return 0.0;
    }
    let (w, h) = (iw as i32, ih as i32);
    let step = (w.max(h) / 700).max(1); // downsample stride for speed
    let off = w; // bin offset so `y - x*tan` stays non-negative
    let nbins = (h + 2 * w) as usize;
    let mean = gray.iter().map(|&v| v as f64).sum::<f64>() / gray.len().max(1) as f64;
    let score = |a: f32| -> f64 {
        let t = a.tan();
        let mut sum = vec![0f64; nbins];
        let mut y = 0;
        while y < h {
            let mut x = 0;
            while x < w {
                let bin = (y as f32 - x as f32 * t).round() as i32 + off;
                if bin >= 0 && (bin as usize) < nbins {
                    // Ink = how far this pixel is from the background mean.
                    sum[bin as usize] += (gray[(y * w + x) as usize] as f64 - mean).abs();
                }
                x += step;
            }
            y += step;
        }
        sum.iter().map(|s| s * s).sum()
    };
    let deg = std::f32::consts::PI / 180.0;
    let base = score(0.0);
    let (mut best_a, mut best) = (0.0f32, base);
    let mut i = -24;
    while i <= 24 {
        let a = i as f32 * 0.5 * deg;
        let s = score(a);
        if s > best {
            best = s;
            best_a = a;
        }
        i += 1;
    }
    // Require a clear tilt and a meaningful sharpening over straight, else don't rotate.
    if best_a.abs() > 1.0 * deg && best > base * 1.01 {
        best_a
    } else {
        0.0
    }
}

/// Rotate a grayscale image by `angle` radians (CCW) about its centre via inverse-map
/// bilinear sampling, keeping the same dimensions; exposed areas are filled white (the
/// post-`render` background is always light).
fn rotate_gray(img: &image::GrayImage, angle: f32) -> image::GrayImage {
    let (w, h) = (img.width(), img.height());
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let (s, c) = angle.sin_cos();
    let mut out = image::GrayImage::new(w, h);
    for oy in 0..h {
        for ox in 0..w {
            // Inverse map: source point for this output pixel (rotate by -angle).
            let (dx, dy) = (ox as f32 - cx, oy as f32 - cy);
            let sxf = cx + dx * c + dy * s;
            let syf = cy - dx * s + dy * c;
            let v = if sxf >= 0.0 && syf >= 0.0 && sxf <= (w - 1) as f32 && syf <= (h - 1) as f32 {
                let (x0, y0) = (sxf.floor() as u32, syf.floor() as u32);
                let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
                let (fx, fy) = (sxf - x0 as f32, syf - y0 as f32);
                let p = |x: u32, y: u32| img.get_pixel(x, y).0[0] as f32;
                let top = p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx;
                let bot = p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx;
                (top * (1.0 - fy) + bot * fy).round() as u8
            } else {
                255
            };
            out.put_pixel(ox, oy, image::Luma([v]));
        }
    }
    out
}
