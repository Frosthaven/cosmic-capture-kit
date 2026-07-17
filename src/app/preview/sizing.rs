//! DRAGON-221: the portable, unit-tested spawn-size + display-fit rules for the
//! windowed preview editor. Pure geometry — NO platform types, NO cosmic types.
//! Platform code only FEEDS this (monitor geometry, the source backing scale) and
//! APPLIES the results (the window size it opens, the fit it draws). Both macOS and
//! Linux (COSMIC) route through here, so the two can't drift.
//!
//! The rules (user spec), each enforced in one place below:
//! 1. spawn only as large as needed to fit the media at perfect scale (+ chrome) —
//!    the uniform `scale` in [`spawn_window_size`].
//! 2. never spawn the media LARGER than its natural size — `scale` caps at `1.0`
//!    (window fit) and the display fit caps at the media's LOGICAL-point size
//!    ([`natural_fit`]); together the opening view is ≤ 100%.
//! 3. window height ≤ [`USABLE_H_FRAC`] of the monitor height (clears docks / panels /
//!    menu bars that no client can measure) — the `bound_h` budget.
//! 4. spawn honours the media aspect ratio — the SAME `scale` on both axes.
//! 5. floor at the control-fit minimums (below the floor the aspect may break; the
//!    controls win) — the `.max(min)`.
//! 6. display scaling is handled by working in LOGICAL POINTS (`physical /
//!    source_scale`, see [`to_points`]): the hi-res capture is KEPT and downsampled at
//!    draw time (sharp on hidpi), while every size the user perceives is its true
//!    on-screen size. `source_scale == 1.0` (all Linux 1x, non-Retina panels) makes
//!    every function here the identity, so those paths stay byte-identical.
//! 7. pan/scroll clamping (no overscroll past the media edges) lives in
//!    `widgets::zoom_pan` (`clamp_pan_of`), the one clamp both platforms share.

/// Fraction of the monitor HEIGHT a spawned windowed preview may occupy (rule 3), so
/// it clears the top/bottom panels, docks, trays and menu bars that are unknowable
/// client-side. Applied to the fit bound on BOTH platforms — the single knob that
/// replaced macOS's old `visibleFrame × 0.95` downscale and Linux's reliance on the
/// compositor's own 2/3 reshape.
pub(crate) const USABLE_H_FRAC: f32 = 0.8;

/// Convert PHYSICAL capture pixels to the LOGICAL points the picture occupied on its
/// SOURCE display: `physical / source_scale`, rounded, never zero. `source_scale <=
/// 1.0` (all Linux 1x, non-Retina mac) returns the dims unchanged, keeping every
/// caller byte-identical there. (rule 6)
pub(crate) fn to_points(px: (u32, u32), source_scale: f32) -> (u32, u32) {
    if source_scale <= 1.0 || px.0 == 0 || px.1 == 0 {
        return px;
    }
    (
        (px.0 as f32 / source_scale).round().max(1.0) as u32,
        (px.1 as f32 / source_scale).round().max(1.0) as u32,
    )
}

/// The windowed preview's spawn size (rules 1–5). `media_pts` is the media in LOGICAL
/// points (already divided by the source scale); `monitor` the target output's FULL
/// logical size (`None` = unknown → native-sized, the compositor clamps any overshoot);
/// `chrome` the `(w, h)` reserve drawn OUTSIDE the media canvas (borders, header,
/// toolbars, transport strip); `min` the control-fit floor; `usable_h_frac` the rule-3
/// height budget.
///
/// The canvas is scaled UNIFORMLY (rule 4) by the largest factor that is both ≤ native
/// (rule 2) and fits the usable monitor minus chrome (rules 1 + 3). The final size
/// floors at `min` (rule 5) — only there may the aspect break, controls winning.
pub(crate) fn spawn_window_size(
    media_pts: (u32, u32),
    monitor: Option<(u32, u32)>,
    chrome: (f32, f32),
    min: (f32, f32),
    usable_h_frac: f32,
) -> (f32, f32) {
    let (mw, mh) = (media_pts.0.max(1) as f32, media_pts.1.max(1) as f32);
    // Largest canvas scale that never upscales past native (rule 2)...
    let mut scale = 1.0_f32;
    if let Some((sw, sh)) = monitor {
        // ...and fits the usable monitor minus chrome: full width, 80%-of-height
        // (rule 3). Scaling both axes by this SAME factor preserves the aspect (rule 4).
        let bound_w = (sw as f32 - chrome.0).max(1.0);
        let bound_h = (sh as f32 * usable_h_frac - chrome.1).max(1.0);
        scale = scale.min(bound_w / mw).min(bound_h / mh);
    }
    (
        (mw * scale + chrome.0).max(min.0),
        (mh * scale + chrome.1).max(min.1),
    )
}

