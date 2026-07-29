//! What the GPU will actually accept, observed at draw time (DRAGON-401).
//!
//! The preview draws its dynamic pixel content through custom wgpu shader primitives
//! (the preview's `layers::LayerStack` and [`super::annotation_fx`]). `iced_wgpu` renders such
//! a primitive by setting the render pass's VIEWPORT to the primitive's bounds:
//!
//! ```text
//! let bounds = instance.bounds * scale;          // logical bounds x window scale factor
//! render_pass.set_viewport(bounds.x, bounds.y, bounds.width, bounds.height, 0.0, 1.0);
//! ```
//!
//! and `instance.bounds` is the widget's layout rect AFTER the active renderer
//! transformation — which, inside [`crate::widgets::ZoomPan`], is the view zoom. wgpu
//! validates a viewport against `max_texture_dimension_2d`, so past a certain zoom the call
//! is a hard validation error and the process panics mid-frame:
//!
//! ```text
//! Viewport size { w: 9183.373, h: 5036.941 } greater than device's
//! requested `max_texture_dimension_2d` limit 8192, or less than zero
//! ```
//!
//! ## There are TWO viewport producers, and the tighter one is OURS (DRAGON-401 fix 2)
//!
//! The formula above is `iced_wgpu`'s, and a clamp that lands the extent exactly ON the limit
//! satisfies it (the validation rejects `> limit`, not `>= limit`). It still crashed, by exactly
//! one pixel:
//!
//! ```text
//! Viewport size { w: 8193, h: 4491 } greater than device's
//! requested `max_texture_dimension_2d` limit 8192, or less than zero
//! ```
//!
//! Note the EXACT INTEGERS — wgpu prints those two fields as the `f32`s it was handed, so a
//! whole number means the caller passed a whole number, which `bounds * scale` essentially never
//! does. That call is not iced's: it is [`crate::widgets::annotation_fx`]'s own `set_viewport`,
//! which SNAPS the rect out to the physical pixel grid before issuing it — floor the origin, ceil
//! the far edge, so the dim/effect blit fully covers a base image that rasterized to whole pixels
//! (without it a ~1px undimmed seam shows on the right/bottom). [`snap_axis`] is that snap, and it
//! is why the extent the GPU actually sees is not `bounds.width * scale` but
//!
//! ```text
//! ceil((x + width) * scale) - floor(x * scale)
//! ```
//!
//! which is up to **two pixels larger** — one from each rounding — and depends on the PAN (the
//! origin's fractional part), which the clamp cannot know. So the old model was not wrong about
//! the arithmetic; it was modelling the wrong producer. Landing exactly on 8192 with a fractional
//! origin snapped to 8193, and the process died. [`usable_viewport_extent`] is the budget that
//! makes it impossible: the device limit less that two-pixel snap slack, less one more pixel of
//! deliberate margin.
//!
//! Nothing here ALLOCATES at that size — `set_viewport` only defines the NDC mapping, and the
//! layer textures are already capped at the preview's `MAX_LAYER_DIM`. The limit is purely a
//! validation rule on the viewport rect, and it is the one thing that bounds how far the
//! preview can zoom. See [`max_zoom_for_device`] for the arithmetic and
//! the preview's `App::max_view_zoom` for where it is applied.
//!
//! ## Why the numbers are OBSERVED rather than assumed
//!
//! `8192` is one machine's limit; wgpu reports the real one per device. The app never holds a
//! `wgpu::Device` — iced owns it — but every shader `prepare` is handed one, along with the
//! render `Viewport` (which carries the window's scale factor). So both facts are recorded
//! here from inside `prepare`, where they are free, and read back from the ordinary
//! view/update code, which is the only place that can act on them.
//!
//! The scale factor is kept as the MAXIMUM seen rather than the latest. Several preview
//! windows can be open on displays of different DPI, and a single global cannot tell them
//! apart; the maximum is the conservative choice — it can only make the zoom ceiling of a
//! low-DPI window lower than strictly necessary, never higher than is safe. (A per-window map
//! would be exact, but it would buy a slightly higher ceiling on a mixed-DPI multi-preview
//! desktop in exchange for shared mutable state on the draw path.)

use std::sync::atomic::{AtomicU32, Ordering};

use cosmic::iced::wgpu;
use cosmic::iced::widget::shader::Viewport;

/// wgpu's own DEFAULT `max_texture_dimension_2d`, and what is assumed until a real device has
/// reported ([`observe`] runs on the first shader prepare, i.e. the preview's very first
/// frame, so the assumption only ever covers a state in which nothing has been drawn yet).
/// It is deliberately the same 8192 that the preview's `MAX_LAYER_DIM` encodes for layer
/// rasters — the two are the same GPU constraint seen from two sides.
pub const DEFAULT_MAX_TEXTURE_DIM: u32 = 8192;

