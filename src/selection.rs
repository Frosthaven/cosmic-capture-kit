//! Resolved capture-target data type.
//!
//! The widget implementations (drag-select overlay, output hover picker,
//! spinner, drag wrapper) live in `crate::widgets`.

// Pure rect/quad geometry lives in `crate::geometry`; re-export the rectangle type
// so existing `selection::GlobalRect` references keep resolving.
pub use crate::geometry::GlobalRect;

/// A resolved selection from the overlay, in global logical coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Output/monitor name when the selection is a whole monitor (capture the
    /// output directly).
    pub output: Option<String>,
    /// Stable toplevel identifier when the selection is a window (capture the
    /// toplevel directly — occlusion-proof, no cropping).
    pub window_id: Option<String>,
}
