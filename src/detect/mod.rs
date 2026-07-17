//! In-region detection overlays, split by scanner:
//!   * [`codes`] — QR codes / barcodes via `rxing` (pure-Rust ZXing; no system deps),
//!     including the typed click actions and the orientation-following outlines.
//!   * [`text`] — OCR via the `tesseract` CLI (a system-binary dependency), returning
//!     per-word boxes for the selectable text layer.
//!
//! Both scan the clean pre-overlay snapshot and map their boxes to global logical
//! screen coords (via [`scale`]) so they can be drawn, hovered, and interacted with
//! over the region. Shared grayscale + computer-vision primitives live in [`luma`].

mod codes;
mod luma;
mod text;

pub use codes::{Mark, MarkAction, scan_codes};
pub use text::{TextWord, join_words, scan_text, tesseract_available, tesseract_langs_available};

/// Buffer px → logical px (the snapshot is at buffer resolution). Shared by both
/// scanners to map detection boxes back to screen coords.
fn scale(iw: u32, ih: u32, rw: u32, rh: u32) -> (f32, f32) {
    (iw as f32 / rw.max(1) as f32, ih as f32 / rh.max(1) as f32)
}