static MAX_TEXTURE_DIM: AtomicU32 = AtomicU32::new(DEFAULT_MAX_TEXTURE_DIM);
/// The largest render scale factor seen, as `f32::to_bits`. For POSITIVE floats the bit
/// pattern orders the same way the value does, so `fetch_max` on the bits is a correct
/// running maximum without a CAS loop.
static RENDER_SCALE_BITS: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

/// Record what this device/frame can accept. Called from every shader `prepare` in the
/// preview (they are the primitives whose viewport the zoom scales); cheap enough to run per
/// frame — two relaxed atomics and a `Limits` read.
pub fn observe(device: &wgpu::Device, viewport: &Viewport) {
    MAX_TEXTURE_DIM.store(device.limits().max_texture_dimension_2d, Ordering::Relaxed);
    let scale = (viewport.scale_factor() as f32).max(1.0);
    RENDER_SCALE_BITS.fetch_max(scale.to_bits(), Ordering::Relaxed);
}

/// The device's `max_texture_dimension_2d` — the hard ceiling on a `set_viewport` extent.
pub fn max_texture_dim() -> u32 {
    MAX_TEXTURE_DIM.load(Ordering::Relaxed)
}

/// The window scale factor `iced_wgpu` multiplies logical bounds by before `set_viewport`
/// (the maximum seen — see the module doc).
pub fn render_scale() -> f32 {
    f32::from_bits(RENDER_SCALE_BITS.load(Ordering::Relaxed))
}

/// One axis of a logical rect, snapped to the physical pixel grid: `(origin, extent)` in
/// physical px, expanded OUTWARD (floor the origin, ceil the far edge).
///
/// This IS the arithmetic [`crate::widgets::annotation_fx`] issues its `set_viewport` with — it
/// calls this function, so the clamp below and the draw path can never drift apart. The
/// expansion is deliberate (it makes the dim/effect blit fully cover a base image that
/// rasterized to whole pixels; a raw `bounds * scale` leaves a ~1px seam on the right/bottom),
/// and it is exactly why the zoom ceiling must budget for it: the result is up to
/// [`VIEWPORT_SNAP_SLACK`] px wider than `extent * scale`, by an amount that depends on the pan.
pub fn snap_axis(origin: f32, extent: f32, scale: f32) -> (f32, f32) {
    let a = (origin * scale).floor();
    let b = ((origin + extent) * scale).ceil();
    (a, b - a)
}

/// Physical px [`snap_axis`] can add to an extent: one for the floored origin, one for the
/// ceiled far edge. The clamp must reserve them because it cannot know the pan the snap will be
/// applied at.
pub const VIEWPORT_SNAP_SLACK: u32 = 2;

/// One more physical px, reserved on purpose (DRAGON-401 fix 2). Being exactly right at this
/// boundary is fragile — layout bounds are fractional, scale factors need not be integral, and
/// a rounding that goes the other way costs the whole process. One pixel of zoom range at an
/// 8192 limit is 0.04% of the ceiling: imperceptible. A crash is not.
pub const VIEWPORT_SAFETY_MARGIN: u32 = 1;

/// The physical extent the zoom clamp may actually spend, given a device `max_texture_dimension_2d`
/// of `limit`: the limit less the snap slack and the safety margin. Saturating, so an absurdly
/// small reported limit yields `0` (no zoom range) rather than wrapping into a huge one.
pub fn usable_viewport_extent(limit: u32) -> f32 {
    limit.saturating_sub(VIEWPORT_SNAP_SLACK + VIEWPORT_SAFETY_MARGIN) as f32
}

