//! The preview's two surface kinds (fullscreen overlay vs CSD window) and
//! every piece of sizing math derived from them: chrome/transport reserves,
//! the windowed open-fit, and the overlay's media-hugging fit box.
//! Split from `preview/mod.rs` (DRAGON-115) — pure code motion.

use super::*;

/// Which kind of surface the OPEN preview lives in — recorded at open time,
/// deliberately decoupled from the `preview_windowed` SETTING (which may
/// flip while a surface of the old kind is still up, e.g. mid-toggle).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PreviewSurface { Overlay, Window }

impl PreviewSurface {
    /// Whether this is the resizable WINDOW appearance, as opposed to the fullscreen
    /// layer-shell overlay.
    pub fn is_window(self) -> bool {
        matches!(self, Self::Window)
    }

    /// The toolbar-button size scale for this surface: the windowed preview's smaller
    /// window wants tighter chrome; the fullscreen overlay uses full size.
    pub fn btn_scale(self) -> f32 {
        match self {
            Self::Window => 0.82,
            Self::Overlay => 1.0,
        }
    }

    /// The CSD header bar's height to reserve above the content — only the WINDOW
    /// draws one; the overlay has none.
    pub fn header_px(self) -> f32 {
        match self {
            Self::Window => 44.0,
            Self::Overlay => 0.0,
        }
    }

    /// The vertical chrome (px) around the preview canvas for this surface kind —
    /// the SINGLE source both the open-size fit ([`windowed_fit_size`]) and the live
    /// content sizing ([`App::preview_content_height`]) derive from, so a windowed
    /// preview opens exactly media-sized and its canvas fills the space between the
    /// bars with no dead bands.
    pub(super) fn chrome_h(self) -> f32 {
        match self {
            // Both toolbars at full scale in a 12px-spaced column, plus the centred
            // group's 40px top & bottom insets.
            Self::Overlay => 2.0 * (GROUP_H_BASE + 12.0) + 80.0,
            // The CSD header + two edge-pinned bars: each a toolbar group at the
            // windowed button scale inside `preview_bar`'s 8px vertical padding.
            // No column spacing, no insets — the canvas fills everything between.
            Self::Window => {
                let bar = self.btn_scale() * GROUP_H_BASE + 2.0 * 8.0;
                self.header_px() + 2.0 * bar
            }
        }
    }

    /// The transport bar's height for this surface — the strip a VIDEO preview
    /// reserves between the canvas and the action toolbar: the tool row (play,
    /// seek time, pointer/razor, delete — a button's height at this surface's
    /// scale) stacked over the timeline editor (ruler + lane stack) with the
    /// column's 6px gap, inside the bar's 8px vertical padding.
    pub(super) fn transport_h(self) -> f32 {
        self.btn_scale() * (ICON_BOX + 2.0 * BTN_PAD)
            + 6.0
            + timeline::RULER_H
            + timeline::LANES_H
            + 2.0 * 8.0
    }

    /// Tear the surface down — the ONE place that knows Window ⇒ `window::close`,
    /// Overlay ⇒ the layer-shell `shell::close_surface`.
    pub fn close(self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        match self {
            Self::Window => window::close(id),
            Self::Overlay => crate::app::shell::close_surface(id),
        }
    }
}

