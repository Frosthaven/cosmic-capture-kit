//! Still-image preview: decode the screenshot off-thread and show it at native size
//! (downscaled only if it exceeds the monitor, never upscaled) with the shared
//! Save / Save As / Copy / Cancel action bar. Covermark edits recomposite from the
//! retained original pixels, so the display is exactly what a bake writes.

use super::layers::{Layer, LayerKey, LayerStack};
use super::*;
use std::sync::Arc;

/// The image preview's payload: the decoded capture, or `None` while it's still
/// decoding (the shared spinner shows until [`PreviewMsg::ImageReady`] arrives).
pub struct ImagePreview {
    pub image: Option<widget::image::Handle>,
    /// The untouched decoded pixels — the recomposite source for edits. `None` when
    /// the decode fell back to `Handle::from_path` (edits then bake from disk and
    /// the display updates only on export).
    pub original: Option<Arc<::image::RgbaImage>>,
}

impl ImagePreview {
    /// A freshly-opened image preview, still decoding.
    pub fn loading() -> Self {
        Self { image: None, original: None }
    }
}

/// Decode `path` off-thread (so a large capture doesn't stall the UI), resolving to
/// [`PreviewMsg::ImageReady`] — or `Cancel` if the channel drops.
pub(super) fn decode_task(path: PathBuf) -> Task<cosmic::Action<Msg>> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let payload = match ::image::open(&path) {
            Ok(img) => {
                // Wrap in Arc FIRST so the handle can SHARE the decoded pixel allocation
                // (via the zero-copy `shared_rgba_handle`) instead of cloning it — the
                // original stays available as the edit recomposite source either way.
                let original = Arc::new(img.into_rgba8());
                let handle = shared_rgba_handle(&original);
                (handle, Some(original))
            }
            Err(_) => (widget::image::Handle::from_path(&path), None),
        };
        let _ = tx.send(payload);
    });
    Task::perform(rx, |res| {
        cosmic::Action::App(Msg::Preview(match res {
            Ok((handle, original)) => PreviewMsg::ImageReady(handle, original),
            Err(_) => PreviewMsg::Cancel,
        }))
    })
}

