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

/// The preview editor's chrome scale — ONE value for BOTH surface kinds (see
/// [`PreviewSurface::btn_scale`]).
///
/// It used to be per-surface: the fullscreen overlay drew its toolbars at full size (1.0)
/// while the windowed preview tightened them for its smaller window. The owner preferred the
/// tighter chrome everywhere, so the overlay was unified ONTO the windowed figure rather than
/// the two being kept in step — there is deliberately no second constant to drift from this one.
///
/// **It briefly also decided how sharply the toolbar GLYPHS render** — it does not any more, and
/// the history matters because it is easy to reach for this constant again for the wrong reason.
///
/// It was raised 0.82 → 0.91 (DRAGON-392) to buy the accept checkmark a solid spine: the glyph
/// box was `ICON_BOX × this`, and a FRACTIONAL box made iced rasterize at one size and draw at
/// another, resampling the texture and softening every edge. DRAGON-399 found and fixed that at
/// the source — [`crate::widgets::icons::box_px`] snaps every icon box to a whole pixel — so the
/// bigger chrome was paying for a bug, and the scale returned to 0.82. **Do not raise it for
/// glyph sharpness again**; if a glyph looks soft, check its box is integral first.
///
/// With the snap in place the numbers are, on a 1× display (`box = round(22 × scale)`, and the
/// raster now equals the draw, so nothing is resampled at ANY scale):
///
/// | scale | raw box | snapped | stroke | solid px in the accept check |
/// |-------|---------|---------|--------|------------------------------|
/// | 0.82  | 18.04   | 18      | 1.50px | 4                            |
/// | 0.91  | 20.02   | 20      | 1.67px | 0                            |
/// | 1.09  | 23.98   | 24      | 2.00px | 16                           |
///
/// Note the middle row: sharpness is NOT monotonic in size — it depends on how the glyph's
/// coordinates land on the pixel grid — so 0.82 measures BETTER than the 0.91 it was raised to,
/// once the resample is gone. Tuning this for glyph crispness is guesswork; tune it for how big
/// the chrome should be, which is what it is for.
///
/// Everything scale-derived (`btn_pad`, `grp_pad`, the flyout padding, [`OVERLAY_HEADER_H`],
/// `slider_item_w`, the bar-height reservations, the group footprints) follows automatically —
/// nothing is tuned to a particular value of it. What does NOT follow are the deliberately
/// scale-independent, text-bearing widths (the Fit combo's `COMBO_W`, the slider rail, the text
/// size/font chips, the label point sizes): those hold still while the buttons around them
/// change size.
pub(super) const CHROME_SCALE: f32 = 0.82;

/// The OVERLAY header row's height (DRAGON-337): its buttons are FLAT header-style icons —
/// a glyph box plus its halo, with NO `tool_group` capsule — so the row is exactly one
/// button box tall, `2 × GROUP_PAD` shorter than a capsuled toolbar bar. Scaled by
/// [`CHROME_SCALE`], like every button the row actually draws.
pub(super) const OVERLAY_HEADER_H: f32 = CHROME_SCALE * (ICON_BOX + 2.0 * BTN_PAD);

impl PreviewSurface {
    /// Whether this is the resizable WINDOW appearance, as opposed to the fullscreen
    /// layer-shell overlay.
    pub fn is_window(self) -> bool {
        matches!(self, Self::Window)
    }

