//! QR code / barcode detection via `rxing` (pure-Rust ZXing; no system deps).
//!
//! Decodes codes in the region, builds an orientation-following outline for each
//! (perspective QR via finder + alignment points, 1D barcodes sized to their measured
//! bar extent), and classifies the payload into a typed [`MarkAction`].
//!
//! Split into [`outline`] (the rxing-based detection + computer-vision geometry, where
//! the `rxing` types stay) and [`action`] (the pure click-action / URI builders).

mod action;
mod outline;

pub use action::MarkAction;

use crate::detect::luma;

/// A detected QR code / barcode: its axis-aligned bounding box (global logical coords,
/// used for hit-testing), the four-corner outline that follows the code's true
/// orientation (`poly`, also global logical — a skewed/rotated code gets a skewed
/// outline), the value shown on hover, and the action a click performs.
#[derive(Clone, Debug)]
pub struct Mark {
    pub rect: (i32, i32, i32, i32),
    pub poly: [(i32, i32); 4],
    pub label: String,
    pub action: MarkAction,
    /// Raw decoded contents — what the right-click "Copy contents" yields.
    pub value: String,
    /// Whether this is a QR-family code (vs a 1D barcode / other 2D symbology).
    pub is_qr: bool,
}

/// Axis-aligned bounding box `(x, y, w, h)` of a 4-corner outline.
fn poly_bbox(poly: &[(i32, i32); 4]) -> (i32, i32, i32, i32) {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for &(x, y) in poly {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    (x0, y0, (x1 - x0).max(10), (y1 - y0).max(10))
}

/// Scan `img` (region cropped from the snapshot, at buffer resolution) for QR codes
/// and barcodes. `rx,ry` is the region's global logical top-left, `rw,rh` its
/// logical size, used to map detection points back to screen coords.
pub fn scan_codes(img: &image::RgbaImage, rx: i32, ry: i32, rw: u32, rh: u32) -> Vec<Mark> {
    let (iw, ih) = (img.width(), img.height());
    if iw < 8 || ih < 8 {
        return Vec::new();
    }
    let lum = luma::to_luma(img);
    let results = outline::decode(&lum, iw, ih);
    let (sx, sy) = super::scale(iw, ih, rw, rh);
    let mut marks = Vec::new();
    for res in results {
        let pts = res.getPoints();
        if pts.is_empty() {
            continue;
        }
        if std::env::var_os("CCK_DUMP_POINTS").is_some() {
            eprintln!(
                "RAW {:?} n={} pts={:?}",
                res.getBarcodeFormat(),
                pts.len(),
                pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()
            );
        }
        let poly = outline::code_poly(res.getBarcodeFormat(), pts, &lum, iw, ih, rx, ry, sx, sy);
        let (action, label) = action::classify(&res);
        marks.push(Mark {
            rect: poly_bbox(&poly),
            poly,
            label,
            action,
            value: res.getText().to_string(),
            is_qr: outline::is_qr(res.getBarcodeFormat()),
        });
    }
    marks
}