/// The window size that shows `media` (native px) with the canvas MATCHING the picture's
/// aspect ratio — so the whole picture fills the canvas with no letterbox and no self-
/// zoom-out — at the largest scale that still fits `monitor` (minus chrome), never
/// upscaling past native. Below the floor size the aspect may break (controls win).
///
/// `extra_h` is additional vertical chrome beyond the bars/header — the video
/// transport strip's height at open time (0 for stills); passed in rather than
/// derived so this stays in lockstep with whatever `preview_transport_h` reserves.
///
/// `monitor` is the target output's FULL logical size; panels/docks (the compositor's
/// non-exclusive zone) are unknowable client-side, so a request may still overshoot
/// that axis — the compositor clamps it at map time and the resize event re-fits the
/// content. The `max_size` hint set at open (see [`super::shell::preview_window`])
/// keeps cosmic-comp from reshaping the request to 2/3-per-axis on the way.
pub(super) fn windowed_fit_size(media: (u32, u32), monitor: Option<(u32, u32)>, extra_h: f32) -> (f32, f32) {
    // Horizontal chrome is just the 1px CSD border each side; vertical is the
    // header + toolbars + the media kind's transport strip.
    let chrome = (2.0, PreviewSurface::Window.chrome_h() + extra_h);
    // ALL the rule 1-5 math lives in the portable, unit-tested `sizing` module —
    // this only supplies THIS surface's chrome, floor, and the shared 80% height
    // budget (rule 3). `media` is already in LOGICAL points (callers divide the
    // capture's physical pixels by the source backing scale first, rule 6).
    sizing::spawn_window_size(
        media,
        monitor,
        chrome,
        (super::shell::PREVIEW_MIN_W, super::shell::PREVIEW_MIN_H),
        sizing::USABLE_H_FRAC,
    )
}

/// The vertical space `kind`'s TRANSPORT strip takes on `surface`: the play/seek
/// strip for videos (between the canvas and the action toolbar), zero for stills.
/// EVERY sizing path funnels through here — the live viewport
/// ([`preview_transport_h`]), the windowed OPEN fit ([`App::preview_surface_for`]),
/// and the poster re-fit — and the strip's wrappers size to their content, so
/// nothing else hard-codes the height.
///
/// THE RUNTIME SEAM: the micro editor will grow this strip (audio/video timelines,
/// segment tools), and its height will change live per editor state. Read that
/// state off the [`VideoPreview`] here — every consumer follows, including the
/// open fit for a preview that starts with the editor already expanded.
pub(super) fn transport_h_for(kind: &PreviewKind, surface: PreviewSurface) -> f32 {
    match kind {
        PreviewKind::Video(_vid) => surface.transport_h(),
        PreviewKind::Image(_) => 0.0,
    }
}

/// [`transport_h_for`] plus the layout gap the OPEN preview's composition adds
/// around a present strip: the overlay's column spaces its children by 12px, so
/// slotting the strip in costs one more gap (`chrome_h` counts only the two
/// toolbar gaps); the window's column has no spacing. This is what the live
/// viewport / pan / fit math reserves.
pub(super) fn preview_transport_h(preview: &PreviewState) -> f32 {
    let strip = transport_h_for(&preview.kind, preview.surface);
    match preview.surface {
        PreviewSurface::Overlay if strip > 0.0 => strip + 12.0,
        _ => strip,
    }
}

/// The overlay's content box: the media FITTED into the available area (never
/// upscaled), so the centred toolbar/canvas/toolbar group hugs the picture instead
/// of pinning to the monitor's extremes. Width floors at `min_w` (the toolbar
/// groups' needs and the shared windowed floor) so the controls never undersize;
/// media-less states (spinner still decoding, failed video probe) get the full
/// available box.
pub(super) fn overlay_fit_box(media: (u32, u32), avail: (f32, f32), min_w: f32) -> (f32, f32) {
    if media.0 == 0 || media.1 == 0 {
        return (avail.0.max(min_w), avail.1.max(1.0));
    }
    let (dw, dh) = video::fit_dims(media.0, media.1, avail.0, avail.1);
    (dw.max(min_w), dh.max(1.0))
}