/// The highest fit-relative zoom at which the media stack's PHYSICAL extent still satisfies
/// the device's `max_texture_dimension_2d`.
///
/// The media stack always renders the WHOLE source frame (a crop just frames a sub-region of
/// it — see `preview/image.rs`), so its logical size is `frame x points_per_source_px`, and
/// the physical viewport `iced_wgpu` derives from it is
///
/// ```text
/// frame x points_per_source_px x zoom x render_scale
/// ```
///
/// on each axis — and then the effects shader snaps that rect out to the pixel grid
/// ([`snap_axis`]), which can add up to [`VIEWPORT_SNAP_SLACK`] px more. So the extent the clamp
/// may spend is not `limit` but [`usable_viewport_extent`], and requiring the LARGER axis to stay
/// within it gives
///
/// ```text
/// zoom <= usable_viewport_extent(limit)
///         / (max(frame_w, frame_h) x points_per_source_px x render_scale)
/// ```
///
/// Pure arithmetic, so it is unit-tested directly. Degenerate inputs (a frame or scale of
/// zero, i.e. media whose dimensions are not known yet) return [`f32::INFINITY`] — "this
/// constraint does not bind" — which leaves the caller's own ceiling in charge rather than
/// collapsing the range to nothing.
///
/// The caller floors the result at fit (the preview viewport's `FIT`): showing the whole picture
/// must always be possible. That floor is not a licence to overflow — it simply cannot help.
/// If the FITTED media alone exceeds `limit` (only reachable by cropping to a tiny region,
/// which scales the whole frame up by `display_size / crop_size`), no zoom ceiling can save
/// the frame; that is a distinct problem from the one this bounds.
pub fn max_zoom_for_device(
    frame: (u32, u32),
    points_per_source_px: f32,
    render_scale: f32,
    limit: u32,
) -> f32 {
    let long = frame.0.max(frame.1) as f32;
    let denom = long * points_per_source_px * render_scale;
    // NaN-safe by construction: `is_finite` rejects NaN/inf, and the `> 0.0` guard the
    // remaining non-positive cases.
    if !denom.is_finite() || denom <= 0.0 {
        return f32::INFINITY;
    }
    usable_viewport_extent(limit) / denom
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The physical viewport extent the RENDERER ends up issuing for a media stack of
    /// `frame x points_per_source_px` at `zoom`, on a `scale` window, when the widget's origin
    /// happens to sit `origin_frac` of a logical pixel off the grid — i.e. the whole chain the
    /// clamp is responsible for, computed the way the draw path computes it ([`snap_axis`],
    /// which `annotation_fx` itself calls). The test net for every case below.
    fn renderer_extent(
        frame: (u32, u32),
        points_per_source_px: f32,
        zoom: f32,
        scale: f32,
        origin_frac: f32,
    ) -> f32 {
        let long = frame.0.max(frame.1) as f32;
        snap_axis(origin_frac, long * points_per_source_px * zoom, scale).1
    }

    /// **The one-pixel crash (DRAGON-401 fix 2)**, and the property every case here asserts: at
    /// the permitted ceiling the extent the renderer actually issues stays STRICTLY inside the
    /// device limit — for any pan, i.e. any fractional origin the pixel-grid snap is applied at.
    ///
    /// The owner's second crash is the first row: 2642x1448, limit 8192. The old ceiling put the
    /// extent exactly ON 8192, which `snap_axis` then expanded to 8193 and wgpu rejected. Note
    /// this asserts `< limit`, not `<= limit`: the validation rejects only `> limit`, so landing
    /// on it is legal — and that is precisely the assumption that failed, so the test refuses it.
    #[test]
    fn the_permitted_ceiling_stays_strictly_inside_the_limit_at_any_pan() {
        // (frame, fitted points-per-source-px, device limit, window scale)
        let cases: &[((u32, u32), f32, u32, f32)] = &[
            // The owner's exact capture + limit (the crash of record), at its fitted scale.
            ((2642, 1448), 1140.0 / 1448.0, 8192, 1.0),
            // ...and the first crash's capture, which fix 1 sized to land exactly on the limit.
            ((2640, 1448), 2078.0 / 2640.0, 8192, 1.0),
            // Hidpi windows: the same geometry with the scale doing the multiplying.
            ((2642, 1448), 1140.0 / 1448.0, 8192, 2.0),
            ((2642, 1448), 1140.0 / 1448.0, 8192, 1.25),
            ((2642, 1448), 1140.0 / 1448.0, 8192, 1.5),
            // A lower-limit GPU, and a higher-limit one.
            ((2642, 1448), 1140.0 / 1448.0, 4096, 1.0),
            ((2642, 1448), 1140.0 / 1448.0, 16384, 1.0),
            // A wide multi-monitor grab, a tall portrait one, a small capture, an odd one.
            ((7680, 2160), 2000.0 / 7680.0, 8192, 1.0),
            ((1440, 3840), 900.0 / 3840.0, 8192, 1.75),
            ((1280, 720), 1.0, 8192, 1.0),
            ((3001, 1999), 0.6667, 8192, 1.0),
        ];
        for &(frame, s, limit, scale) in cases {
            let max = max_zoom_for_device(frame, s, scale, limit);
            // Any pan puts the origin anywhere within a pixel; the snap's worst case is what
            // must still fit, so sweep the fraction rather than assuming a friendly one.
            for frac in [0.0f32, 0.001, 0.25, 0.5, 0.75, 0.999] {
                let extent = renderer_extent(frame, s, max, scale, frac);
                assert!(
                    extent < limit as f32,
                    "{frame:?} s {s} limit {limit} scale {scale} frac {frac}: the renderer would \
                     issue {extent}, which is not strictly inside {limit}"
                );
            }
        }
    }

    /// The ceiling is not merely safe but TIGHT: it spends everything the budget allows, so the
    /// margin cannot quietly grow into a visibly shorter zoom range. At the permitted zoom the
    /// unsnapped extent is within a pixel of [`usable_viewport_extent`], and a hair more zoom
    /// (0.1%) already overshoots that budget — this is a boundary, not a rounded-down guess.
    #[test]
    fn the_ceiling_spends_the_whole_budget_and_no_more() {
        let frame = (2642, 1448);
        let s = 1140.0 / 1448.0;
        let max = max_zoom_for_device(frame, s, 1.0, 8192);
        let unsnapped = frame.0 as f32 * s * max;
        let budget = usable_viewport_extent(8192);
        assert!((unsnapped - budget).abs() < 1.0, "{unsnapped} vs budget {budget}");
        assert!(frame.0 as f32 * s * (max * 1.001) > budget, "the ceiling must be the boundary");
        // Only 3px of 8192 are held back — 0.04% of the range, invisible to the user.
        assert_eq!(budget, 8189.0);
    }

    /// The snap is what made the old model a pixel short, so pin its shape directly: it expands
    /// OUTWARD, never inward, and never by more than [`VIEWPORT_SNAP_SLACK`].
    #[test]
    fn the_pixel_grid_snap_expands_outward_by_at_most_the_slack() {
        for &(origin, extent, scale) in &[
            (0.0f32, 8192.0f32, 1.0f32),
            (0.5, 8192.0, 1.0),   // the crash: exactly on the limit, half a pixel off the grid
            (0.999, 8192.0, 1.0), // the worst case for the floored origin
            (-1000.4, 4489.8, 1.0), // panned left (negative origin floors away from zero)
            (3.3, 1024.7, 2.0),
            (0.4, 700.25, 1.5),
        ] {
            let (_, snapped) = snap_axis(origin, extent, scale);
            let raw = extent * scale;
            assert!(snapped >= raw, "the snap must never shrink the rect ({snapped} < {raw})");
            assert!(
                snapped <= raw + VIEWPORT_SNAP_SLACK as f32,
                "origin {origin} extent {extent} scale {scale}: snapped {snapped} exceeds \
                 {raw} + {VIEWPORT_SNAP_SLACK}"
            );
            assert_eq!(snapped, snapped.round(), "the snapped extent is a whole pixel count");
        }
        // The crash of record, exactly: an extent sitting ON the limit becomes limit + 1.
        assert_eq!(snap_axis(0.5, 8192.0, 1.0).1, 8193.0);
    }

    /// The owner's FIRST crash (DRAGON-401): a 2640x1448 capture fitted to 2078 points wide
    /// on a 1x display, against this GPU's 8192 limit. The reported failure was a
    /// 9183.373-wide viewport, i.e. 3.478x the source; the ceiling must reject it and must
    /// still leave the picture usefully zoomable.
    #[test]
    fn the_owners_capture_caps_below_the_zoom_that_crashed() {
        let frame = (2640, 1448);
        let s = 2078.0 / 2640.0; // fitted points per source pixel (~0.787)
        let max = max_zoom_for_device(frame, s, 1.0, 8192);
        // The crash was at 3.478x source = zoom 4.42; the ceiling is below it.
        assert!(max < 3.478 / s, "the crashing zoom must be excluded (cap {max})");
        // ...and comfortably above fit, so the raise DRAGON-400 asked for still bites.
        assert!(max > 3.9 && max < 4.0, "cap {max} (~3.94 = 8189 / (2640 x 0.787))");
    }

    /// A large multi-monitor grab: the cap bites hard — well below the 500% (DRAGON-400)
    /// ceiling, but still above fit, so the picture stays fully usable.
    #[test]
    fn a_wide_multi_monitor_capture_caps_hard_but_stays_above_fit() {
        // 7680x2160 (three 2560x1440 panels side by side, scaled), fitted into ~2000 points.
        let frame = (7680u32, 2160u32);
        let s = 2000.0 / 7680.0; // ~0.26
        let max = max_zoom_for_device(frame, s, 1.0, 8192);
        assert!(max > 1.0, "fit must remain reachable (cap {max})");
        // The advertised 500% visual on this capture would be 5.0 / 0.26 = ~19.2 — the GPU
        // ceiling of ~4.1 is what the user actually gets, and the readout must say so.
        assert!(max < 5.0 / s, "the GPU ceiling, not MAX_VISUAL, is the binding one here");
    }

    /// A lower-limit GPU crashes SOONER, so the cap must track the reported limit rather than
    /// a hardcoded 8192 — halving the limit halves the reachable zoom.
    ///
    /// Only APPROXIMATELY halves it, since DRAGON-401's fix 2: the reserve
    /// ([`usable_viewport_extent`]) is a fixed few pixels, not a fraction, so a smaller limit
    /// gives up proportionally slightly more of itself. Asserted against the budgets rather than
    /// against the raw limits, which is where the exact halving genuinely lives.
    #[test]
    fn a_lower_device_limit_lowers_the_ceiling_proportionally() {
        let frame = (2640, 1448);
        let s = 2078.0 / 2640.0;
        let at_8192 = max_zoom_for_device(frame, s, 1.0, 8192);
        let at_4096 = max_zoom_for_device(frame, s, 1.0, 4096);
        let ratio = usable_viewport_extent(8192) / usable_viewport_extent(4096);
        assert!((at_8192 / at_4096 - ratio).abs() < 1e-4, "{at_8192} vs {at_4096}");
        assert!((ratio - 2.0).abs() < 1e-3, "the reserve costs well under a part in a thousand");
        let at_16384 = max_zoom_for_device(frame, s, 1.0, 16384);
        assert!((at_16384 / at_8192 - 2.0).abs() < 1e-3);
    }

    /// A 2x window doubles the physical extent of the same logical bounds, so it halves the
    /// reachable zoom — the mac/hidpi case the app-side geometry alone cannot see.
    #[test]
    fn a_hidpi_window_halves_the_ceiling() {
        let frame = (2640, 1448);
        let s = 2078.0 / 2640.0;
        let at_1x = max_zoom_for_device(frame, s, 1.0, 8192);
        let at_2x = max_zoom_for_device(frame, s, 2.0, 8192);
        assert!((at_1x / at_2x - 2.0).abs() < 1e-4, "{at_1x} vs {at_2x}");
    }

    /// The bound is on the LONGER axis: a tall capture is limited by its height, a wide one by
    /// its width — the same cap either way once transposed.
    #[test]
    fn the_longer_axis_is_what_binds() {
        let wide = max_zoom_for_device((4000, 1000), 0.5, 1.0, 8192);
        let tall = max_zoom_for_device((1000, 4000), 0.5, 1.0, 8192);
        assert!((wide - tall).abs() < 1e-6, "{wide} vs {tall}");
        assert!(
            (wide - usable_viewport_extent(8192) / 2000.0).abs() < 1e-4,
            "bound by the 4000px axis"
        );
    }

    /// The budget is the device limit less the snap slack and the deliberate margin — and it
    /// saturates rather than wrapping if a device ever reports a limit smaller than the reserve.
    #[test]
    fn the_usable_extent_reserves_the_snap_slack_and_the_margin() {
        assert_eq!(usable_viewport_extent(8192), 8189.0);
        assert_eq!(usable_viewport_extent(4096), 4093.0);
        assert_eq!(usable_viewport_extent(3), 0.0);
        assert_eq!(usable_viewport_extent(0), 0.0);
        // A zero budget means "no zoom range", never a negative or wrapped one; the caller
        // floors at fit, so the picture is still shown whole.
        assert_eq!(max_zoom_for_device((2642, 1448), 1.0, 1.0, 2), 0.0);
    }

    /// Media whose dimensions are not known yet must not collapse the zoom range — an unknown
    /// constraint does not bind.
    #[test]
    fn degenerate_geometry_does_not_bind() {
        assert_eq!(max_zoom_for_device((0, 0), 1.0, 1.0, 8192), f32::INFINITY);
        assert_eq!(max_zoom_for_device((2640, 1448), 0.0, 1.0, 8192), f32::INFINITY);
        assert_eq!(max_zoom_for_device((2640, 1448), 1.0, 0.0, 8192), f32::INFINITY);
    }

    /// The observed values start at wgpu's own defaults, so the very first frame (before any
    /// prepare has run) is bounded by a real number rather than by nothing.
    #[test]
    fn the_defaults_are_wgpus_own() {
        assert_eq!(DEFAULT_MAX_TEXTURE_DIM, wgpu::Limits::default().max_texture_dimension_2d);
        assert!(max_texture_dim() >= DEFAULT_MAX_TEXTURE_DIM.min(max_texture_dim()));
        assert!(render_scale() >= 1.0);
    }
}
