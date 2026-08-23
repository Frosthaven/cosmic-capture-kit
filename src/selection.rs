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

// ── What a Selection does NOT say, and why two readers got it wrong (DRAGON-734) ──────────
//
// A `Selection` says where a capture is aimed, and for two of the three modes it says it
// completely: a REGION carries the drawn rectangle, a MONITOR names its `output`, a WINDOW
// picked by OUR picker carries the toplevel's rect plus its `window_id`.
//
// There is a fourth case and it says almost nothing. A WINDOW recording whose target was
// picked in the xdg ScreenCast PORTAL's own dialog is a stream node and nothing else: the
// portal never names a window, and COSMIC's implementation sends `position: None` for every
// window stream (measured in DRAGON-562, restated at `app::capture_flow`'s
// `PortalWindowDeco::apply`). `App::portal_for_mode` therefore stands in a placeholder,
// `Selection { x: 0, y: 0, width: 1, height: 1, output: None, window_id: None }`, and that
// placeholder is BYTE-INDISTINGUISHABLE from a legitimate 1x1 region selection, which
// `App::normalized_region` admits (its floor is one point, not five).
//
// Both readers that asked "is this a region?" by testing for the ABSENCE of `output` and
// `window_id` therefore answered yes for it, and both were wrong. `App::ink_crop_in` clamped
// every live-annotation stroke into a one-point square at the screen's origin, so the marker
// drew nothing at all (DRAGON-734), and `App::toolbar_layout` never found a window rect to
// dock beside (DRAGON-735). The predicates below are the two questions those readers should
// have been asking, written once, over the MODE as well as the selection, because the mode is
// the fact that separates the fourth case from the first.

/// **Pure**, unit-tested: does `sel` name a REGION crop of one output, the rectangle live
/// annotation must be clamped into?
///
/// The `region_mode` term is what makes this safe. Without it the answer is inferred from two
/// absent fields, which the portal's window placeholder also satisfies; with it, a session
/// that is not cropping to a drawn rectangle can never be read as one. It is strictly
/// NARROWER than the guard it replaced, so every selection that used to be treated as
/// "not a region" still is.
///
/// The other two terms are kept rather than folded away: they are redundant today (every
/// `Selection` built in region mode leaves both `None`) and they cost nothing, and a future
/// mode that carries a named target while `region_mode` is somehow true must not silently
/// start cropping to its rect.
pub fn region_crop(region_mode: bool, sel: &Selection) -> bool {
    region_mode && sel.output.is_none() && sel.window_id.is_none()
}

/// **Pure**, unit-tested: is this session's capture target one we cannot LOCATE on screen?
///
/// True for exactly one case today: a WINDOW recording whose target was picked in the portal
/// dialog, where `window_id` is the field that would have carried the answer and is `None`.
/// Our own window picker always fills it, so a native window pick is never unlocated, and no
/// other mode reaches this at all.
///
/// The rect such a selection carries is a PLACEHOLDER and must not be read as geometry.
/// Callers should fall back to something they know independently (the output the portal grant
/// was made on, the whole surface) rather than to the numbers in the struct.
pub fn target_unlocated(window_mode: bool, sel: &Selection) -> bool {
    window_mode && sel.window_id.is_none()
}

#[cfg(test)]
mod target_shape_tests {
    use super::*;

    /// The placeholder `App::portal_for_mode` stands in for a portal window pick.
    fn portal_window_placeholder() -> Selection {
        Selection { x: 0, y: 0, width: 1, height: 1, output: None, window_id: None }
    }

    fn region(x: i32, y: i32, w: u32, h: u32) -> Selection {
        Selection { x, y, width: w, height: h, output: None, window_id: None }
    }

    fn monitor() -> Selection {
        Selection {
            x: 0,
            y: 0,
            width: 5120,
            height: 1440,
            output: Some("DP-3".into()),
            window_id: None,
        }
    }

    fn native_window() -> Selection {
        Selection {
            x: 1700,
            y: 56,
            width: 1694,
            height: 1384,
            output: None,
            window_id: Some("42".into()),
        }
    }

    #[test]
    fn a_drawn_region_is_a_crop() {
        assert!(region_crop(true, &region(100, 200, 640, 480)));
    }

    /// DRAGON-418 admits a one-point drag, so a 1x1 REGION is legitimate and must still crop.
    /// It is also the exact shape of the portal placeholder, which is why the mode term is
    /// the only thing that can tell the two apart.
    #[test]
    fn a_one_point_region_still_crops() {
        assert!(region_crop(true, &region(0, 0, 1, 1)));
    }

    /// DRAGON-734, the bug: the same struct, in WINDOW mode, is not a crop.
    #[test]
    fn the_portal_window_placeholder_is_never_a_crop() {
        assert!(!region_crop(false, &portal_window_placeholder()));
    }

    #[test]
    fn a_monitor_or_window_target_is_never_a_crop() {
        assert!(!region_crop(false, &monitor()));
        assert!(!region_crop(false, &native_window()));
        // Even if a mode signal ever went wrong, a named target refuses the crop.
        assert!(!region_crop(true, &monitor()));
        assert!(!region_crop(true, &native_window()));
    }

    #[test]
    fn only_a_portal_window_pick_is_unlocated() {
        assert!(target_unlocated(true, &portal_window_placeholder()));
        assert!(!target_unlocated(true, &native_window()));
        assert!(!target_unlocated(false, &portal_window_placeholder()));
        assert!(!target_unlocated(false, &region(100, 200, 640, 480)));
        assert!(!target_unlocated(false, &monitor()));
    }

    /// The two predicates answer different questions and must never both be true: a session
    /// cropping to a drawn rectangle knows exactly where it is aimed.
    #[test]
    fn a_crop_is_never_unlocated() {
        for sel in [region(0, 0, 1, 1), region(100, 200, 640, 480), monitor(), native_window()] {
            for region_mode in [false, true] {
                let window_mode = !region_mode;
                assert!(
                    !(region_crop(region_mode, &sel) && target_unlocated(window_mode, &sel)),
                    "both answered true for {sel:?}"
                );
            }
        }
    }
}
