//! The preview viewport: zoom/pan state (`Viewport`), the fit/viewport/pan
//! math every consumer shares, and the zoom-scale control.
//! Split from `preview/mod.rs` (DRAGON-115) — pure code motion.

use super::*;

/// The pan/zoom of the preview image. `zoom` 1.0 = fit (default), `pan` in screen px.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub zoom: f32,
    pub pan: (f32, f32),
    // DRAGON-392: the pan MODE flag is gone. Panning is the HAND TOOL
    // (`annotation_canvas::Tool::Hand`, `EditState::pan_active`), so "a plain drag pans" is
    // derived from the armed tool rather than tracked beside it. Alt+drag still pans under
    // every tool.
    /// The zoom-scale dropdown selection: `Some(i)` = a preset is exactly applied, `None` =
    /// an in-between zoom (slider drag / scroll). Drives the combo's current label.
    pub zoom_preset: Option<usize>,
    /// The zoom preset menu (combo popover) is open.
    pub zoom_menu_open: bool,
    /// The crop tool is active (DRAGON-382): the zoom floor relaxes from [`Viewport::MIN`] to
    /// [`Viewport::CROP_MIN`] so the user can pull the media much smaller for a roomy crop
    /// workspace. Set on crop-session enter, cleared on exit.
    pub crop_mode: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: (0.0, 0.0),
            zoom_preset: Some(0),
            zoom_menu_open: false,
            crop_mode: false,
        }
    }
}

impl Viewport {
    /// The zoom range, as a multiple of the FIT size. The floor is HALF the fit (0.5) — the
    /// user can zoom OUT to half the whole-picture-fit size (DRAGON: preview-editor polish).
    /// Below fit the picture is smaller than the viewport, so it just recentres (no pan). The
    /// ceiling is a hard cap; the effective max is the VISUAL limit (see `App::max_view_zoom`),
    /// which is what actually bounds zoom-IN.
    ///
    /// `MAX` is a backstop and must stay clear of that limit, which is `MAX_VISUAL /
    /// visual_scale` — so the smaller the fit, the higher it reaches. At 500% (DRAGON-400) a
    /// capture fitted at 8% of natural size wants `5.0 / 0.078 ≈ 64`, i.e. the OLD `64.0` cap
    /// would have started binding on a wide multi-monitor grab in a small window and silently
    /// capped the zoom below the advertised 500%. Raised with headroom to spare.
    pub(super) const MIN: f32 = 0.5;
    pub(super) const MAX: f32 = 320.0;
    /// The relaxed zoom floor while the crop tool is active (DRAGON-382) — a fit-relative
    /// multiplier well below [`Self::MIN`], so the media can be pulled small for a roomy crop
    /// workspace (much farther out than the normal 50% floor).
    pub(super) const CROP_MIN: f32 = 0.1;
    /// The "fit" multiplier (whole picture visible) — the recentre point.
    pub(super) const FIT: f32 = 1.0;

    /// The zoom floor for the current mode: the relaxed crop floor while the crop tool is
    /// active, else the normal half-fit floor.
    pub(super) fn zoom_floor(&self) -> f32 {
        if self.crop_mode { Self::CROP_MIN } else { Self::MIN }
    }

