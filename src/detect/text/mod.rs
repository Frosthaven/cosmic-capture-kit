//! OCR text detection via the `tesseract` CLI (an optional system-binary dependency,
//! like ffmpeg — the feature hints + no-ops when it's missing).
//!
//! The region is pre-processed (grayscale, auto-invert for light-on-dark UIs, contrast
//! stretch, then upscaled toward ~300dpi) so small on-screen text resolves, and the
//! TSV output is parsed into per-word boxes in reading order — the data behind the
//! selectable text layer.
//!
//! Split into [`preprocess`] (image prep + deskew + the tesseract call), [`parse`]
//! (TSV → word boxes + token heuristics), and [`layout`] (word boxes → spaced text).

mod layout;
mod parse;
mod preprocess;

pub use layout::join_words;

use crate::detect::luma;

/// A recognised word: its box (global logical coords) and the reading-order line it
/// sits on. Words are returned in reading order, so a span between two of them (by
/// index) selects the natural run of text and breaks lines where `line` changes.
#[derive(Clone, Debug)]
pub struct TextWord {
    /// Axis-aligned bounding box (global logical coords) — used for region filtering,
    /// layout (`join_words`), and merge ordering.
    pub rect: (i32, i32, i32, i32),
    /// Four-corner outline (global logical coords) following the text's true slant when
    /// the region was deskewed for OCR; the highlight + hit-test use this. For straight
    /// text it's just the `rect` corners.
    pub poly: [(i32, i32); 4],
    pub text: String,
    pub line: u32,
}

/// Whether the `tesseract` OCR binary is on PATH (text scanning requires it).
pub fn tesseract_available() -> bool {
    // DRAGON-244: via `tool_available` so the on-disk `EXE_SUFFIX` is honored — on
    // Windows the binary is `tesseract.exe`, so a bare `tesseract` file check wrongly
    // reported "not installed" even though the langs probe (which SPAWNS `tesseract`,
    // resolving `.exe`) found language data. Byte-identical on Linux/macOS (empty suffix).
    crate::util::tool_available(std::path::Path::new("tesseract"))
}

/// Whether tesseract has at least one usable LANGUAGE pack installed. The binary
/// alone isn't enough — without language data (e.g. tesseract-data-eng) every OCR
/// pass fails at runtime. Runs `tesseract --list-langs` (fast), so callers should
/// cache the result; false when the binary itself is missing.
pub fn tesseract_langs_available() -> bool {
    let Ok(out) = crate::util::quiet_command("tesseract").arg("--list-langs").output() else {
        return false;
    };
    // Output: a "List of available languages (N):" header, then one language per
    // line. Some builds print the header on stderr; accept a language from either.
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.lines()
        .map(str::trim)
        .any(|l| !l.is_empty() && !l.contains(':') && !l.contains(' '))
}

/// OCR `img` (region cropped from the snapshot) with tesseract, returning per-word
/// boxes mapped to global logical coords. `rx,ry` is the region's top-left, `rw,rh` its
/// logical size.
#[allow(clippy::too_many_arguments)]
pub fn scan_text(
    img: &image::RgbaImage,
    rx: i32,
    ry: i32,
    rw: u32,
    rh: u32,
    conf_thresh: f32,
) -> Vec<TextWord> {
    let (iw, ih) = (img.width(), img.height());
    if iw < 8 || ih < 8 {
        return Vec::new();
    }
    // Grayscale (BT.601), with the running sum for the mixed-contrast check.
    let gray = luma::to_luma(img);
    let total: u64 = gray.iter().map(|&v| v as u64).sum();
    let region = (rx, ry, rw, rh);
    // Estimate text skew once (polarity-independent) so both mixed-contrast passes
    // deskew by the same angle; 0.0 when straight enough to skip rotation.
    let skew = preprocess::estimate_skew(&gray, iw, ih);
    if std::env::var_os("CCK_SKEW").is_some() {
        eprintln!("SKEW {:.2} deg", skew * 180.0 / std::f32::consts::PI);
    }

    // Mixed contrast — a bright horizontal band AND a dark band in the same selection
    // (e.g. a light header over a dark card) — can't be served by one global invert:
    // whichever polarity is wrong becomes garbage. Detect it by row brightness (robust
    // even when the bright region is a small fraction of the pixels), then OCR both
    // polarities and merge — each reads its matching-contrast region and the confidence
    // gate drops the wrong-polarity junk. Uniform regions stay a single pass.
    let row_mean = |y: u32| -> u64 {
        gray[(y * iw) as usize..((y + 1) * iw) as usize]
            .iter()
            .map(|&v| v as u64)
            .sum::<u64>()
            / iw as u64
    };
    let min_band = (ih as usize / 40).max(4);
    let bright_rows = (0..ih).filter(|&y| row_mean(y) > 170).count();
    let dark_rows = (0..ih).filter(|&y| row_mean(y) < 90).count();
    if bright_rows >= min_band && dark_rows >= min_band {
        let a = preprocess::run_ocr(&gray, iw, ih, false, region, skew, conf_thresh);
        let b = preprocess::run_ocr(&gray, iw, ih, true, region, skew, conf_thresh);
        parse::merge_words(a, b)
    } else {
        let invert = total / ((iw * ih) as u64).max(1) < 110; // mostly dark => light on dark
        preprocess::run_ocr(&gray, iw, ih, invert, region, skew, conf_thresh)
    }
}
