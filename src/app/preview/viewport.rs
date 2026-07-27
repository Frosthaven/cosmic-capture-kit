//! The preview viewport: zoom/pan state (`Viewport`), the fit/viewport/pan
//! math every consumer shares, and the zoom-scale control.
//! Split from `preview/mod.rs` (DRAGON-115) — pure code motion.

use super::*;

/// The pan/zoom of the preview image. `zoom` 1.0 = fit (default), `pan` in screen px.
#[derive(Clone, Copy)]
pub struct Viewport {
    pub zoom: f32,
    pub pan: (f32, f32),
    /// Pan tool active: a plain left-drag pans (the grabby-hand mode) instead of the normal
    /// pointer. Alt+drag pans in either mode.
    pub pan_mode: bool,
    /// The zoom-scale dropdown selection: `Some(i)` = a preset is exactly applied, `None` =
    /// an in-between zoom (slider drag / scroll). Drives the combo's current label.
    pub zoom_preset: Option<usize>,
    /// The zoom preset menu (combo popover) is open.
    pub zoom_menu_open: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: (0.0, 0.0),
            pan_mode: false,
            zoom_preset: Some(0),
            zoom_menu_open: false,
        }
    }
}

impl Viewport {
    /// The zoom range, as a multiple of the FIT size. The floor is HALF the fit (0.5) — the
    /// user can zoom OUT to half the whole-picture-fit size (DRAGON: preview-editor polish).
    /// Below fit the picture is smaller than the viewport, so it just recentres (no pan). The
    /// ceiling is a hard cap; the effective max is the 200%-visual limit (see
    /// `App::max_view_zoom`), which is what actually bounds zoom-IN.
    pub(super) const MIN: f32 = 0.5;
    pub(super) const MAX: f32 = 64.0;
    /// The "fit" multiplier (whole picture visible) — the recentre point.
    pub(super) const FIT: f32 = 1.0;

    /// Set the zoom multiplier directly (slider / preset), clamped. At or below fit the
    /// picture fully fits (no overflow), so recentre — drop any pan.
    pub(super) fn set_zoom(&mut self, z: f32) {
        self.zoom = z.clamp(Self::MIN, Self::MAX);
        if self.zoom <= Self::FIT {
            self.pan = (0.0, 0.0);
        }
    }

}

/// The zoom-scale dropdown's presets. `Fit to screen` (`None`) fits the whole picture; the
/// rest are VISUAL fractions — `100%` is the picture's true on-screen size (what the user
/// saw when capturing), NOT physical 1:1. On a 2× Retina capture, `100%` = natural size
/// (physical 1:1 would read `200%`); on Linux / 1× panels a visual fraction equals a native
/// fraction, so these are byte-identical to the pre-visual-units behaviour.
pub(super) const ZOOM_PRESET_LABELS: [&str; 5] =
    ["Fit", "100%", "125%", "150%", "200%"];

/// The presets as VISUAL fractions (`1.0` = natural on-screen size). Converted to the
/// viewport's fit-relative multiplier via `visual_scale` (see [`App::preview_visual_scale`]).
pub(super) const ZOOM_PRESET_VISUAL: [Option<f32>; 5] =
    [None, Some(1.0), Some(1.25), Some(1.5), Some(2.0)];

/// Compose the user-facing VISUAL scale from the internal `fit_scale` (physical-pixel
/// fraction at fit) and the capture's `source_scale` (backing scale; `1.0` on Linux/1×).
/// `visual_scale = fit_scale × source_scale`, floored so it can't zero out. This is the
/// ONE seam between physical geometry and the visual-units readout/presets — at
/// `source_scale = 1.0` it returns `fit_scale` unchanged, so Linux is byte-identical.
pub(super) fn visual_scale(fit_scale: f32, source_scale: f32) -> f32 {
    let src = if source_scale > 0.0 { source_scale } else { 1.0 };
    (fit_scale * src).max(0.0001)
}