impl App {
    /// The loaded-image view: the capture (ScaleDown — like Contain, but never enlarges
    /// a sub-monitor shot) with the edit bar above and the action bar anchored directly
    /// beneath it, all centred together as one group.
    pub(super) fn image_loaded_view<'a>(
        &'a self,
        preview: &'a PreviewState,
        img: &'a ImagePreview,
        tb: Tb,
    ) -> Element<'a, Msg> {
        // `is_loading()` guarantees `image` is Some here; fall back to the spinner just
        // in case, so this is never an empty frame.
        let Some(handle) = &img.image else {
            return self.preview_loading_view(preview, tb);
        };
        // The base image stays a STABLE handle; the covermark is a separate raster stacked
        // over it, drawn through the persistent-texture shader (same as the video path). The
        // base never re-uploads and the covermark's texture updates in place, so neither
        // blinks mid-edit. Both are sized to the same fitted box (fit_dims), so they align;
        // the bake still composites at full source resolution.
        // Fit the media at its NATURAL on-screen size — LOGICAL points (physical /
        // source scale), so a hidpi capture is never drawn larger than 100% even when a
        // floored window's canvas is bigger than the picture (rule 2, DRAGON-221). The
        // image HANDLE stays the hi-res physical pixels, downsampled into this box, so
        // it's sharp on hidpi. `source_scale == 1.0` (Linux 1x) makes points == physical
        // — byte-identical to the old `edit.frame` fit.
        let (ow, oh) = preview.frame_points();
        let image: Element<'a, Msg> = if ow > 0 && oh > 0 {
            let (avail_w, avail_h) = self.preview_viewport(preview);
            let (dw, dh) = video::fit_dims(ow, oh, avail_w, avail_h);
            self.still_media(preview, handle, dw, dh)
        } else {
            // No known dims (rare decode fallback): plain fit, no covermark overlay.
            widget::container(
                widget::image(handle.clone()).content_fit(cosmic::iced::ContentFit::ScaleDown),
            )
            .center_x(Length::Fill)
            .into()
        };
        // The ZoomPan covers the whole canvas box: windowed fills the (media-fitted)
        // window; the overlay uses its media-hugging viewport height, so the toolbars
        // sit right above/below the picture instead of at the monitor's extremes.
        let slot = widget::container(image)
            .width(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        let slot = if preview.surface.is_window() {
            slot.height(Length::Fill)
        } else {
            slot.height(Length::Fixed(self.preview_viewport(preview).1))
        };
        // Ctrl+scroll zooms, alt+scroll/drag pans — base + covermark transform together (one
        // ZoomPan over the stack), so the covermark never moves relative to the picture. The
        // fitted picture's pixel size (zoom 1.0) lets the widget clamp the pan and drive the
        // scrollbars from its REAL bounds — no dependence on an app-side viewport estimate.
        let content_px = {
            // Same natural (logical-point) fit as the drawn box above, so the pan clamp
            // and scrollbars track the REAL displayed picture size (DRAGON-221).
            let (iw, ih) = preview.frame_points();
            if iw > 0 && ih > 0 {
                let (avail_w, avail_h) = self.preview_viewport(preview);
                video::fit_dims(iw, ih, avail_w, avail_h)
            } else {
                (0.0, 0.0)
            }
        };
        let image = crate::widgets::ZoomPan::new(
            slot,
            preview.view.zoom,
            preview.view.pan,
            preview.view.pan_mode,
            content_px,
            |step, ux, uy| Msg::Preview(PreviewMsg::Zoom(step, ux, uy)),
            |dx, dy| Msg::Preview(PreviewMsg::Pan(dx, dy)),
        );
        // Left: do-not-train + covermark tools. Right: the size + Delete group. (Save / Save
        // As / Copy, appearance, and Close live on the top bar.) Center reserved for the zoom
        // scale.
        // `Vec<Element<'static, _>>` is a subtype of `Vec<Element<'a, _>>` (Element
        // is covariant in its lifetime), so this is a plain re-binding.
        let left: Vec<Element<'a, Msg>> = self.edit_tools(preview, tb);
        // Right: the zoom scale (Fit/%/presets), then the pointer/pan tools at the far right.
        // (Size + Delete moved to the top bar.)
        let right: Vec<Element<'a, Msg>> = vec![
            self.zoom_control(preview, tb),
            tb.pan_tool_group(preview.view.pan_mode),
        ];
        let toolbar = toolbar_row(left, Vec::new(), right);
        compose_preview(
            preview.surface.is_window(),
            self.overlay_control_width(preview),
            self.edit_toolbar(preview, tb),
            image.into(),
            None,
            toolbar,
            tb.glass,
        )
    }

    /// The base still (plus the covermark, when applied) for the loaded-image view, fitted
    /// to `dw`×`dh`. Portable path: a STABLE `widget::image` handle with the covermark
    /// stacked over it through the persistent-texture shader (the base never re-uploads and
    /// the covermark's texture updates in place, so neither blinks mid-edit; both sized to
    /// the same fitted box so they align — the bake still composites at full source
    /// resolution). Windows OVERLAY exception (DRAGON-235): iced's raster-image pipeline does
    /// not composite on the premultiplied transparent surface, so the base is drawn through
    /// the SAME LayerStack shader instead — with the covermark folded into that one stack, so
    /// only a single LayerStack ever lives on the surface (two would fight over slot pruning).
    /// The opaque windowed surface, Linux (layer-shell) and macOS keep `widget::image`; those
    /// platforms compile only the portable path below (byte-identical).
    fn still_media(
        &self,
        preview: &PreviewState,
        handle: &widget::image::Handle,
        dw: f32,
        dh: f32,
    ) -> Element<'static, Msg> {
        #[cfg(windows)]
        if !preview.surface.is_window()
            && let Some(base) = super::layers::rgba_handle_frame(handle)
        {
            let mut layers = vec![Layer { key: LayerKey::VIDEO, frame: base }];
            if let Some(cm) = preview.edit.cm_raster.frame() {
                layers.push(Layer { key: LayerKey::COVERMARK, frame: cm.clone() });
            }
            let shader = cosmic::iced::widget::shader::Shader::new(LayerStack::new(layers))
                .width(Length::Fixed(dw))
                .height(Length::Fixed(dh));
            return widget::container(Element::new(shader)).center_x(Length::Fill).into();
        }
        let base = widget::container(
            widget::image(handle.clone())
                .content_fit(cosmic::iced::ContentFit::Fill)
                .width(Length::Fixed(dw))
                .height(Length::Fixed(dh)),
        )
        .center_x(Length::Fill);
        if let Some(frame) = preview.edit.cm_raster.frame() {
            let layers = LayerStack::new(vec![Layer { key: LayerKey::COVERMARK, frame: frame.clone() }]);
            let shader = cosmic::iced::widget::shader::Shader::new(layers)
                .width(Length::Fixed(dw))
                .height(Length::Fixed(dh));
            let overlay = widget::container(Element::new(shader)).center_x(Length::Fill);
            cosmic::iced::widget::stack(vec![base.into(), overlay.into()]).into()
        } else {
            base.into()
        }
    }
}