// Rule 2 for the DISPLAY (not upscaling a hidpi capture past its natural on-screen
// size in a floored window) is applied at the call sites: they fit the media's
// LOGICAL-point size ([`to_points`] via `PreviewState::frame_points`) with the shared
// `video::fit_dims`, whose no-upscale cap then falls on the natural size, not the
// physical pixels. Keeping the two shared primitives ([`to_points`] +
// [`spawn_window_size`]) here avoids threading `fit_dims` through this pure module.

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME: (f32, f32) = (2.0, 132.0); // ~a still window's border + bars
    const MIN: (f32, f32) = (792.0, 440.0);

    #[test]
    fn to_points_is_identity_at_scale_one_and_divides_on_hidpi() {
        assert_eq!(to_points((800, 600), 1.0), (800, 600));
        assert_eq!(to_points((800, 600), 0.5), (800, 600)); // never upscales
        assert_eq!(to_points((1600, 1200), 2.0), (800, 600)); // retina → logical
        assert_eq!(to_points((900, 600), 1.5), (600, 400)); // fractional
        assert_eq!(to_points((1, 1), 2.0), (1, 1)); // never zeroes
        assert_eq!(to_points((0, 0), 2.0), (0, 0));
    }

    /// Rule 1 + 4: a mid-size picture that fits opens at native, aspect exact.
    #[test]
    fn spawns_native_sized_and_aspect_exact_when_it_fits() {
        let (w, h) = spawn_window_size((1280, 720), Some((3840, 2160)), CHROME, MIN, USABLE_H_FRAC);
        assert!((w - (1280.0 + CHROME.0)).abs() < 0.5);
        assert!((h - (720.0 + CHROME.1)).abs() < 0.5);
        let (cw, ch) = (w - CHROME.0, h - CHROME.1);
        assert!((cw / ch - 1280.0 / 720.0).abs() < 1e-3);
    }

    /// Rule 2: never larger than native, even on a huge monitor.
    #[test]
    fn never_upscales_past_native() {
        let (w, h) = spawn_window_size((640, 480), Some((5120, 1440)), CHROME, MIN, USABLE_H_FRAC);
        // Floors bite here (640 < 792), but the CANVAS never exceeds native.
        assert!(w - CHROME.0 <= 640.0 + 0.5 || w == MIN.0);
        assert!(h - CHROME.1 <= 480.0 + 0.5 || h == MIN.1);
    }

    /// Rule 3: a full-monitor-tall capture is capped at 80% of the monitor height.
    #[test]
    fn caps_height_at_80_percent_of_the_monitor() {
        for (mon, media) in [((2560u32, 1440u32), (2560u32, 1440u32)), ((3840, 2160), (3840, 2160))] {
            let (_, h) = spawn_window_size(media, Some(mon), CHROME, MIN, USABLE_H_FRAC);
            let cap = mon.1 as f32 * USABLE_H_FRAC;
            assert!(h <= cap + 0.5, "height {h} exceeded 80% cap {cap} on monitor {mon:?}");
            // The canvas fills that budget (minus chrome), not less.
            assert!(h >= cap - 0.5, "height {h} did not reach the 80% budget {cap}");
        }
    }

    /// Rule 4: the canvas keeps the media aspect on every output (above the floor).
    #[test]
    fn preserves_aspect_across_outputs_and_media() {
        for output in [(3840, 2160), (5120, 1440), (2560, 1440), (1920, 1080), (3440, 1440)] {
            for media in [(3840u32, 2160u32), (5120, 1440), (1920, 1080), (1280, 720)] {
                let (w, h) = spawn_window_size(media, Some(output), CHROME, MIN, USABLE_H_FRAC);
                if w > MIN.0 && h > MIN.1 {
                    let (cw, ch) = (w - CHROME.0, h - CHROME.1);
                    let want = media.0 as f32 / media.1 as f32;
                    assert!(
                        (cw / ch - want).abs() < 1e-3,
                        "aspect drift: output {output:?} media {media:?} canvas {cw}x{ch}"
                    );
                    // And it fits the usable monitor.
                    assert!(w <= output.0 as f32 + 0.5);
                    assert!(h <= output.1 as f32 * USABLE_H_FRAC + 0.5);
                }
            }
        }
    }

    /// Rule 5: tiny media floors at the control minimums (controls never clip).
    #[test]
    fn floors_at_the_control_minimums() {
        let (w, h) = spawn_window_size((200, 100), Some((1920, 1080)), CHROME, MIN, USABLE_H_FRAC);
        assert_eq!((w, h), MIN);
    }

    /// Rule 6: a 2× (Retina) or 1.5× (fractional) capture spawns at its LOGICAL size,
    /// not its physical size — identical windows for the same on-screen footprint
    /// regardless of the backing scale. This is the whole cross-platform-drift fix:
    /// feed physical dims through `to_points`, and hidpi matches 1x.
    #[test]
    fn hidpi_spawns_logical_sized_matching_1x() {
        let monitor = Some((6000u32, 3400u32)); // roomy, so nothing clamps/floors
        let native = spawn_window_size((1400, 900), monitor, CHROME, MIN, USABLE_H_FRAC);
        for scale in [2.0f32, 1.5, 1.25] {
            let physical = ((1400.0 * scale) as u32, (900.0 * scale) as u32);
            let logical = to_points(physical, scale);
            let got = spawn_window_size(logical, monitor, CHROME, MIN, USABLE_H_FRAC);
            assert!((got.0 - native.0).abs() < 2.0, "scale {scale}: w {got:?} vs {native:?}");
            assert!((got.1 - native.1).abs() < 2.0, "scale {scale}: h {got:?} vs {native:?}");
        }
    }

    /// No monitor known → native sized (the compositor clamps any overshoot later).
    #[test]
    fn without_a_monitor_is_native_sized() {
        let (w, h) = spawn_window_size((1600, 900), None, CHROME, MIN, USABLE_H_FRAC);
        assert!((w - (1600.0 + CHROME.0)).abs() < 0.5);
        assert!((h - (900.0 + CHROME.1)).abs() < 0.5);
    }
}