/// The internal fit-relative zoom that renders a preset's VISUAL fraction: `zoom =
/// frac / visual_scale`. `None` (Fit to screen) → the fit multiplier. So "100%" (frac
/// `1.0`) targets natural on-screen size (`zoom = 1/visual_scale`), which on a 2× capture
/// is the fit size, and physical 1:1 lives at "200%".
pub(super) fn preset_zoom(visual_frac: Option<f32>, visual_scale: f32) -> f32 {
    match visual_frac {
        Some(frac) => frac / visual_scale.max(0.0001),
        None => Viewport::FIT,
    }
}

/// The user-facing percent shown in the readout: `zoom × visual_scale × 100`, rounded.
/// 100% is the picture's true on-screen size regardless of capture DPI.
pub(super) fn displayed_percent(zoom: f32, visual_scale: f32) -> i32 {
    (zoom * visual_scale * 100.0).round() as i32
}

/// Snap a slider/scroll zoom to EXACTLY 100% on-screen (natural size) when it lands within a
/// couple display-percent of it — a magnetic detent so the user can hit 100% precisely.
/// Returns the input unchanged when it isn't near 100%.
pub(super) fn snap_to_hundred(zoom: f32, visual_scale: f32) -> f32 {
    if (zoom * visual_scale * 100.0 - 100.0).abs() <= 2.5 {
        preset_zoom(Some(1.0), visual_scale) // the zoom whose displayed% == 100
    } else {
        zoom
    }
}

impl App {
    /// The pixel height available for the content between the edit bar above and the
    /// toolbar below, per the surface's REAL chrome ([`PreviewSurface::chrome_h`]).
    /// Sharing the exact chrome with [`windowed_fit_size`] is what lets a windowed
    /// preview open media-tight with no dead bands above/below the picture.
    pub(super) fn preview_content_height(&self, preview: &PreviewState) -> f32 {
        (preview.monitor.1 as f32 - preview.surface.chrome_h()).max(1.0)
    }

    /// The fraction of native (PHYSICAL-pixel) size the picture is displayed at when FIT
    /// (ScaleDown) into the content area. The internal building block for pan/zoom geometry;
    /// the USER-FACING percent and presets go through [`Self::preview_visual_scale`] instead
    /// (visual units). `1.0` when the media dims aren't known yet (still loading).
    pub(super) fn preview_fit_scale(&self, preview: &PreviewState) -> f32 {
        let (iw, ih) = preview.edit.frame;
        if iw == 0 || ih == 0 {
            return 1.0;
        }
        // Fit the media's NATURAL (logical-point) size, then express it as a fraction of
        // the PHYSICAL pixels (the divisor). A floored hidpi window thus reads ≤ 100%
        // visual at fit — the picture is shown at its natural size, not physical 1:1
        // (rule 2, DRAGON-221). `source_scale == 1.0` (Linux 1x) makes points ==
        // physical, so `dw / iw` is byte-identical to before.
        let (pw, ph) = preview.frame_points();
        let (avail_w, avail_h) = self.preview_viewport(preview);
        let (dw, _) = video::fit_dims(pw.max(1), ph.max(1), avail_w, avail_h);
        (dw / iw as f32).clamp(0.0001, 1.0)
    }

    /// The fraction of the picture's TRUE ON-SCREEN (visual) size it is displayed at when
    /// FIT — the bridge between the viewport's fit-relative zoom and the user-facing
    /// percent/preset scale, which is expressed in VISUAL units (100% = the natural size
    /// the picture had on its source display, matching what the user saw).
    ///
    /// `visual_scale = fit_scale × source_scale`. `fit_scale` is the fraction of PHYSICAL
    /// pixels shown; `source_scale` (the source display's backing scale, DRAGON-130)
    /// converts physical to visual. On a 2× capture, fitting the whole picture is its
    /// natural size, so `fit_scale ≈ 0.5`, `source_scale = 2.0`, and `visual_scale ≈ 1.0`
    /// → the readout reads 100% at fit. On Linux (and any 1× panel) `source_scale = 1.0`,
    /// so this equals `fit_scale` and every downstream percent/preset/max is byte-identical
    /// to before the visual-units change.
    pub(super) fn preview_visual_scale(&self, preview: &PreviewState) -> f32 {
        visual_scale(self.preview_fit_scale(preview), preview.source_scale)
    }