    /// Set the zoom multiplier directly (slider / preset), clamped. At or below fit the
    /// picture fully fits (no overflow), so recentre — drop any pan.
    pub(super) fn set_zoom(&mut self, z: f32) {
        self.zoom = z.clamp(self.zoom_floor(), Self::MAX);
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
/// The 200%-and-below stops are UNCHANGED by the 500% raise (DRAGON-400): 125% and 150% are the
/// fine-tuning steps people actually use, and dropping them to spread five stops evenly across
/// the wider range would have taken away precision exactly where the work happens. The new range
/// gets ROUND HUNDREDS instead — at 300%+ you are inspecting pixels, not choosing a working
/// scale, so coarse stops cost nothing and the list stays scannable.
pub(super) const ZOOM_PRESET_LABELS: [&str; 8] =
    ["Fit", "100%", "125%", "150%", "200%", "300%", "400%", "500%"];

/// The presets as VISUAL fractions (`1.0` = natural on-screen size). Converted to the
/// viewport's fit-relative multiplier via `visual_scale` (see [`App::preview_visual_scale`]).
pub(super) const ZOOM_PRESET_VISUAL: [Option<f32>; 8] =
    [None, Some(1.0), Some(1.25), Some(1.5), Some(2.0), Some(3.0), Some(4.0), Some(MAX_VISUAL)];

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

/// **The zoom ceiling in VISUAL units** — 500% of the picture's natural on-screen size
/// (DRAGON-400, raised from 200%). Read by [`App::max_view_zoom`] and by the top preset, so the
/// rail's end and the list's last row can never disagree about where the range stops.
pub(super) const MAX_VISUAL: f32 = 5.0;

/// Whether preset `i` can actually be APPLIED at the current ceiling (DRAGON-401).
///
/// A preset above `max_zoom` would clamp to the ceiling and land the user somewhere other than
/// the row they clicked, while the menu went on advertising it — so those rows are dropped
/// from the list instead. On any capture the GPU bound does not bite (the common case, and
/// every capture at all before DRAGON-400 raised the ceiling) every preset survives and the
/// menu is unchanged.
///
/// "Fit" (`None`) is always reachable — it is the fit multiplier itself — and `max_zoom` is
/// floored at fit, so the list can never empty out. A hair of tolerance keeps a preset that
/// sits exactly ON the ceiling (float rounding either way) in the list rather than blinking
/// out of it.
pub(super) fn reachable_preset(i: usize, visual_scale: f32, max_zoom: f32) -> bool {
    match ZOOM_PRESET_VISUAL.get(i).copied() {
        Some(frac) => preset_zoom(frac, visual_scale) <= max_zoom * 1.001,
        // Out of range: not a real preset, so nothing to offer.
        None => false,
    }
}

/// The 100% detent's capture zone for the SCROLL wheel, in displayed percent. Scroll steps are
/// multiplicative and fine (12% per notch), so a value-space tolerance is the natural one and
/// this is the historic figure, unchanged.
pub(super) const SNAP_SCROLL_PCT: f32 = 2.5;

/// The 100% detent's capture zone for the SLIDER, in RAIL PIXELS either side of the tick.
///
/// The slider's detent is specified in PIXELS, not percent, because the rail maps the whole
/// zoom range linearly onto ~64pt: how much percent a pixel is worth depends entirely on the
/// range, so a fixed percent tolerance is a moving target. At the old 200% ceiling a pixel was
/// worth ~2.3% and the historic ±2.5% detent was about a pixel wide; at 500% a pixel is worth
/// ~7% and that same ±2.5% would be **sub-pixel — literally unhittable by dragging**, silently
/// retiring a detent the owner asked to keep. Specifying the zone in rail space keeps it the
/// same SIZE UNDER THE POINTER at any range, which is the property that actually matters.
///
/// 1.5px gives a 3px-wide capture zone: a little more forgiving than the historic ~2px, which is
/// deliberate — the old one was tight enough to miss.
pub(super) const SNAP_RAIL_PX: f32 = 1.5;

/// The detent tolerance (displayed percent) for a rail `rail_px` long spanning
/// `min_pct..=max_pct` — [`SNAP_RAIL_PX`] converted into the value space the snap works in.
/// Floored at [`SNAP_SCROLL_PCT`] so a degenerate rail (zero length, or a range so narrow the
/// pixels are worth almost nothing) can never produce a detent tighter than the historic one.
pub(super) fn rail_snap_pct(min_pct: f32, max_pct: f32, rail_px: f32) -> f32 {
    if rail_px <= 0.0 {
        return SNAP_SCROLL_PCT;
    }
    ((max_pct - min_pct).max(0.0) / rail_px * SNAP_RAIL_PX).max(SNAP_SCROLL_PCT)
}

/// Snap a slider/scroll zoom to EXACTLY 100% on-screen (natural size) when it lands within
/// `tol_pct` displayed percent of it — a magnetic detent so the user can hit 100% precisely.
/// Returns the input unchanged when it isn't near 100%.
///
/// The tolerance is the CALLER's because the two callers snap in different spaces: the scroll
/// wheel in value space ([`SNAP_SCROLL_PCT`]), the slider in rail space ([`rail_snap_pct`]).
/// One rule, two geometries — rather than one tolerance that is right for neither.
pub(super) fn snap_to_hundred(zoom: f32, visual_scale: f32, tol_pct: f32) -> f32 {
    if (zoom * visual_scale * 100.0 - 100.0).abs() <= tol_pct {
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
    /// (ScaleDown) into the content area — capped at `1.0`, so fit never reads over 100%. The
    /// internal building block for pan/zoom geometry; the USER-FACING percent and presets go
    /// through [`Self::preview_visual_scale`] instead (visual units). `1.0` when the media
    /// dims aren't known yet (still loading).
    pub(super) fn preview_fit_scale(&self, preview: &PreviewState) -> f32 {
        // Expressed through the unclamped form so the two can never drift apart. The `1.0`
        // ceiling is the readout's rule ("fit never reads over 100%"), not a geometric fact —
        // see [`Self::preview_points_per_source_px`] for why the device bound must NOT take it.
        self.preview_points_per_source_px(preview).min(1.0)
    }

    /// Screen POINTS per SOURCE pixel at zoom 1 — the scale `preview/image.rs` lays the media
    /// stack out at, and the geometric building block [`Self::preview_fit_scale`] adds its
    /// readout ceiling to.
    ///
    /// Un-cropped this is `dw / frame_w` (the fit); with a crop applied it is `dw / crop_w` —
    /// the WHOLE frame rendered at the crop's scale, since the media stack always renders the
    /// whole frame and the crop only frames a sub-region of it (DRAGON-385/391).
    ///
    /// **Why the device bound (DRAGON-401) reads this rather than `preview_fit_scale`**: the
    /// clamped form can only ever REPORT LESS, and under-reporting the media stack's size is
    /// precisely what hands the GPU an oversized viewport. Today the two agree everywhere —
    /// `fit_dims` caps its scale at `1.0`, so the crop case cannot blow a small region up past
    /// natural size and the ceiling never binds. Should display ever start upscaling (a
    /// fill-the-viewport crop, say), the clamped form would silently under-report and this
    /// crash would come straight back; taking the unclamped one here means it cannot.
    ///
    /// `1.0` when the media dims aren't known yet (still loading).
    pub(super) fn preview_points_per_source_px(&self, preview: &PreviewState) -> f32 {
        // The DISPLAY frame (DRAGON-385): a crop makes the fit / zoom / presets relative to the
        // cropped framing (100% = the crop at natural size). Un-cropped = the decoded frame,
        // byte-identical to before.
        let (iw, ih) = preview.display_frame();
        if iw == 0 || ih == 0 {
            return 1.0;
        }
        // Fit the media's NATURAL (logical-point) size, then express it as a fraction of
        // the PHYSICAL pixels (the divisor). A floored hidpi window thus reads ≤ 100%
        // visual at fit — the picture is shown at its natural size, not physical 1:1
        // (rule 2, DRAGON-221). `source_scale == 1.0` (Linux 1x) makes points ==
        // physical, so `dw / iw` is byte-identical to before.
        let (pw, ph) = preview.display_frame_points();
        let (avail_w, avail_h) = self.preview_viewport(preview);
        let (dw, _) = video::fit_dims(pw.max(1), ph.max(1), avail_w, avail_h);
        (dw / iw as f32).max(0.0001)
    }

    /// The zoom ceiling this DEVICE imposes (DRAGON-401): past it, the media stack's viewport
    /// exceeds `max_texture_dimension_2d` and wgpu kills the process mid-frame. See
    /// [`crate::widgets::gpu::max_zoom_for_device`] for the arithmetic, and that module for
    /// why the limit and the render scale are observed rather than assumed — and for why the
    /// budget is the limit less three pixels rather than the limit itself (the effects shader
    /// snaps its viewport out to the pixel grid, so a ceiling landing exactly ON the limit
    /// overflowed it by one).
    ///
    /// Floored at fit: showing the whole picture must always be possible, and it always CAN
    /// be. `fit_dims` never upscales, so at zoom 1 the media is at most its own source size
    /// and the whole capture would have to exceed the device limit for the floor to be
    /// reached — in which case nothing could be shown at all and no zoom ceiling would help.
    pub(super) fn gpu_max_zoom(&self, preview: &PreviewState) -> f32 {
        crate::widgets::gpu::max_zoom_for_device(
            preview.edit.frame,
            self.preview_points_per_source_px(preview),
            crate::widgets::gpu::render_scale(),
            crate::widgets::gpu::max_texture_dim(),
        )
        .max(Viewport::FIT)
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
        // `content_px`, the authoritative clamp (DRAGON-221). The DISPLAY frame (DRAGON-385):
        // a crop clamps the pan to the cropped framing. Byte-identical at scale 1, un-cropped.
        let (iw, ih) = preview.display_frame_points();
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

    /// The maximum view zoom (fit-relative) — the [`MAX_VISUAL`] cap, 500% since DRAGON-400:
    /// displayed visual fraction = zoom × visual_scale, so the zoom for 500% visual is
    /// `5.0 / visual_scale`. Never below fit.
    ///
    /// The old 200% ceiling was chosen so a 2× capture could always reach physical 1:1 — at
    /// `source_scale = 2`, `visual_scale ≈ 1.0` at fit, so 200% visual IS 1:1. **That property
    /// still holds**: 500% simply extends past it, and actual-pixels remains reachable (now as a
    /// point inside the range rather than at its end). What the raise adds is inspection room
    /// above 1:1 — reading small text, checking an annotation's edge — which is what the owner
    /// asked for.
    ///
    /// **DRAGON-401**: the [`MAX_VISUAL`] ceiling is then cut down to whatever this DEVICE can
    /// actually draw ([`Self::gpu_max_zoom`]). On a large capture the GPU bound is the binding
    /// one, so 500% is simply not reachable there — and because the slider's rail, its readout
    /// and the preset list are all built from THIS function, the UI stops advertising a zoom it
    /// cannot deliver rather than pretending and stopping short.
    pub(super) fn max_view_zoom(&self, preview: &PreviewState) -> f32 {
        (MAX_VISUAL / self.preview_visual_scale(preview))
            .min(self.gpu_max_zoom(preview))
            .max(Viewport::FIT)
    }

    /// The zoom FLOOR: the multiplier whose DISPLAYED percent is 50% (`0.5 / visual_scale`),
    /// capped at fit so a picture whose fit already reads below 50% can still be shown whole.
    /// Zooming OUT bottoms out at 50% on-screen regardless of how far the fit downscaled the
    /// capture — a large grab fits at e.g. 78%, and half of THAT would read 39%, not 50%.
    pub(super) fn min_view_zoom(&self, preview: &PreviewState) -> f32 {
        // Crop tool (DRAGON-382): relax the slider floor to the fit-relative crop minimum so the
        // user can zoom the media much smaller for a roomy crop workspace.
        if preview.view.crop_mode {
            return Viewport::CROP_MIN;
        }
        (0.5 / self.preview_visual_scale(preview)).min(Viewport::FIT)
    }

    /// The bottom-center zoom scale: a slider (fit → max) plus a preset dropdown
    /// (Fit / 1:1 / % levels). Shown for images (which pan/zoom via [`ZoomPan`]).
    pub(super) fn zoom_control(&self, preview: &PreviewState, tb: Tb) -> Element<'static, Msg> {
        // Slashed from the former 150px (which was sized for the long "Fit to screen"): the
        // preset labels are now short ("Fit", "100%"…"500%"), so most of that was dead space.
        // The floor is set by the widest label plus the dropdown chevron. DRAGON-400 checked the
        // worst case the readout can produce — a three-digit NON-preset percent from a slider or
        // scroll zoom, e.g. "437%": four glyphs at size 12 (~27pt) + the 10pt chevron + 3pt
        // spacing + 12pt padding ≈ 52pt, comfortably inside this. It never needs to grow for the
        // wider range, because "500%" is the same four glyphs "200%" was.
        const COMBO_W: f32 = 72.0;
        // The zoom-preset menu's on-screen height (px) for the UPWARD flyout offset (item 2
        // CAUTION: bottom-bar dropdowns open up). One `menu_container` row per preset — each a
        // size-13 text button (~27px with cosmic's 5px button padding) — plus the 2px inter-row
        // gaps and the container's 4px inset top+bottom. Derived from the label COUNT, so the
        // DRAGON-400 stops grew it automatically: 8 rows ≈ 238pt (was ≈151pt at five). It still
        // lands on screen — the smallest content area this control can appear in is the 732pt
        // minimum window less its chrome, ≈597pt, so the popup clears the bar with room to spare.
        // A small over-estimate only lifts the menu a hair clear of the chip; it never overlaps.
        // DRAGON-401 made it a fn of the ROW COUNT rather than of the label count: a device
        // ceiling can drop the top presets from the list, and a height still sized for all
        // eight would float the shortened menu a couple of rows above the chip.
        fn zoom_menu_panel_h(rows: usize) -> f32 {
            let n = rows.max(1) as f32;
            n * 27.0 + (n - 1.0) * 2.0 + 2.0 * 4.0
        }
        // Addressed to this document (DRAGON-336 phase 2).
        let pid = preview.window;
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
        // The preview editor's own (smaller) thumb — see `PREVIEW_SLIDER_THUMB`. The wrapper
        // gets the SAME class so its 100% notch stays aligned with the resized thumb.
        // The rail is `ZOOM_SLIDER_W` — the shared toolbar-slider width widened by a third
        // (owner's request). It came FROM that shared width in DRAGON-392 and diverged from it
        // here; see the constant for why the zoom rail alone earns the extra pixels. The tick is
        // drawn by `notched_slider` from the slider's own bounds, so it stays on the 100 position
        // at any width, and the magnetic detent is measured in RAIL PIXELS (`rail_snap_pct`), so
        // a longer rail makes it FINER in percent while keeping it the same size under the
        // pointer — the resize can only improve it.
        let slider = crate::widgets::notched_slider(
            widget::slider(min_pct..=max_pct, cur_pct, move |pct| {
                Msg::Preview(pid, PreviewMsg::SetViewZoom(pct / (vscale * 100.0)))
            })
            .step(1.0f32)
            .class(super::chrome::preview_slider_class())
            .width(Length::Fixed(super::chrome::ZOOM_SLIDER_W)),
            min_pct..=max_pct,
            vec![100.0],
            super::chrome::preview_slider_class(),
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
        // DRAGON-357 item 2: the Fit/scale chip is the EXACT same closed control the text SIZE /
        // FONT dropdowns wear ([`chrome::dropdown_chip`]) — a `Button::Text` chip (size-12 label,
        // size-10 pan-down chevron, [2,6] padding, fixed `COMBO_W`) so the trim colour, font
        // size, chevron glyph AND hover trim all match across the editor (the hover styling lives
        // in the shared `Button::Text` class, so the Fit control can't miss it). It still INHERITS
        // the enclosing bordered cluster's fill.
        let button =
            // Always enabled: this is the BOTTOM bar, which a crop session does not gate.
            super::chrome::dropdown_chip(
                pid,
                widget::text(label).size(12).into(),
                COMBO_W,
                PreviewMsg::ToggleZoomMenu,
                true,
            );
        let combo: Element<'static, Msg> = if preview.view.zoom_menu_open {
            let cur = preview.view.zoom_preset;
            let items: Vec<Element<'static, Msg>> = ZOOM_PRESET_LABELS
                .iter()
                .enumerate()
                .filter(|(i, _)| reachable_preset(*i, visual, max_zoom))
                .map(|(i, lbl)| {
                    // Match the text menus: the CURRENT preset row reads accent, the rest default.
                    let hot = cur == Some(i);
                    let text = if hot {
                        widget::text(*lbl).size(13).class(cosmic::theme::Text::Custom(|t| {
                            cosmic::iced::widget::text::Style {
                                color: Some(crate::app::theme::accent(t)),
                                ..Default::default()
                            }
                        }))
                    } else {
                        widget::text(*lbl).size(13)
                    };
                    crate::widgets::arrow_cursor::arrow_cursor(
                        widget::button::custom(text)
                            .width(Length::Fill)
                            .class(cosmic::theme::Button::Text)
                            .on_press(Msg::Preview(pid, PreviewMsg::ZoomPreset(i))),
                    )
                })
                .collect();
            let panel_h = zoom_menu_panel_h(items.len());
            // DRAGON-357 item 17: the Fit/scale menu wears the SAME opaque dropdown surface as the
            // text SIZE / FONT menus. Item 2 CAUTION: it opens UPWARD (bottom bar), via the shared
            // `flyout` helper's `Up` direction — deterministic in both the overlay and the window,
            // unlike the raw popover's room-below auto-flip.
            let menu = tb.menu_container(widget::column(items).spacing(2.0), COMBO_W);
            super::chrome::flyout(
                button,
                menu,
                super::chrome::FlyoutDir::Up(panel_h),
                Msg::Preview(pid, PreviewMsg::ToggleZoomMenu),
            )
        } else {
            button
        };
        // DRAGON-357: the slider + dropdown share ONE bordered cluster, like the other bottom-bar
        // groups (the dropdown chip above no longer draws its own chrome). Item 9: the slider
        // gets inner-LEFT padding so its rail doesn't hug the group's left edge.
        let slider =
            widget::container(slider).padding([0.0, 0.0, 0.0, super::chrome::CLUSTER_INNER_PAD]);
        // The slider + dropdown are both SHORTER than a toolbar button box, so left to their own
        // natural height this cluster collapsed below the tool group beside it (the
        // dropdown-chip rebuild dropped the tall combo that used to anchor the height). Pin the
        // row to the button-box height (`icon_box + 2*btn_pad`, the SAME height `slider_with_icon`
        // and every `tool_toggle` resolve to) and centre its contents, so the zoom cluster matches
        // its sibling group's height with its items vertically centred.
        let row = widget::container(
            widget::row(vec![slider.into(), combo])
                .spacing(8.0)
                .align_y(Alignment::Center),
        )
        .height(Length::Fixed(tb.icon_box() + 2.0 * tb.btn_pad()))
        .align_y(Alignment::Center);
        tb.tool_cluster(vec![row.into()])
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
        v.set_zoom(10_000.0);
        // The BACKSTOP, not the user-facing ceiling — that is `App::max_view_zoom`, which
        // always binds first. Raised with the 500% range (DRAGON-400) so it stays clear of it.
        assert_eq!(v.zoom, Viewport::MAX);
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
        let tol = SNAP_SCROLL_PCT;
        // Within a couple percent of 100 snaps to exactly 100.
        assert_eq!(displayed_percent(snap_to_hundred(z100 * 1.02, vs, tol), vs), 100);
        assert_eq!(displayed_percent(snap_to_hundred(z100 * 0.98, vs, tol), vs), 100);
        // Far from 100 (a 50%-ish zoom) is left untouched.
        let far = 0.5 / vs;
        assert!((snap_to_hundred(far, vs, tol) - far).abs() < 1e-6);
    }

    /// **The acceptance test for DRAGON-400: dragging must still land exactly on 100%.**
    ///
    /// Simulated the way the control actually works — the rail maps `min..=max` displayed
    /// percent linearly onto the 64pt rail, the slider quantises to whole percent, and the
    /// message runs through the snap. Every pixel within the detent's reach must produce
    /// EXACTLY 100, at the old 200% ceiling AND the new 500% one.
    ///
    /// This is what the old fixed ±2.5% tolerance would have failed: at 500% a pixel is worth
    /// ~7%, so no drag position other than a bullseye would have landed inside it.
    #[test]
    fn the_hundred_percent_detent_stays_hittable_by_dragging_at_any_ceiling() {
        // The REAL rail, not a copy: a widened rail must re-run this, not silently pass.
        let rail = chrome::ZOOM_SLIDER_W;
        for (label, max_visual) in [("old 200% ceiling", 2.0_f32), ("new 500% ceiling", MAX_VISUAL)] {
            // A large grab fitted at 78% on-screen — the realistic case, where fit != 100%.
            let vs = visual_scale(0.78, 1.0);
            let min_pct = displayed_percent((0.5 / vs).min(Viewport::FIT), vs) as f32;
            let max_pct = displayed_percent(max_visual / vs, vs) as f32;
            let tol = rail_snap_pct(min_pct, max_pct, rail);
            // Where 100% sits along the rail, in pixels.
            let px_of = |pct: f32| (pct - min_pct) / (max_pct - min_pct) * rail;
            let hundred_px = px_of(100.0);
            // Drag to the nearest whole pixel either side of the tick — the best a pointer can
            // realistically do — and demand an exact 100 out of it.
            let mut hits = 0;
            for dx in [-1.0_f32, 0.0, 1.0] {
                let px = (hundred_px + dx).clamp(0.0, rail);
                // The slider reports the value under the pointer, quantised by `.step(1.0)`.
                let pct = (min_pct + px / rail * (max_pct - min_pct)).round();
                let zoom = pct / (vs * 100.0);
                let snapped = snap_to_hundred(zoom, vs, tol);
                assert_eq!(
                    displayed_percent(snapped, vs),
                    100,
                    "{label}: dragging to {dx:+} px from the 100% tick landed off 100"
                );
                hits += 1;
            }
            assert_eq!(hits, 3);
            // …and the detent is still a DETENT, not a dead zone swallowing the neighbourhood:
            // a drag a quarter of the way along the rail is well outside it and must come
            // through UNCHANGED.
            let far_pct = (min_pct + 0.25 * (max_pct - min_pct)).round();
            assert!(
                (far_pct - 100.0).abs() > tol,
                "{label}: the fixture is inside the detent, so it proves nothing"
            );
            let far = far_pct / (vs * 100.0);
            assert!(
                (snap_to_hundred(far, vs, tol) - far).abs() < f32::EPSILON,
                "{label}: the detent reached a quarter of the way down the rail"
            );
        }
    }

    /// The rail detent is specified in PIXELS, so it must stay ~the same width under the pointer
    /// as the range grows — that is the whole reason it is not a percent.
    #[test]
    fn the_rail_detent_keeps_its_pixel_width_as_the_range_grows() {
        let rail = 64.0_f32;
        for (min_pct, max_pct) in [(50.0_f32, 200.0_f32), (50.0, 500.0), (78.0, 641.0)] {
            let tol = rail_snap_pct(min_pct, max_pct, rail);
            let px_per_pct = rail / (max_pct - min_pct);
            let half_width_px = tol * px_per_pct;
            assert!(
                (half_width_px - SNAP_RAIL_PX).abs() < 0.01,
                "range {min_pct}..{max_pct}: detent is {half_width_px}px either side, not \
                 {SNAP_RAIL_PX}"
            );
        }
        // A degenerate rail can never produce a detent TIGHTER than the historic value.
        assert_eq!(rail_snap_pct(100.0, 100.0, 64.0), SNAP_SCROLL_PCT);
        assert_eq!(rail_snap_pct(50.0, 500.0, 0.0), SNAP_SCROLL_PCT);
    }

    /// The ceiling reaches the advertised 500% of natural size, and the hard `MAX` backstop
    /// stays clear of it even for a very small fit (a wide multi-monitor grab in a small window),
    /// where the old 64.0 would have started clamping below the advertised range.
    #[test]
    fn the_ceiling_reaches_five_hundred_percent_without_the_backstop_binding() {
        for fit in [1.0_f32, 0.5, 0.25, 0.117, 0.078] {
            let vs = visual_scale(fit, 1.0);
            let ceiling = (MAX_VISUAL / vs).max(Viewport::FIT);
            assert_eq!(displayed_percent(ceiling, vs), 500, "fit {fit}: ceiling is not 500%");
            assert!(
                ceiling <= Viewport::MAX,
                "fit {fit}: the ceiling {ceiling} exceeds the MAX backstop {}",
                Viewport::MAX
            );
        }
    }

    /// The preset table and its labels stay in step, the list ends at the ceiling, and the
    /// pre-500% stops are untouched (existing muscle memory keeps working).
    #[test]
    fn the_presets_span_the_range_and_keep_their_old_stops() {
        assert_eq!(ZOOM_PRESET_LABELS.len(), ZOOM_PRESET_VISUAL.len());
        assert_eq!(&ZOOM_PRESET_LABELS[..5], &["Fit", "100%", "125%", "150%", "200%"]);
        assert_eq!(ZOOM_PRESET_VISUAL[0], None, "the first stop is Fit");
        assert_eq!(
            *ZOOM_PRESET_VISUAL.last().unwrap(),
            Some(MAX_VISUAL),
            "the last preset must be the rail's own ceiling"
        );
        // Strictly increasing after Fit, and every label matches its fraction.
        let mut prev = 0.0;
        for (i, frac) in ZOOM_PRESET_VISUAL.iter().enumerate().skip(1) {
            let f = frac.expect("only Fit is None");
            assert!(f > prev, "preset {i} is not above the one before it");
            prev = f;
            assert_eq!(ZOOM_PRESET_LABELS[i], format!("{}%", (f * 100.0).round() as i32));
        }
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

    /// DRAGON-401: when the device ceiling does NOT bite, every preset is still offered — the
    /// menu on an ordinary capture is byte-identical to before.
    #[test]
    fn every_preset_is_offered_when_the_ceiling_does_not_bite() {
        let vs = visual_scale(0.787, 1.0);
        let max = MAX_VISUAL / vs; // the 500% ceiling itself — nothing below it
        for (i, lbl) in ZOOM_PRESET_LABELS.iter().enumerate() {
            assert!(reachable_preset(i, vs, max), "preset {i} ({lbl}) dropped");
        }
    }

    /// The owner's capture (2640x1448 fitted at 0.787) against this GPU's 8192 limit: the real
    /// ceiling is ~394% fit-relative = ~310% displayed, so 400% and 500% cannot be reached and
    /// must not be listed — the menu stops advertising a row that would land somewhere else.
    #[test]
    fn presets_above_the_device_ceiling_are_dropped() {
        let vs = visual_scale(2078.0 / 2640.0, 1.0);
        let max = crate::widgets::gpu::max_zoom_for_device((2640, 1448), 2078.0 / 2640.0, 1.0, 8192);
        let offered: Vec<&str> = ZOOM_PRESET_LABELS
            .iter()
            .enumerate()
            .filter(|(i, _)| reachable_preset(*i, vs, max))
            .map(|(_, l)| *l)
            .collect();
        assert_eq!(offered, ["Fit", "100%", "125%", "150%", "200%", "300%"]);
    }

    /// However hard the ceiling bites, "Fit" survives — the list can never empty out, because
    /// `max_view_zoom` floors at fit and "Fit" IS that multiplier.
    #[test]
    fn fit_is_always_offered() {
        for vs in [0.05f32, 0.5, 1.0, 4.0] {
            assert!(reachable_preset(0, vs, Viewport::FIT), "visual_scale {vs}");
        }
    }

    /// A preset sitting exactly ON the ceiling stays listed (float rounding must not make a row
    /// blink in and out as the window is resized by a pixel).
    #[test]
    fn a_preset_exactly_at_the_ceiling_stays_listed() {
        let vs = visual_scale(0.5, 1.0);
        let at_200 = preset_zoom(Some(2.0), vs);
        assert!(reachable_preset(4, vs, at_200), "200% is preset index 4");
        // A hair below it and the row goes.
        assert!(!reachable_preset(4, vs, at_200 * 0.99));
    }
}