    /// The toolbar-button size scale for this surface: [`CHROME_SCALE`], the same in both.
    /// The overlay used to render its chrome larger (1.0); it now matches the windowed
    /// editor exactly, so the two surfaces differ in LAYOUT only, never in control size.
    pub fn btn_scale(self) -> f32 {
        CHROME_SCALE
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
            // THREE rows in a 12px-spaced column, plus the centred group's 40px top & bottom
            // insets: both toolbars (at the shared [`CHROME_SCALE`], like the window's), and
            // above them the DRAGON-337 header row (windowed-swap / undo / redo ⟨split⟩
            // Close). That row's buttons are FLAT — no `tool_group` capsule — so it is one
            // button box tall ([`OVERLAY_HEADER_H`]), NOT a full `GROUP_H_BASE`. Portable:
            // the overlay composition is the same view on every platform (Linux layer-shell,
            // the mac/Windows fullscreen windows), so there is no per-OS arm to add here.
            Self::Overlay => {
                2.0 * (self.btn_scale() * GROUP_H_BASE + 12.0) + (OVERLAY_HEADER_H + 12.0) + 80.0
            }
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

/// CAPTURE units per POINT for the display a windowed preview is SIZED AGAINST
/// (DRAGON-449) — the same per-output factor the capture overlay's units bridge carries
/// ([`crate::geometry::OverlayUnits`]), read through the one platform seam
/// ([`crate::platform::overlay_point_scale`]) so "how far apart are the two spaces on this
/// OS" keeps a single answer. Windows: that monitor's `dpi / 96`. macOS / Linux: `1.0`.
///
/// `None` = no NAMED display anchors this preview (`--preview`, a handed-over document, a
/// capture whose outputs are already torn down). There is then no capture space to convert
/// FROM, so the caller has already resolved points and the answer is the identity — exactly
/// the pre-DRAGON-449 behaviour, never a guess.
///
/// The `cfg` is about the HANDLE, not about the units: off Linux an [`OutputHandle`] IS the
/// display name the seam takes, while on Linux it is a `WlOutput` carrying no name — and
/// Linux's answer is `1.0` regardless (a layer surface's app space IS point space), so the
/// arm gives nothing up.
pub(super) fn monitor_point_scale(output: Option<&OutputHandle>) -> f32 {
    #[cfg(not(target_os = "linux"))]
    {
        output.map(|name| crate::platform::overlay_point_scale(name)).unwrap_or(1.0)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = output;
        1.0
    }
}

/// A monitor's extent in LOGICAL POINTS — the ONLY units the windowed preview's open fit
/// may measure against (DRAGON-449).
///
/// Three consumers, all point-space by definition, all fed from here:
/// [`windowed_fit_size`]'s monitor bound (the full-width / [`sizing::USABLE_H_FRAC`]-height
/// budgets), and [`super::shell::preview_window`]'s transient max-size hint plus its
/// `min_size` clamp — a `window::Settings` size is logical points on every backend.
///
/// `capture` is that monitor in CAPTURE space (PHYSICAL pixels on Windows, points on macOS
/// and Linux — the [`crate::platform::backend::OutputDesc`] units contract), and
/// `point_scale` comes from [`monitor_point_scale`]. Feeding the capture-space rect straight
/// in made every budget and the floor clamp `dpi/96`× too permissive on Windows: at 300% the
/// window could open TALLER than the display it landed on.
///
/// At `point_scale <= 1.0` this returns `capture` UNCHANGED (see [`sizing::to_points`]), so
/// Linux, macOS and every 96-DPI Windows box are byte-identical.
pub(super) fn monitor_fit_points(capture: (u32, u32), point_scale: f32) -> (u32, u32) {
    // The SAME rule-6 primitive `preview_source_scale` already uses for the MEDIA
    // dimensions — one physical→points conversion, not two that can drift.
    sizing::to_points(capture, point_scale)
}

/// The largest known output, in LOGICAL POINTS — the monitor bound a preview with NO capture
/// anchor opens against (`--preview`, and a handed-over document on a host whose own capture
/// is long finished). Each output converts through its OWN [`OutputState::point_scale`]
/// (DRAGON-448/449), so a mixed-DPI desktop compares like with like instead of letting a
/// high-DPI panel win on physical pixels alone. `None` when no output is known — the caller
/// keeps its placeholder.
pub(super) fn largest_output_points(outputs: &[OutputState]) -> Option<(u32, u32)> {
    outputs
        .iter()
        .map(|o| monitor_fit_points(o.logical_size, o.point_scale))
        .max_by_key(|(w, h)| *w as u64 * *h as u64)
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
/// a little padding between the split's two sides — the widest of the three bars. Must track
/// the toolbar compositions (see [`App::overlay_header_row`], [`App::edit_toolbar`],
/// [`App::edit_tools`], and the action rows in `image.rs` / `video.rs`).
pub(super) fn overlay_min_content_width(preview: &PreviewState) -> f32 {
    overlay_min_content_width_for(
        matches!(preview.kind, PreviewKind::Video(_)),
        preview.edit.covermark.is_some(),
    )
}

/// [`overlay_min_content_width`]'s pure arithmetic, keyed only on what actually changes the
/// composition: the media kind and whether a covermark's sliders are showing. Split out so
/// the bar math is unit-testable without a whole `PreviewState`.
fn overlay_min_content_width_for(video: bool, covermark: bool) -> f32 {
    // Every button/group footprint below is at the shared chrome scale — the overlay's bars
    // are built from the same `Tb` measurements the windowed editor's are.
    let s = PreviewSurface::Overlay.btn_scale();
    let button = s * (ICON_BOX + 2.0 * BTN_PAD);
    // tool_group: `grp_pad` padding + `n` buttons spaced 2px apart.
    let group = |n: f32| 2.0 * s * GROUP_PAD + n * button + (n - 1.0) * 2.0;
    // A bar's width: its group widths + 8px row spacing between items (groups + the split)
    // + the little split gap.
    let bar = |groups: f32, items: f32| groups + 8.0 * (items - 1.0) + SPLIT_MIN_GAP;

    // Header row (DRAGON-337 + DRAGON-353's Settings button): swap | settings | undo | redo
    // ⟨split⟩ close — five FLAT buttons (no group capsules), so each costs a bare button box
    // and they are all row items.
    let header = bar(5.0 * button, 6.0);

    // Top bar (DRAGON-392): ⟨split⟩ save/save-as/copy/delete(4). The filesize chip that used to
    // sit here for VIDEOS moved to their bottom bar, and the images' info block leads the LEFT
    // side with the annotation tray — like the tray itself it isn't counted, because that whole
    // leading row wraps gracefully. `--preview` drops Delete, so four is the wider case and the
    // one to reserve for.
    let top = bar(group(4.0), 2.0);

    // Bottom bar (DRAGON-392 re-derivation). VIDEO: [covermark (+ its zoom/opacity sliders when
    // applied)] ⟨split⟩ [the document-info block]. IMAGE: [swatch + 7 line-width segments],
    // [covermark (+ sliders)] ⟨split⟩ [the document-info block], [the zoom slider + preset
    // dropdown]. The crop button and the pointer/pan pair are no longer here at all — crop moved
    // into the top tray's Select/Hand/Crop group and the pan pair became the Hand tool inside it.
    let sliders = if covermark { 2.0 * slider_item_w(s) } else { 0.0 };
    // The info block: group padding + the button-box side padding its container adds + the
    // widest line it can hold. That line is a resolution ("3840 × 2160" — 11 glyphs at the
    // 13px menu-label size, ~7px each in Inter), never the filesize, which is far shorter.
    let info_chip = 2.0 * s * GROUP_PAD + 2.0 * s * BTN_PAD + 88.0;
    let (bottom_groups, bottom_items) = if video {
        (group(1.0) + sliders + info_chip, 3.0)
    } else {
        // The zoom cluster: group padding + the 8px inner-left slider pad + the ZOOM rail
        // (`ZOOM_SLIDER_W` — its own width since the owner widened it a third; the icon-led
        // sliders above still ride `slider_item_w`'s shared one) + 8px row spacing + the 72px
        // combo (COMBO_W in viewport.rs).
        let zoom_ctrl = 2.0 * s * GROUP_PAD + 8.0 + super::chrome::ZOOM_SLIDER_W + 8.0 + 72.0;
        (group(8.0) + group(1.0) + sliders + info_chip + zoom_ctrl, 5.0)
    };
    let bottom = bar(bottom_groups, bottom_items);

    header.max(top).max(bottom)
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

    /// DRAGON-337: the overlay reserves its two toolbars PLUS the header row — each with one of
    /// the column's 12px gaps, inside the 40px top/bottom insets. The header row's buttons are
    /// FLAT (no capsule), so it costs one button box, `2 × GROUP_PAD` less than a toolbar bar
    /// would. Everything here is at the shared [`CHROME_SCALE`] (the overlay no longer draws
    /// its chrome at 1.0).
    #[test]
    fn overlay_chrome_reserves_the_flat_header_row_and_its_gap() {
        let o = PreviewSurface::Overlay;
        let s = o.btn_scale();
        // The header row + its gap is the ONLY delta vs the historical two-bar reserve...
        let two_bars = 2.0 * (s * GROUP_H_BASE + 12.0) + 80.0;
        assert!(((o.chrome_h() - two_bars) - (OVERLAY_HEADER_H + 12.0)).abs() < 0.001);
        // ...and a flat row is exactly the (scaled) group padding shorter than a capsuled bar.
        assert!(((s * GROUP_H_BASE - OVERLAY_HEADER_H) - s * 2.0 * GROUP_PAD).abs() < 0.001);
    }

    /// BOTH surfaces now draw the preview editor's chrome at the SAME scale (the overlay was
    /// unified onto the windowed figure) — there is one constant, and no second one to drift.
    #[test]
    fn both_surfaces_share_one_chrome_scale() {
        assert_eq!(PreviewSurface::Overlay.btn_scale(), CHROME_SCALE);
        assert_eq!(PreviewSurface::Window.btn_scale(), CHROME_SCALE);
    }

    /// The overlay control floor covers EVERY row, including the new header line — so the
    /// swap / undo / redo ⟨split⟩ Close row can never be squeezed narrower than it needs.
    #[test]
    fn overlay_min_width_covers_the_header_row() {
        let button = PreviewSurface::Overlay.btn_scale() * (ICON_BOX + 2.0 * BTN_PAD);
        // Five flat buttons since DRAGON-353 added Settings beside the appearance toggle.
        let header = 5.0 * button + 8.0 * 5.0 + SPLIT_MIN_GAP;
        for video in [false, true] {
            for covermark in [false, true] {
                assert!(
                    overlay_min_content_width_for(video, covermark) >= header,
                    "the floor must fit the header row (video={video} covermark={covermark})"
                );
            }
        }
    }

    /// DRAGON-357 item 11: the user-measured windowed floor (`PREVIEW_MIN_W`, 924 since
    /// DRAGON-392) DOMINATES
    /// the content-derived toolbar floor in every composition (worst case — image + covermark
    /// sliders — re-derived at ~890px after the item 9 line-width regrouping to seven segments).
    /// If a future cluster grows the content floor past the constant, this fails: at that point
    /// switch the windowed floor to `max(PREVIEW_MIN_W, content)` instead of silently clipping.
    #[test]
    fn windowed_floor_dominates_the_content_derived_bar_widths() {
        for video in [false, true] {
            for covermark in [false, true] {
                let content = overlay_min_content_width_for(video, covermark);
                assert!(
                    content <= super::super::shell::PREVIEW_MIN_W,
                    "content floor {content} exceeds PREVIEW_MIN_W (video={video} covermark={covermark}) — bump the floor to max(constant, content)"
                );
            }
        }
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

/// DRAGON-449: the windowed open fit's MONITOR bound (and the max-size hint / `min_size`
/// clamp built from it) in LOGICAL POINTS, not the capture-space rect Windows reports.
#[cfg(test)]
mod monitor_points_tests {
    use super::*;

    /// THE byte-identity pin. At factor 1.0 — Linux, macOS, and every 96-DPI Windows box —
    /// the conversion returns the capture rect UNCHANGED, so `windowed_fit_size`, the
    /// max-size hint and the `min_size` clamp all see the EXACT expression they saw before
    /// the fix (`Some(capture_monitor)` / `capture_monitor.0 as f32`). If this moves, the
    /// change has touched platforms it had no business touching.
    #[test]
    fn factor_one_is_byte_identical_to_the_old_capture_monitor() {
        for capture in [(1920u32, 1080u32), (3840, 2160), (5120, 1440), (800, 480), (1, 1)] {
            let pts = monitor_fit_points(capture, 1.0);
            assert_eq!(pts, capture, "the bound must be the same tuple at 1.0");
            for media in [(1280u32, 720u32), (3840, 2160), (400, 300)] {
                for extra_h in [0.0f32, PreviewSurface::Window.transport_h()] {
                    assert_eq!(
                        windowed_fit_size(media, Some(pts), extra_h),
                        windowed_fit_size(media, Some(capture), extra_h),
                    );
                }
            }
            assert_eq!(
                (pts.0 as f32, pts.1 as f32),
                (capture.0 as f32, capture.1 as f32),
            );
            assert_eq!(
                super::shell::preview_min_size((pts.0 as f32, pts.1 as f32)),
                super::shell::preview_min_size((capture.0 as f32, capture.1 as f32)),
            );
        }
        // An UNKNOWN display (no name to resolve, a `--preview` file, a torn-down capture)
        // is the identity too — never a guess.
        assert_eq!(monitor_point_scale(None), 1.0);
    }

    /// The customer's display (DRAGON-447): 3840x2160 at 300% is 1280x720 POINTS, and every
    /// Windows scale step divides the same way.
    #[test]
    fn a_scaled_windows_monitor_reads_back_as_its_point_extent() {
        for (px, factor, want) in [
            ((3840u32, 2160u32), 3.0f32, (1280u32, 720u32)),
            ((3840, 2160), 2.0, (1920, 1080)),
            ((3840, 2160), 1.5, (2560, 1440)),
            ((2560, 1440), 1.25, (2048, 1152)),
            ((5120, 1440), 1.0, (5120, 1440)),
        ] {
            assert_eq!(monitor_fit_points(px, factor), want, "{px:?} at {factor}x");
        }
    }

    /// The ticket's symptom, end to end: a size-unknown open (a spinner still decoding) on
    /// the customer's 3840x2160 @ 300% display asked for 1600x1000 — on a screen that is
    /// only 1280x720 POINTS, i.e. TALLER and WIDER than the display it lands on. In points
    /// it fits, and the height lands on the floor rather than past the screen.
    #[test]
    fn the_size_unknown_fallback_no_longer_exceeds_the_display() {
        let capture = (3840u32, 2160u32);
        let fallback = |m: (u32, u32)| {
            (
                (m.0 as f32 * 0.8).clamp(super::shell::PREVIEW_MIN_W, 1600.0),
                (m.1 as f32 * 0.9).clamp(super::shell::PREVIEW_MIN_H, 1000.0),
            )
        };
        let pts = monitor_fit_points(capture, 3.0);
        assert_eq!(pts, (1280, 720));
        let (bw, bh) = fallback(capture);
        assert!(bw > pts.0 as f32 && bh > pts.1 as f32, "the bug: {bw}x{bh} on a {pts:?} screen");
        let (fw, fh) = fallback(pts);
        assert!(fw <= pts.0 as f32, "fixed width {fw} must fit {}", pts.0);
        // The 924x732 control floor is itself taller than a 720pt screen — the window can only
        // shrink to what the display holds, which is what the `min_size` clamp is for.
        assert_eq!((fw, fh), (1024.0, super::shell::PREVIEW_MIN_H));
        let (mw, mh) = super::shell::preview_min_size((pts.0 as f32, pts.1 as f32));
        assert_eq!((mw, mh), (super::shell::PREVIEW_MIN_W, 720.0), "the floor clamps to the screen");
        // ...and against the physical rect it never clamped at all (the bug).
        assert_eq!(
            super::shell::preview_min_size((capture.0 as f32, capture.1 as f32)),
            (super::shell::PREVIEW_MIN_W, super::shell::PREVIEW_MIN_H),
        );
    }

    /// A media-sized open on a scaled display: the fit is bounded by the POINT extent, so the
    /// window fits the screen instead of overflowing it by the scale factor.
    #[test]
    fn a_full_screen_capture_fits_the_display_it_opens_on() {
        // The whole 3840x2160 @ 200% display captured: 3840x2160 physical media, which is
        // 1920x1080 points on a 1920x1080-point screen.
        let capture = (3840u32, 2160u32);
        let pts = monitor_fit_points(capture, 2.0);
        let media = sizing::to_points(capture, 2.0);
        let (w, h) = windowed_fit_size(media, Some(pts), 0.0);
        assert!(w <= pts.0 as f32 + 0.5, "width {w} spills off a {}pt screen", pts.0);
        assert!(
            h <= pts.1 as f32 * sizing::USABLE_H_FRAC + 0.5,
            "height {h} exceeds the {}% budget of a {}pt screen",
            sizing::USABLE_H_FRAC * 100.0,
            pts.1,
        );
        // The un-divided bound let the SAME capture ask for a window taller than the screen.
        let (_, buggy_h) = windowed_fit_size(media, Some(capture), 0.0);
        assert!(buggy_h > pts.1 as f32, "the bug: {buggy_h}pt tall on a {}pt screen", pts.1);
    }

    /// Mixed DPI: the largest output is chosen by its POINT area, so a small high-DPI panel
    /// can't out-vote a big 100% display on physical pixels alone. (The `--preview` /
    /// handed-over-document bound.)
    #[test]
    fn the_largest_output_is_measured_in_points() {
        // A 2560x1440 panel at 200% is only 1280x720 points — smaller than a plain 1920x1080,
        // though its physical pixel count is larger.
        let hi = monitor_fit_points((2560, 1440), 2.0);
        let lo = monitor_fit_points((1920, 1080), 1.0);
        assert_eq!(hi, (1280, 720));
        assert!(lo.0 as u64 * lo.1 as u64 > hi.0 as u64 * hi.1 as u64);
    }
}
