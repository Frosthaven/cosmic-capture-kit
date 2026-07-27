//! Still-image preview: decode the screenshot off-thread and show it at native size
//! (downscaled only if it exceeds the monitor, never upscaled) with the shared
//! Save / Save As / Copy / Cancel action bar. Covermark edits recomposite from the
//! retained original pixels, so the display is exactly what a bake writes.

use super::layers::{Layer, LayerKey, LayerStack};
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Warn ONCE per process when a content-aware pixelate cell hits the
/// [`annotate::PIXELATE_BLOCK_MAX`] ceiling — the block is safely clamped there (bounding the
/// GPU shader's O(block²) mosaic loop), so this is a perf heads-up, not an error. Once-guarded
/// so a live drag over coarse content can't spam the log.
fn warn_pixelate_cap() {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        log::warn!(
            "pixelate: content-aware cell clamped to the {}px ceiling (bounds the GPU mosaic \
             loop); very large redaction regions stay capped",
            annotate::PIXELATE_BLOCK_MAX
        );
    }
}

/// The annotation right-click context menu: z-order + delete, as a small floating panel. A
/// Spotlight item (DRAGON-329) is a pure knockout region with no color, so it drops the color row —
/// but it KEEPS z-order (needed to send it behind and select layers under it) + Duplicate + Delete.
fn annot_context_menu(spotlight: bool) -> Element<'static, Msg> {
    let item = |label: &'static str, msg: PreviewMsg| -> Element<'static, Msg> {
        crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::custom(widget::text(label).size(13))
                .width(Length::Fill)
                .class(cosmic::theme::Button::Text)
                .on_press(Msg::Preview(msg)),
        )
    };
    // A thin full-width horizontal rule between action groups — the horizontal sibling of the
    // palette's vertical `annot_palette_sep`. Drawn in the menu's own border color
    // (`background.divider`) so the rule matches the container's edge, with a little vertical
    // breathing room so it reads as a gap between groups rather than a squeezed line.
    let divider = || -> Element<'static, Msg> {
        let line = widget::container(
            widget::Space::new().width(Length::Fill).height(Length::Fixed(1.0)),
        )
        .class(cosmic::theme::Container::custom(|theme| {
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(theme.cosmic().background.divider.into())),
                ..Default::default()
            }
        }));
        widget::container(line).padding([2.0, 0.0]).into()
    };
    let rows = if spotlight {
        // A spotlight carries no color, so no color row. Its z-order doesn't change the RENDER (a
        // knockout is a union), but it DOES decide hit-test order — a spotlight on top swallows
        // clicks meant for layers beneath it, so z-order controls are needed to send it back and
        // select what's under it. Groups: Duplicate · z-order · Delete, each split by a rule.
        vec![
            item("Duplicate", PreviewMsg::DuplicateSelected),
            divider(),
            item("Bring to Front", PreviewMsg::SelectionToFront),
            item("Send to Back", PreviewMsg::SelectionToBack),
            item("Move Up", PreviewMsg::RaiseSelected),
            item("Move Down", PreviewMsg::LowerSelected),
            divider(),
            item("Delete", PreviewMsg::DeleteSelected),
        ]
    } else {
        // Three groups: color/duplicate · z-order actions · delete — each pair split by a rule.
        vec![
            item("Set to current color", PreviewMsg::SetSelectedColor),
            item("Duplicate", PreviewMsg::DuplicateSelected),
            divider(),
            item("Bring to Front", PreviewMsg::SelectionToFront),
            item("Send to Back", PreviewMsg::SelectionToBack),
            item("Move Up", PreviewMsg::RaiseSelected),
            item("Move Down", PreviewMsg::LowerSelected),
            divider(),
            item("Delete", PreviewMsg::DeleteSelected),
        ]
    };
    let col = widget::column(rows).spacing(2.0);
    widget::container(col)
        .width(Length::Fixed(170.0))
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
        }))
        .into()
}

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
        // WRAP the ZoomPan in the annotation interaction canvas (it OWNS the ZoomPan as its
        // child): iced's `stack` doesn't reliably propagate Ignored mouse events from a top
        // sibling to a lower one, so a sibling-over layout left pan/scrollbars dead. As the
        // owner, the canvas forwards every event it doesn't consume to the ZoomPan. The
        // committed shapes are drawn by the canvas itself as TRUE VECTOR geometry
        // (DRAGON-324) — crisp at any zoom, no raster to blur — along with the (never-baked)
        // selection chrome, both clipped to the content rect. The full-res bake rasterizes
        // the same scene separately.
        let canvas_over: Element<'a, Msg> = if content_px.0 > 0.0 && preview.edit.frame.0 > 0 {
            // The in-flight eraser's marked groups draw at half opacity (DRAGON-338) — the
            // preview of what releasing the button deletes.
            let items = annotate::widget_items(
                &preview.edit.annotations,
                preview.edit.curve_radius(),
                &preview.edit.erase_marks,
            );
            let accent = crate::app::theme::accent(&cosmic::theme::active());
            let source = (preview.edit.frame.0 as f32, preview.edit.frame.1 as f32);
            crate::widgets::annotation_canvas::AnnotationCanvas::new(
                image,
                items,
                // The whole selection, in selection order — the last id is the PRIMARY (the one
                // wearing resize handles). DRAGON-341.
                preview.edit.sel.ids().iter().map(|id| id.0).collect(),
                preview.edit.tool,
                preview.view.zoom,
                preview.view.pan,
                content_px,
                source,
                preview.view.pan_mode,
                accent,
                |ev| {
                    use crate::widgets::annotation_canvas::AnnotEvent;
                    Msg::Preview(match ev {
                        AnnotEvent::Select(o) => PreviewMsg::SelectAnnotation(o.map(AnnotId)),
                        AnnotEvent::SelectToggle(id) => {
                            PreviewMsg::ToggleAnnotationSelected(AnnotId(id))
                        }
                        AnnotEvent::BoxSelect(x0, y0, x1, y1, add) => {
                            PreviewMsg::BandSelectAnnotations(x0, y0, x1, y1, add)
                        }
                        AnnotEvent::DrawBegin(t, x, y) => PreviewMsg::AnnotDrawBegin(t, x, y),
                        AnnotEvent::GrabBegin(g, x, y) => PreviewMsg::AnnotGrabBegin(g, x, y),
                        AnnotEvent::GestureTo(x, y) => PreviewMsg::AnnotGestureTo(x, y),
                        AnnotEvent::GestureEnd => PreviewMsg::AnnotGestureEnd,
                        AnnotEvent::Menu(x, y) => PreviewMsg::AnnotMenuOpen(x, y),
                    })
                },
            )
            .into()
        } else {
            image.into()
        };
        // A right-click context menu floats over the selected item at the click point. A
        // Spotlight selection gets the restricted (Delete-only) menu.
        let spotlight_selected = preview
            .edit
            .selected()
            .and_then(|id| preview.edit.annotations.iter().find(|it| it.id == id))
            .is_some_and(|it| matches!(it.kind, annotate::AnnotKind::Spotlight { .. }));
        let canvas_over: Element<'a, Msg> = match preview.edit.annot_menu {
            Some((mx, my)) => widget::popover(canvas_over)
                .popup(annot_context_menu(spotlight_selected))
                .position(widget::popover::Position::Point(cosmic::iced::Point::new(mx, my)))
                .on_close(Msg::Preview(PreviewMsg::AnnotMenuClose))
                .into(),
            None => canvas_over,
        };
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
            tb.pan_tool_group(preview.view.pan_mode, &self.keymap),
        ];
        let toolbar = toolbar_row(left, Vec::new(), right);
        // The overlay's header line (appearance / undo / redo ⟨split⟩ Close); the windowed
        // preview carries those in its titlebar instead (DRAGON-337).
        let header = (!preview.surface.is_window())
            .then(|| self.overlay_header_row(preview, tb));
        compose_preview(
            preview.surface.is_window(),
            self.overlay_control_width(preview),
            header,
            self.edit_toolbar(preview, tb),
            canvas_over,
            None,
            toolbar,
            tb.glass,
        )
    }

    /// The base still plus the effect + covermark overlays for the loaded-image view, fitted to
    /// `dw`×`dh`. Stack order (bottom→top) mirrors the bake: base, then the REGION EFFECTS
    /// (highlight / pixelate / blur) rendered in true z-order by the real-time GPU shader
    /// ([`crate::widgets::annotation_fx`], DRAGON-330 — no CPU raster, updates every frame as
    /// the user drags), then the covermark (its own persistent-texture `LayerStack`). Box/arrow
    /// stay vector geometry drawn by the `AnnotationCanvas` over this surface. All three ride
    /// the ZoomPan transform (in its content) so they zoom/pan locked to the picture and clip to
    /// the media viewport. Portable path: a STABLE `widget::image` base. Windows OVERLAY
    /// exception (DRAGON-235): iced's raster-image pipeline does not composite on the
    /// premultiplied transparent surface, so the base (and covermark) fold into ONE `LayerStack`
    /// drawn through the shader; the effects shader stacks on top of it (over the covermark on
    /// Windows — a z-order deviation vs Linux/mac to VERIFY, since Linux builds can't exercise
    /// this cfg arm). The opaque windowed surface, Linux (layer-shell) and macOS keep
    /// `widget::image` and compile only the portable path below (byte-identical).
    fn still_media(
        &self,
        preview: &PreviewState,
        handle: &widget::image::Handle,
        dw: f32,
        dh: f32,
    ) -> Element<'static, Msg> {
        use crate::widgets::annotation_fx::{EffectsFx, FxConst, FxEffect, FxItem};
        // The region-effect items, in scene z-order (SOURCE-pixel geometry). Box/arrow are
        // vectors drawn by the AnnotationCanvas, never here. The pristine full-res source pixels
        // (`fx_base`) size the CONTENT-AWARE pixelate cell via the SAME shared analyzer the bake
        // calls, so display + bake pick identical blocks (WYSIWYG).
        let fx_src = preview.edit.fx_base.as_ref();
        let fx_items: Vec<FxItem> = preview
            .edit
            .annotations
            .iter()
            .filter_map(|it| {
                let (rect, effect) = match &it.kind {
                    annotate::AnnotKind::Highlight { rect } => (rect, FxEffect::Highlight),
                    // BoxHighlight feeds its FILL to the GPU shader as a plain Highlight; its
                    // box outline is drawn by the AnnotationCanvas, not here (DRAGON-333).
                    annotate::AnnotKind::BoxHighlight { rect, .. } => (rect, FxEffect::Highlight),
                    annotate::AnnotKind::Pixelate { rect } => (rect, FxEffect::Pixelate),
                    annotate::AnnotKind::Blur { rect } => (rect, FxEffect::Blur),
                    _ => return None,
                };
                // Recomputed each view build (so a live drag updates the mosaic as the rect moves
                // over different content); cheap (a bounded strided sample). Only pixelate uses it.
                let pixelate_block = if matches!(effect, FxEffect::Pixelate) {
                    let b = fx_src
                        .map(|f| annotate::content_pixelate_block_px(&f.rgba, f.w, f.h, rect))
                        .unwrap_or(annotate::PIXELATE_BLOCK);
                    if b >= annotate::PIXELATE_BLOCK_MAX {
                        warn_pixelate_cap();
                    }
                    b as f32
                } else {
                    0.0
                };
                Some(FxItem {
                    rect: [rect.x, rect.y, rect.w, rect.h],
                    effect,
                    color: [
                        it.color[0] as f32 / 255.0,
                        it.color[1] as f32 / 255.0,
                        it.color[2] as f32 / 255.0,
                    ],
                    pixelate_block,
                })
            })
            .collect();
        // The global dim (DRAGON-329) + its knockout rects (SOURCE px): the union of spotlight /
        // box / highlight / box-highlight rects, capped at the shader's uniform-array size.
        let dim = preview.edit.dim;
        let mut knockouts: Vec<[f32; 4]> = annotate::knockout_rects(&preview.edit.annotations)
            .into_iter()
            .map(|r| [r.x, r.y, r.w, r.h])
            .collect();
        if knockouts.len() > crate::widgets::annotation_fx::MAX_KNOCKOUTS {
            log::warn!(
                "dim/spotlight: {} knockout rects exceeds the {}-rect shader cap; only the first \
                 {} are rendered (the bake stays faithful)",
                knockouts.len(),
                crate::widgets::annotation_fx::MAX_KNOCKOUTS,
                crate::widgets::annotation_fx::MAX_KNOCKOUTS,
            );
            knockouts.truncate(crate::widgets::annotation_fx::MAX_KNOCKOUTS);
        }
        // The real-time effects shader element runs when there are region effects OR a non-zero
        // dim, AND there are retained base pixels to sample. Mirrors the bake's block sizes +
        // highlight weight (SOURCE px). With no effects and no dim it is None — byte-identical.
        let fx_element: Option<Element<'static, Msg>> =
            match ((!fx_items.is_empty() || dim > 0.0), preview.edit.fx_base.clone()) {
                (true, Some(base)) => {
                    let consts = FxConst {
                        blur_block: annotate::BLUR_BLOCK as f32,
                        highlight_weight: annotate::HIGHLIGHT_ALPHA as f32 / 255.0,
                        blur_passes: annotate::BLUR_PASSES,
                    };
                    let src = (preview.edit.frame.0 as f32, preview.edit.frame.1 as f32);
                    let fx = EffectsFx::new(
                        base,
                        fx_items,
                        src,
                        (dw, dh),
                        preview.edit.curve_radius(),
                        consts,
                        dim,
                        knockouts,
                    );
                    let shader = cosmic::iced::widget::shader::Shader::new(fx)
                        .width(Length::Fixed(dw))
                        .height(Length::Fixed(dh));
                    Some(widget::container(Element::new(shader)).center_x(Length::Fill).into())
                }
                _ => None,
            };
        // The covermark overlay (its own LayerStack — a persistent texture updated in place).
        let cm_layers: Vec<Layer> = preview
            .edit
            .cm_raster
            .frame()
            .map(|cm| vec![Layer { key: LayerKey::COVERMARK, frame: cm.clone() }])
            .unwrap_or_default();
        // The `widget::image` base (Linux/mac + the opaque windowed surface — byte-identical).
        let image_base: Element<'static, Msg> = widget::container(
            widget::image(handle.clone())
                .content_fit(cosmic::iced::ContentFit::Fill)
                .width(Length::Fixed(dw))
                .height(Length::Fixed(dh)),
        )
        .center_x(Length::Fill)
        .into();
        // The base element, plus whether the covermark is folded into it. Windows OVERLAY
        // (DRAGON-235): iced's raster-image pipeline doesn't composite on the transparent
        // surface, so base + covermark ride ONE LayerStack (two LayerStacks on a surface fight
        // over slot pruning); the effects shader is a distinct primitive type, stacked on top —
        // over the covermark on Windows (a z-order deviation vs Linux/mac to VERIFY, as Linux
        // builds can't exercise this cfg arm).
        #[cfg(windows)]
        let (base_element, cm_folded): (Element<'static, Msg>, bool) = if !preview
            .surface
            .is_window()
            && let Some(base) = super::layers::rgba_handle_frame(handle)
        {
            let mut layers = vec![Layer { key: LayerKey::VIDEO, frame: base }];
            layers.extend(cm_layers.iter().cloned());
            let base_ls = cosmic::iced::widget::shader::Shader::new(LayerStack::new(layers))
                .width(Length::Fixed(dw))
                .height(Length::Fixed(dh));
            (widget::container(Element::new(base_ls)).center_x(Length::Fill).into(), true)
        } else {
            (image_base, false)
        };
        #[cfg(not(windows))]
        let (base_element, cm_folded): (Element<'static, Msg>, bool) = (image_base, false);

        // Bottom→top: base, real-time effects shader, covermark (unless folded into the base).
        let mut children: Vec<Element<'static, Msg>> = vec![base_element];
        if let Some(fx) = fx_element {
            children.push(fx);
        }
        if !cm_folded && !cm_layers.is_empty() {
            let shader = cosmic::iced::widget::shader::Shader::new(LayerStack::new(cm_layers))
                .width(Length::Fixed(dw))
                .height(Length::Fixed(dh));
            children.push(widget::container(Element::new(shader)).center_x(Length::Fill).into());
        }
        if children.len() == 1 {
            return children.pop().expect("one child");
        }
        cosmic::iced::widget::stack(children).into()
    }
}