    /// The image canvas viewport (px): the area the ZoomPan actually fills, and the
    /// single source the fit-scale / pan-bound / view code all read. Windowed spans
    /// the full window (the window itself was media-fitted at open); the overlay
    /// gets the media FITTED into the available area so its toolbars hug the
    /// picture instead of pinning to the monitor's extremes — floored at the
    /// toolbar groups' own needs and the shared `PREVIEW_MIN_W` so the controls
    /// never undersize. Media-less states (spinner, failed probe) keep the full box.
    pub(super) fn preview_viewport(&self, preview: &PreviewState) -> (f32, f32) {
        // Videos carry a transport strip below the canvas — reserve it here so
        // every consumer (fit scale, pan bounds, the views) sizes the media into
        // what is genuinely left.
        let transport = preview_transport_h(preview);
        if preview.surface.is_window() {
            // The window canvas is `Length::Fill` (the REAL space left by the native header +
            // toolbars), but we fit the image using this estimated height. If `chrome_h()`'s
            // header estimate (`header_px()`) under-counts libcosmic's real header by a few px,
            // the fit lands a hair too tall and the image's top/bottom rows clip. A small guard
            // keeps the fit just inside the real canvas (imperceptible, and only the windowed
            // path — the overlay's Fixed-height column is exact and stays untouched).
            const WINDOW_FIT_GUARD: f32 = 8.0;
            return (
                (preview.monitor.0 as f32).max(1.0),
                (self.preview_content_height(preview) - transport - WINDOW_FIT_GUARD).max(1.0),
            );
        }
        let avail = (
            (preview.monitor.0 as f32 - 80.0).max(1.0),
            (self.preview_content_height(preview) - transport).max(1.0),
        );
        let min_w = overlay_min_content_width(preview).max(super::shell::PREVIEW_MIN_W);
        // Hug the sizing media: a video's captured footprint (the encode upscales
        // back into it), a still's decoded pixels — in LOGICAL points (the physical
        // dims divided by the source display's backing scale, so a macOS Retina grab
        // hugs at its true on-screen size, not 2×; `source_scale` is always 1.0 on
        // Linux, keeping this byte-identical there).
        overlay_fit_box(preview.sizing_media_points(), avail, min_w)
    }

    /// The pan limits `((min_x, max_x), (min_y, max_y))` for the current zoom. The picture
    /// is centred in the FULL viewport, but the scrollbars sit on the right/bottom, so the
    /// limits are ASYMMETRIC: the right/bottom side gets an extra reserve so those edges can
    /// be panned out from under the bars, while the left/top just reach the edge.
    pub(super) fn preview_pan_bounds(&self, preview: &PreviewState) -> ((f32, f32), (f32, f32)) {
        // The displayed picture is fitted at its natural (logical-point) size (rule 2),
        // so the pan range clamps against THAT — matching the ZoomPan widget's own
        // `content_px`, the authoritative clamp (DRAGON-221). Byte-identical at scale 1.
        let (iw, ih) = preview.frame_points();
        let (vw, vh) = self.preview_viewport(preview);
        let (dw, dh) = video::fit_dims(iw.max(1), ih.max(1), vw, vh);
        let z = preview.view.zoom;
        let (cw, ch) = (dw * z, dh * z);
        let base_x = ((cw - vw) * 0.5).max(0.0);
        let base_y = ((ch - vh) * 0.5).max(0.0);
        // Vertical bar (right) shows on vertical overflow → reserve on the right (x); the
        // horizontal bar (bottom) shows on horizontal overflow → reserve on the bottom (y).
        let rev_x = if ch > vh + 0.5 { crate::widgets::zoom_pan::SCROLLBAR_TOTAL } else { 0.0 };
        let rev_y = if cw > vw + 0.5 { crate::widgets::zoom_pan::SCROLLBAR_TOTAL } else { 0.0 };
        (
            (-(base_x + rev_x), base_x),
            (-(base_y + rev_y), base_y),
        )
    }