/// The minimum width (px) the overlay control area needs to show every toolbar group with
/// a little padding between the split's two sides — the wider of the two bars. Must track
/// the toolbar compositions (see [`App::edit_toolbar`], [`App::edit_tools`], and the action
/// rows in `image.rs` / `video.rs`).
pub(super) fn overlay_min_content_width(preview: &PreviewState) -> f32 {
    let button = ICON_BOX + 2.0 * BTN_PAD;
    // tool_group: `grp_pad` padding + `n` buttons spaced 2px apart.
    let group = |n: f32| 2.0 * GROUP_PAD + n * button + (n - 1.0) * 2.0;
    // A bar's width: its group widths + 8px row spacing between items (groups + the split)
    // + the little split gap.
    let bar = |groups: f32, items: f32| groups + 8.0 * (items - 1.0) + SPLIT_MIN_GAP;

    // Top bar: appearance(1) | undo/redo(2) ⟨split⟩ size+Delete(2) | save/save-as/copy(3) |
    // close(1).
    let info = group(2.0); // size label (~a button box) + Delete button
    let top = bar(group(1.0) + group(2.0) + info + group(3.0) + group(1.0), 6.0);

    // Bottom bar: do-not-train(1) | covermark(1, +2 sliders when applied) ⟨split⟩ [images:
    // pointer/pan(2) + zoom control]. The zoom/opacity sliders live inside the covermark
    // group, so they widen the BOTTOM bar.
    let sliders = if preview.edit.covermark.is_some() {
        2.0 * SLIDER_ITEM_W
    } else {
        0.0
    };
    let (bottom_groups, bottom_items) = if matches!(preview.kind, PreviewKind::Video(_)) {
        (group(1.0) + group(1.0) + sliders, 3.0)
    } else {
        // Images: pointer/pan tools (2) + zoom control (slider + dropdown).
        let zoom_ctrl = 120.0 + 150.0 + 8.0;
        (group(1.0) + group(1.0) + sliders + group(2.0) + zoom_ctrl, 5.0)
    };
    let bottom = bar(bottom_groups, bottom_items);

    top.max(bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The windowed chrome is the header plus two scaled, padded bars — strictly less
    /// than the overlay's reserve (whose 40px insets and full-scale bars don't exist in
    /// a window). This is the invariant behind the no-dead-bands open fit.
    #[test]
    fn windowed_chrome_is_the_header_plus_two_scaled_bars() {
        let w = PreviewSurface::Window;
        let bar = w.btn_scale() * GROUP_H_BASE + 16.0;
        assert_eq!(w.chrome_h(), w.header_px() + 2.0 * bar);
        assert!(w.chrome_h() < PreviewSurface::Overlay.chrome_h() + w.header_px());
    }

    /// The canvas (window minus chrome) must keep the media's aspect exactly — the
    /// per-axis reshape this guards against is the whole point of DRAGON-108. Checked
    /// across the acceptance outputs incl. the 5120x1440 super-ultrawide.
    #[test]
    fn windowed_fit_keeps_the_media_aspect_on_every_output() {
        let chrome_h = PreviewSurface::Window.chrome_h();
        for output in [(3840, 2160), (5120, 1440), (2560, 1440), (1920, 1080), (3440, 1440)] {
            for media in [(3840u32, 2160u32), (5120, 1440), (1920, 1080), (1280, 720)] {
                let (w, h) = windowed_fit_size(media, Some(output), 0.0);
                let (cw, ch) = (w - 2.0, h - chrome_h);
                // Skip combinations where the PREVIEW_MIN floor bites (aspect yields there).
                if w > super::shell::PREVIEW_MIN_W && h > super::shell::PREVIEW_MIN_H {
                    let want = media.0 as f32 / media.1 as f32;
                    assert!(
                        (cw / ch - want).abs() < 0.001,
                        "aspect drifted: output {output:?} media {media:?} canvas {cw}x{ch}"
                    );
                    assert!(w <= output.0 as f32 && h <= output.1 as f32);
                }
            }
        }
    }

    /// A video's window opens taller than a still's by exactly the transport
    /// strip's reserve, so the recording isn't squeezed by the play/seek bar.
    #[test]
    fn video_open_reserves_the_transport_strip() {
        let transport = PreviewSurface::Window.transport_h();
        let still = windowed_fit_size((1280, 720), Some((3840, 2160)), 0.0);
        let video = windowed_fit_size((1280, 720), Some((3840, 2160)), transport);
        assert_eq!(video.0, still.0);
        assert!((video.1 - (still.1 + transport)).abs() < 0.001);
    }

    /// Never upscale past native: a small picture gets a native-sized canvas (window
    /// floors permitting), not a blown-up one.
    #[test]
    fn windowed_fit_never_upscales_past_native() {
        let chrome_h = PreviewSurface::Window.chrome_h();
        let (w, h) = windowed_fit_size((1280, 720), Some((3840, 2160)), 0.0);
        assert_eq!((w - 2.0).round(), 1280.0);
        assert_eq!((h - chrome_h).round(), 720.0);
    }

    /// The floor always wins (toolbars must not clip), even for tiny media.
    #[test]
    fn windowed_fit_respects_the_floor() {
        let (w, h) = windowed_fit_size((320, 200), Some((1920, 1080)), 0.0);
        assert_eq!(w, super::shell::PREVIEW_MIN_W);
        assert_eq!(h, super::shell::PREVIEW_MIN_H);
    }

    /// The overlay's content box hugs the media (fit, never upscaled), floors its
    /// width for the toolbars, and falls back to the full box before dims are known.
    #[test]
    fn overlay_fit_box_hugs_floors_and_falls_back() {
        // Media-less: the full available box (spinner / failed probe).
        assert_eq!(overlay_fit_box((0, 0), (3000.0, 2000.0), 800.0), (3000.0, 2000.0));
        // Fits within avail keeping aspect; wider-than-avail media scales down.
        let (w, h) = overlay_fit_box((3840, 2160), (3000.0, 2000.0), 800.0);
        assert!((w / h - 16.0 / 9.0).abs() < 0.001);
        assert!(w <= 3000.0 && h <= 2000.0);
        // Small media is NOT upscaled — the box hugs it exactly...
        assert_eq!(overlay_fit_box((1280, 720), (3000.0, 2000.0), 800.0), (1280.0, 720.0));
        // ...except width never drops below the toolbar floor.
        let (w, h) = overlay_fit_box((400, 300), (3000.0, 2000.0), 800.0);
        assert_eq!((w, h), (800.0, 300.0));
    }

    /// An unknown output (no monitor yet) opens at native size — the compositor's own
    /// clamp plus the resize re-fit handle any overshoot.
    #[test]
    fn windowed_fit_without_a_monitor_is_native_sized() {
        let chrome_h = PreviewSurface::Window.chrome_h();
        let (w, h) = windowed_fit_size((1600, 900), None, 0.0);
        assert_eq!((w - 2.0).round(), 1600.0);
        assert_eq!((h - chrome_h).round(), 900.0);
    }
}

#[cfg(test)]
mod dpi_proof_tests {
    use super::*;

    /// PROOF of the DRAGON-130 DPI fix, region case (the monitor clamp does NOT
    /// mask it): a 1400×900 LOGICAL region on a 2× display captures to 2800×1800
    /// physical. Buggy code fed physical dims → window 2× too large; the fix feeds
    /// logical dims (physical/scale) → window is region-sized + chrome.
    #[test]
    fn region_on_a_retina_display_opens_logical_sized_not_2x() {
        // A wide logical monitor so neither result is clamped by the monitor bound,
        // and a region above the PREVIEW_MIN floor so neither is floored.
        let monitor = Some((6000u32, 3400u32));
        let chrome_h = PreviewSurface::Window.chrome_h();
        // BUG: physical pixels treated as logical.
        let buggy = windowed_fit_size((2800, 1800), monitor, 0.0);
        // FIX: physical / source_scale(2.0) = the 1400×900 logical footprint.
        let fixed = windowed_fit_size((1400, 900), monitor, 0.0);
        // The fixed window's canvas IS the true 1400×900 logical footprint...
        assert!((fixed.0 - (1400.0 + 2.0)).abs() < 0.5, "fixed w {}", fixed.0);
        assert!((fixed.1 - (900.0 + chrome_h)).abs() < 0.5, "fixed h {}", fixed.1);
        // ...and the buggy window's canvas was 2× that (the reported user symptom).
        assert!((buggy.0 - 2.0 - 2800.0).abs() < 0.5, "buggy w {}", buggy.0);
        assert!(((buggy.0 - 2.0) - 2.0 * (fixed.0 - 2.0)).abs() < 1.0, "buggy must be ~2× fixed");
    }
}