    /// The maximum view zoom (fit-relative) — the 200%-VISUAL cap: displayed visual
    /// fraction = zoom × visual_scale, so zoom for 200% visual = 2.0 / visual_scale. Never
    /// below fit. On a 2× capture, 200% visual is exactly physical 1:1 (`source_scale = 2`,
    /// so `visual_scale ≈ 1.0` at fit and the ceiling ≈ 2.0), keeping the actual-pixels view
    /// reachable. On Linux (`source_scale = 1.0`) this is `2.0 / fit` — byte-identical.
    pub(super) fn max_view_zoom(&self, preview: &PreviewState) -> f32 {
        (2.0 / self.preview_visual_scale(preview)).max(Viewport::FIT)
    }

    /// The zoom FLOOR: the multiplier whose DISPLAYED percent is 50% (`0.5 / visual_scale`),
    /// capped at fit so a picture whose fit already reads below 50% can still be shown whole.
    /// Zooming OUT bottoms out at 50% on-screen regardless of how far the fit downscaled the
    /// capture — a large grab fits at e.g. 78%, and half of THAT would read 39%, not 50%.
    pub(super) fn min_view_zoom(&self, preview: &PreviewState) -> f32 {
        (0.5 / self.preview_visual_scale(preview)).min(Viewport::FIT)
    }

    /// The bottom-center zoom scale: a slider (fit → max) plus a preset dropdown
    /// (Fit / 1:1 / % levels). Shown for images (which pan/zoom via [`ZoomPan`]).
    pub(super) fn zoom_control(&self, preview: &PreviewState, tb: Tb) -> Element<'static, Msg> {
        // Slashed from the former 150px (which was sized for the long "Fit to screen"): the
        // preset labels are now short ("Fit", "100%"…"200%"), so most of that was dead space.
        // The floor is set by the widest label ("200%") plus the dropdown chevron.
        const COMBO_W: f32 = 72.0;
        let z = preview.view.zoom;
        let visual = self.preview_visual_scale(preview);
        let max_zoom = self.max_view_zoom(preview);
        // The slider runs in DISPLAY percent (not the raw zoom multiplier) so the 100% mark is a
        // CONSTANT position on the rail, notched by a tick. The callback maps percent back to the
        // fit-relative zoom; the magnetic detent onto exactly 100% is `snap_to_hundred`, applied
        // when the message lands (iced's `.breakpoints` is draw-only despite its doc comment).
        //
        // The tick is drawn by `widgets::notched_slider`, NOT by `.breakpoints` (DRAGON-343):
        // stock iced draws breakpoints over a different span than the thumb, so the notch sits
        // ~3-4px off the value it marks (and shifts again when the thumb grows on hover). That
        // bug — and the one-line upstream fix that lets us delete the wrapper — is documented in
        // `widgets/notched_slider.rs`. Never re-add `.breakpoints` here while it stands.
        let min_pct = displayed_percent(self.min_view_zoom(preview), visual) as f32;
        let max_pct = displayed_percent(max_zoom, visual) as f32;
        let cur_pct = (displayed_percent(z, visual) as f32).clamp(min_pct, max_pct);
        let vscale = visual;
        let slider = crate::widgets::notched_slider(
            widget::slider(min_pct..=max_pct, cur_pct, move |pct| {
                Msg::Preview(PreviewMsg::SetViewZoom(pct / (vscale * 100.0)))
            })
            .step(1.0f32)
            .width(Length::Fixed(120.0)),
            min_pct..=max_pct,
            vec![100.0],
        );
        // (The live percent readout that used to sit LEFT of the slider was removed — the combo
        // to the right already shows the current zoom as a preset label or "N%".)
        // A fixed-width combo: the button shows the CURRENT zoom (a preset label, or the live
        // "N%" for an in-between slider/scroll zoom) so it never blanks; clicking opens the
        // preset menu. Fixed width so it never resizes as the label changes.
        let label = match preview.view.zoom_preset {
            Some(i) => ZOOM_PRESET_LABELS[i].to_string(),
            None => format!("{}%", displayed_percent(z, visual)),
        };
        // The clickable chip matches the timer dropdown's chip: a bordered container at the
        // toolbar-button height with the 1px trim ring. The ring is the ACCENT trim whenever
        // we're zoomed off the default (a % level or an in-between slider/scroll zoom), and
        // subdued at Fit / 100% — same "armed vs resting" cue as the timer chip.
        let armed = !matches!(preview.view.zoom_preset, Some(0) | Some(1));
        let button = widget::mouse_area(
            widget::container(
                widget::row(vec![
                    widget::text(label).size(14).width(Length::Fill).into(),
                    crate::widgets::icons::handle("pan-down-symbolic").icon().size(14).into(),
                ])
                .align_y(Alignment::Center),
            )
            .width(Length::Fixed(COMBO_W))
            .height(Length::Fixed(tb.icon_box() + 2.0 * tb.btn_pad()))
            // Tighter than a button's `btn_pad`: the narrowed combo needs the room for its
            // label + chevron.
            .padding([0.0, tb.btn_pad() * 0.5])
            .align_y(Alignment::Center)
            .class(cosmic::theme::Container::Custom(Box::new(move |theme| {
                let c = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(c.background.component.base.into())),
                    text_color: Some(c.background.component.on.into()),
                    border: Border {
                        // Button token — the combo reads as a bordered button.
                        radius: crate::app::theme::rounding(theme).xl.into(),
                        width: 1.0,
                        color: if armed {
                            crate::app::theme::accent(theme)
                        } else {
                            state_mix(theme, MIX_OFF)
                        },
                    },
                    ..Default::default()
                }
            }))),
        )
        .on_press(Msg::Preview(PreviewMsg::ToggleZoomMenu));
        let combo: Element<'static, Msg> = if preview.view.zoom_menu_open {
            let items: Vec<Element<'static, Msg>> = ZOOM_PRESET_LABELS
                .iter()
                .enumerate()
                .map(|(i, lbl)| {
                    let _ = i;
                    // All preset options render on the theme foreground (white).
                    let text = widget::text(*lbl).size(14).class(cosmic::theme::Text::Custom(
                        |t| cosmic::iced::widget::text::Style {
                            color: Some(t.cosmic().background.component.on.into()),
                            ..Default::default()
                        },
                    ));
                    crate::widgets::arrow_cursor::arrow_cursor(
                        widget::button::custom(text)
                            .width(Length::Fill)
                            .class(cosmic::theme::Button::Text)
                            .on_press(Msg::Preview(PreviewMsg::ZoomPreset(i))),
                    )
                })
                .collect();
            let menu = widget::container(widget::column(items).spacing(2.0))
                .width(Length::Fixed(COMBO_W))
                .padding(4.0)
                .class(cosmic::theme::Container::custom(|theme| {
                    let c = theme.cosmic();
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(c.background.component.base.into())),
                        border: Border {
                            radius: crate::app::theme::rounding(theme).s.into(),
                            width: 1.0,
                            color: c.background.divider.into(),
                        },
                        ..Default::default()
                    }
                }));
            widget::popover(button)
                .popup(menu)
                .position(widget::popover::Position::Point(cosmic::iced::Point::new(0.0, 0.0)))
                .on_close(Msg::Preview(PreviewMsg::ToggleZoomMenu))
                .into()
        } else {
            button.into()
        };
        widget::row(vec![slider, combo])
            .spacing(8.0)
            .align_y(Alignment::Center)
            .into()
    }

    /// The explicit width (px) for the fullscreen-overlay control column — the
    /// media-hugging viewport width (see [`Self::preview_viewport`]), so the
    /// toolbars span the picture rather than the whole monitor.
    pub(super) fn overlay_control_width(&self, preview: &PreviewState) -> f32 {
        self.preview_viewport(preview).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_zoom_clamps_to_the_floor_and_drops_pan() {
        let mut v = Viewport { pan: (10.0, 10.0), ..Viewport::default() };
        // The floor is now HALF the fit (0.5) — a zoom-out request past it clamps to 0.5,
        // and being at/below fit recentres (drops any pan).
        v.set_zoom(0.1);
        assert_eq!(v.zoom, 0.5);
        assert_eq!(v.pan, (0.0, 0.0));
    }

    /// Zooming OUT to exactly 50% is reachable, and anything below it clamps up to 0.5.
    /// Below fit the picture is smaller than the viewport, so the pan recentres.
    #[test]
    fn set_zoom_allows_fifty_percent_and_recentres_below_fit() {
        let mut v = Viewport { pan: (7.0, 9.0), ..Viewport::default() };
        v.set_zoom(0.5);
        assert_eq!(v.zoom, 0.5, "50% (half the fit) is reachable");
        assert_eq!(v.pan, (0.0, 0.0), "below fit recentres");
        // A 0.75 zoom is between the new floor and fit — kept as-is, pan still recentred.
        let mut v2 = Viewport { pan: (7.0, 9.0), ..Viewport::default() };
        v2.set_zoom(0.75);
        assert_eq!(v2.zoom, 0.75);
        assert_eq!(v2.pan, (0.0, 0.0));
        // Below the floor clamps up to 0.5.
        let mut v3 = Viewport::default();
        v3.set_zoom(0.2);
        assert_eq!(v3.zoom, Viewport::MIN);
        assert_eq!(Viewport::MIN, 0.5);
    }

    #[test]
    fn set_zoom_above_fit_clamps_to_the_ceiling_and_keeps_pan() {
        let mut v = Viewport { pan: (3.0, 4.0), ..Viewport::default() };
        v.set_zoom(1000.0);
        assert_eq!(v.zoom, 64.0);
        assert_eq!(v.pan, (3.0, 4.0), "zooming in past fit must not disturb an existing pan");
    }

    /// `visual_scale` composes the physical fit fraction with the capture backing scale.
    /// A 2× capture fitted at `fit_scale = 0.5` has `visual_scale = 1.0` — its natural
    /// on-screen size, so the readout reads 100% at fit.
    #[test]
    fn visual_scale_composes_fit_and_source_scale() {
        assert!((visual_scale(0.5, 2.0) - 1.0).abs() < 1e-6, "2× fit = natural = 100%");
        assert!((visual_scale(0.25, 2.0) - 0.5).abs() < 1e-6, "2× shrunk-to-fit reads 50%");
        // Physical 1:1 on a 2× capture (fit_scale would be 1.0) reads 200% visual.
        assert!((visual_scale(1.0, 2.0) - 2.0).abs() < 1e-6, "2× physical 1:1 = 200%");
        // A non-positive/zero source scale defensively degrades to 1.0.
        assert!((visual_scale(0.5, 0.0) - 0.5).abs() < 1e-6);
    }

    /// Scale 1.0 (every Linux capture, and non-Retina mac panels) is the IDENTITY —
    /// `visual_scale` returns `fit_scale` unchanged, so the readout/preset math is
    /// byte-identical to the pre-visual-units behaviour.
    #[test]
    fn visual_scale_is_identity_at_source_scale_one() {
        for fit in [1.0f32, 0.5, 0.25, 0.8125, 0.0001] {
            assert!((visual_scale(fit, 1.0) - fit).abs() < 1e-6, "fit {fit}");
        }
    }

    /// The user-facing readout is `zoom × visual_scale × 100`. At fit (zoom 1.0) a 2×
    /// capture reads 100%; a 1× capture whose fit fraction is 0.5 reads 50% (honest — the
    /// picture is genuinely shown at half its physical size on a 1× panel).
    #[test]
    fn displayed_percent_is_visual_units() {
        // 2× capture, fit: zoom 1.0, visual_scale 1.0 → 100.
        assert_eq!(displayed_percent(1.0, visual_scale(0.5, 2.0)), 100);
        // 2× capture zoomed to physical 1:1 (zoom = 1/fit = 2.0) → 200.
        assert_eq!(displayed_percent(2.0, visual_scale(0.5, 2.0)), 200);
        // 1× capture, fit fraction 1.0 → 100 (byte-identical to old native readout).
        assert_eq!(displayed_percent(1.0, visual_scale(1.0, 1.0)), 100);
        // 1× capture shrunk to fit at 0.5 → 50 (old behaviour, unchanged at scale 1).
        assert_eq!(displayed_percent(1.0, visual_scale(0.5, 1.0)), 50);
    }

    #[test]
    fn snap_to_hundred_is_magnetic_only_near_100() {
        // A large grab fitted at 78% on-screen (visual_scale 0.78).
        let vs = visual_scale(0.78, 1.0);
        let z100 = preset_zoom(Some(1.0), vs); // the zoom whose displayed% == 100
        // Within a couple percent of 100 snaps to exactly 100.
        assert_eq!(displayed_percent(snap_to_hundred(z100 * 1.02, vs), vs), 100);
        assert_eq!(displayed_percent(snap_to_hundred(z100 * 0.98, vs), vs), 100);
        // Far from 100 (a 50%-ish zoom) is left untouched.
        let far = 0.5 / vs;
        assert!((snap_to_hundred(far, vs) - far).abs() < 1e-6);
    }

    /// The reset / "100%" preset targets natural on-screen size: `preset_zoom(Some(1.0), s)
    /// = 1/s`. On a 2× capture fitted at `visual_scale = 1.0` that's zoom 1.0 = the fit size
    /// (natural). "Fit to screen" (None) is always the fit multiplier.
    #[test]
    fn preset_zoom_targets_visual_fractions() {
        let vs_2x = visual_scale(0.5, 2.0); // 1.0
        assert!((preset_zoom(Some(1.0), vs_2x) - 1.0).abs() < 1e-6, "100% on 2× = natural = fit");
        assert!((preset_zoom(Some(2.0), vs_2x) - 2.0).abs() < 1e-6, "200% on 2× = physical 1:1");
        assert!((preset_zoom(None, vs_2x) - Viewport::FIT).abs() < 1e-6, "Fit = fit multiplier");
        // Round-trip: applying a preset then reading the percent back yields the label.
        for (frac, want) in [(1.0f32, 100), (1.25, 125), (1.5, 150), (2.0, 200)] {
            let z = preset_zoom(Some(frac), vs_2x);
            assert_eq!(displayed_percent(z, vs_2x), want, "preset {frac} round-trips");
        }
    }

    /// At source_scale 1.0 the preset math is byte-identical to the old native-fraction
    /// formula (`zoom = frac / fit`): visual_scale degenerates to fit, so the two agree.
    #[test]
    fn preset_zoom_is_identity_at_source_scale_one() {
        for fit in [1.0f32, 0.5, 0.25] {
            let vs = visual_scale(fit, 1.0);
            for frac in [1.0f32, 1.25, 1.5, 2.0] {
                let visual_form = preset_zoom(Some(frac), vs);
                let old_native_form = frac / fit.max(0.0001);
                assert!(
                    (visual_form - old_native_form).abs() < 1e-4,
                    "fit {fit} frac {frac}: {visual_form} vs {old_native_form}"
                );
            }
        }
    }
}
