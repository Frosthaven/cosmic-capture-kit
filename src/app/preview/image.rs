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
fn annot_context_menu(pid: window::Id, spotlight: bool) -> Element<'static, Msg> {
    let item = |label: &'static str, msg: PreviewMsg| -> Element<'static, Msg> {
        crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::custom(widget::text(label).size(13))
                .width(Length::Fill)
                .class(cosmic::theme::Button::Text)
                .on_press(Msg::Preview(pid, msg)),
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
pub(super) fn decode_task(pid: window::Id, path: PathBuf) -> Task<cosmic::Action<Msg>> {
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
    Task::perform(rx, move |res| {
        cosmic::Action::App(Msg::Preview(pid, match res {
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
        toasts: Option<Element<'a, Msg>>,
    ) -> Element<'a, Msg> {
        // Every message this view emits is ADDRESSED to its own document (DRAGON-336
        // phase 2), so a click in one preview can never act on another.
        let pid = preview.window;
        // `is_loading()` guarantees `image` is Some here; fall back to the spinner just
        // in case, so this is never an empty frame.
        let Some(handle) = &img.image else {
            return self.preview_loading_view(preview, tb, toasts);
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
        // DRAGON-366 (TEMPORARY): the view build's own wall clock. See
        // `crate::widgets::dragon366` — remove with the probe call at the end of this fn.
        let d366_view_start = std::time::Instant::now();
        let mut d366_still_ms = 0.0f64;
        let mut d366_shown = (0.0f32, 0.0f32);
        let mut d366_avail = (0.0f32, 0.0f32);
        // Every persistent-texture layer key this window draws this frame (DRAGON-373): the
        // covermark's, the Windows-overlay base's, and one per text annotation. EVERY LayerStack
        // this window mounts must carry the same set, or their prepares take turns freeing each
        // other's textures — see `layers.rs`. The Windows base key is included whenever that arm
        // could fold (over-approximating is safe; under-approximating frees a live texture).
        let mut window_keys: Vec<LayerKey> = preview
            .edit
            .text_layers
            .iter()
            .map(|l| LayerKey::text(preview.window, l.id.0))
            .collect();
        if preview.edit.cm_raster.frame().is_some() {
            window_keys.push(LayerKey::covermark(preview.window));
        }
        #[cfg(windows)]
        if !preview.surface.is_window() {
            window_keys.push(LayerKey::video(preview.window));
        }
        let (ow, oh) = preview.frame_points();
        let image: Element<'a, Msg> = if ow > 0 && oh > 0 {
            let (avail_w, avail_h) = self.preview_viewport(preview);
            let (dw, dh) = video::fit_dims(ow, oh, avail_w, avail_h);
            d366_avail = (avail_w, avail_h);
            d366_shown = (dw, dh);
            let t = std::time::Instant::now();
            let media = self.still_media(preview, handle, dw, dh, &window_keys);
            d366_still_ms = t.elapsed().as_secs_f64() * 1000.0;
            media
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
            move |step, ux, uy| Msg::Preview(pid, PreviewMsg::Zoom(step, ux, uy)),
            move |dx, dy| Msg::Preview(pid, PreviewMsg::Pan(dx, dy)),
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
            // The blinking text caret (DRAGON-354): box-relative geometry (source px) of the
            // edited box, passed only on a blink-ON tick so `None` reads as "no caret this
            // frame". Computed from the SAME layout the renderer uses, so it lands between the
            // right glyphs at any zoom.
            // The UN-blinked caret geometry (DRAGON-359): computed once from the SAME layout the
            // renderer uses, so it lands between the right glyphs at any zoom. It drives the OS
            // IME cursor area every frame (the emoji picker / composition candidate window
            // anchors here); the DRAWN caret below is this same geometry, but gated by the blink.
            let ime_caret = preview.edit.text_edit.as_ref().and_then(|te| {
                preview.edit.annotations.iter().find(|it| it.id == te.id).and_then(|it| {
                    match &it.kind {
                        annotate::AnnotKind::Text { rect, text, size_px, font, constrained, .. } => {
                            let lay = annotate::text_kind_layout(
                                text, *size_px, *font, *rect, *constrained, source.0,
                            );
                            Some(text_annot::caret_geometry(&lay, *font, *size_px, te.caret))
                        }
                        _ => None,
                    }
                })
            });
            // The blinking text caret (DRAGON-354): passed only on a blink-ON tick so `None`
            // reads as "no caret this frame".
            let text_caret = preview
                .edit
                .text_edit
                .as_ref()
                .filter(|te| te.blink_on)
                .and(ime_caret);
            // The edited box id + its selection-highlight rects (DRAGON-354 item 12): derived
            // from the SAME layout, so the wash lands under exactly the selected glyphs.
            let editing_text = preview.edit.text_edit.as_ref().map(|te| te.id.0);
            let text_selection: Vec<(f32, f32, f32, f32)> = preview
                .edit
                .text_edit
                .as_ref()
                .and_then(|te| te.selection().map(|(s, e)| (te.id, s, e)))
                .and_then(|(tid, s, e)| {
                    preview.edit.annotations.iter().find(|it| it.id == tid).and_then(|it| {
                        match &it.kind {
                            annotate::AnnotKind::Text { rect, text, size_px, font, constrained, .. } => {
                                let lay = annotate::text_kind_layout(
                                    text, *size_px, *font, *rect, *constrained, source.0,
                                );
                                Some(text_annot::selection_rects(&lay, *font, *size_px, s, e))
                            }
                            _ => None,
                        }
                    })
                })
                .unwrap_or_default();
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
                move |ev| {
                    use crate::widgets::annotation_canvas::AnnotEvent;
                    Msg::Preview(pid, match ev {
                        AnnotEvent::Select(o) => PreviewMsg::SelectAnnotation(o.map(AnnotId)),
                        AnnotEvent::SelectToggle(id) => {
                            PreviewMsg::ToggleAnnotationSelected(AnnotId(id))
                        }
                        AnnotEvent::BoxSelect(x0, y0, x1, y1, add) => {
                            PreviewMsg::BandSelectAnnotations(x0, y0, x1, y1, add)
                        }
                        AnnotEvent::DrawBegin(t, x, y) => PreviewMsg::AnnotDrawBegin(t, x, y),
                        AnnotEvent::GrabBegin(g, x, y) => PreviewMsg::AnnotGrabBegin(g, x, y),
                        AnnotEvent::GestureTo(x, y, scale_type) => {
                            PreviewMsg::AnnotGestureTo(x, y, scale_type)
                        }
                        AnnotEvent::GestureEnd => PreviewMsg::AnnotGestureEnd,
                        AnnotEvent::EditText(aid) => PreviewMsg::EditText(AnnotId(aid)),
                        AnnotEvent::TextClick { x, y, extend, word, all } => {
                            PreviewMsg::TextClickAt { x, y, extend, word, all }
                        }
                        AnnotEvent::TextDragTo(x, y) => PreviewMsg::TextDragTo(x, y),
                        AnnotEvent::ImeCommit(s) => PreviewMsg::TextImeCommit(s),
                        AnnotEvent::Menu(x, y) => PreviewMsg::AnnotMenuOpen(x, y),
                    })
                },
            )
            .text_caret(text_caret)
            // The un-blinked caret drives the OS IME cursor area while editing (DRAGON-359).
            .ime_caret(ime_caret)
            // The id being edited + its text-selection rects (DRAGON-356 in-box shift guard +
            // DRAGON-354 item 12 drag-select). `editing_text` is set for the WHOLE edit, unlike
            // the blink-gated caret above.
            .text_editing(editing_text, text_selection)
            // The TEXT rasters (DRAGON-373): one passive, draw-only layer per text annotation,
            // handed to the canvas so it can draw each at its OWN place in the item order —
            // which is what makes a rectangle brought over one caption and under another render
            // on screen the way `rasterize_scene` has always baked it. They are elements, not
            // widgets in the tree: the canvas never routes an event to them, so hit-testing
            // stays entirely with the canvas's own model (see its `text_layers`).
            .text_layers(
                preview
                    .edit
                    .text_layers
                    .iter()
                    .map(|l| {
                        let layer = Layer::at(
                            LayerKey::text(preview.window, l.id.0),
                            l.frame.clone(),
                            // The layer covers only its own caption REGION (DRAGON-362), so it is
                            // PLACED rather than stretched — see `layers::Layer::dest`.
                            l.geom.dest(preview.edit.frame),
                        );
                        let stack = LayerStack::part(
                            vec![layer],
                            self.live_preview_windows(),
                            window_keys.clone(),
                        );
                        (l.id.0, Element::new(cosmic::iced::widget::shader::Shader::new(stack)))
                    })
                    .collect(),
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
        // The popover WRAPPER is unconditional (DRAGON-375's class): only the POPUP comes and
        // goes. `Popover::children` puts its content at index 0 whether or not a popup is
        // attached, so the `AnnotationCanvas` subtree — which holds the in-flight gesture in its
        // widget state — keeps its identity when the menu opens or closes. Wrapping conditionally
        // changed the tag at this position, and iced answers a tag change by rebuilding the whole
        // subtree, so dismissing the menu with a press-drag threw the gesture away as it started.
        let mut menu_over = widget::popover(canvas_over);
        if let Some((mx, my)) = preview.edit.annot_menu {
            menu_over = menu_over
                .popup(annot_context_menu(pid, spotlight_selected))
                .position(widget::popover::Position::Point(cosmic::iced::Point::new(mx, my)))
                .on_close(Msg::Preview(pid, PreviewMsg::AnnotMenuClose));
        }
        let canvas_over: Element<'a, Msg> = menu_over.into();
        // Left: do-not-train + covermark tools. Right: the size + Delete group. (Save / Save
        // As / Copy, appearance, and Close live on the top bar.) Center reserved for the zoom
        // scale.
        // `Vec<Element<'static, _>>` is a subtype of `Vec<Element<'a, _>>` (Element
        // is covariant in its lifetime), so this is a plain re-binding.
        let left: Vec<Element<'a, Msg>> = self.edit_tools(preview, tb);
        // Right: the zoom scale (Fit/%/presets), then the pointer/pan tools at the far right.
        // Filesize chip DISABLED pending a decision on whether it belongs on the bottom bar.
        // Left commented (not deleted) so it can be restored intact (prepend the extend below);
        // the size chip block also dropped out of the bottom-bar min-width reserve in
        // `surface.rs` while it's off.
        // right.extend(tb.size_chip(preview.size));
        let right: Vec<Element<'a, Msg>> = vec![
            self.zoom_control(preview, tb),
            tb.pan_tool_group(preview.view.pan_mode, &self.keymap),
        ];
        let toolbar = toolbar_row(left, Vec::new(), right);
        // The overlay's header line (appearance / undo / redo ⟨split⟩ Close); the windowed
        // preview carries those in its titlebar instead (DRAGON-337).
        let header = (!preview.surface.is_window())
            .then(|| self.overlay_header_row(preview, tb));
        let composed = compose_preview(
            preview.surface.is_window(),
            self.overlay_control_width(preview),
            header,
            self.edit_toolbar(preview, tb),
            canvas_over,
            None,
            toolbar,
            tb.glass,
            toasts,
        );
        // DRAGON-366 (TEMPORARY): one line per new interaction plus a sampled stream within it,
        // carrying the redraw cadence, our own CPU cost, the scene shape, and WHAT THE USER WAS
        // DOING — the split the ticket hangs on. Remove this call, `d366_interaction`, and the
        // timers above with the diagnostic.
        let (d366_verb, d366_item) = self.d366_interaction(preview);
        let mut d366_fx = (0usize, 0usize, 0usize);
        for it in &preview.edit.annotations {
            match it.kind {
                annotate::AnnotKind::Blur { .. } => d366_fx.0 += 1,
                annotate::AnnotKind::Pixelate { .. } => d366_fx.1 += 1,
                annotate::AnnotKind::Highlight { .. }
                | annotate::AnnotKind::BoxHighlight { .. } => d366_fx.2 += 1,
                _ => {}
            }
        }
        crate::widgets::dragon366::view_built(crate::widgets::dragon366::FrameFacts {
            verb: d366_verb,
            item: d366_item,
            build_ms: d366_view_start.elapsed().as_secs_f64() * 1000.0,
            still_ms: d366_still_ms,
            source: preview.edit.frame,
            shown: d366_shown,
            avail: d366_avail,
            zoom: preview.view.zoom,
            overlay: !preview.surface.is_window(),
            annots: preview.edit.annotations.len(),
            fx_blur: d366_fx.0,
            fx_pixelate: d366_fx.1,
            fx_highlight: d366_fx.2,
            dim: preview.edit.dim,
            covermark: preview.edit.cm_raster.frame().is_some(),
            text_layer: !preview.edit.text_layers.is_empty(),
        });
        composed
    }

    /// DRAGON-366 (TEMPORARY): name the interaction currently on screen, as
    /// `(verb, item-kind)`, from state the view can already see. This is what makes the
    /// diagnostic self-interpreting — comparing `idle` against `drag/text` against
    /// `create/blur` on the SAME capture is what separates a per-frame cost that is paid
    /// regardless (the base image) from one driven by a specific interaction.
    ///
    /// An in-flight pointer gesture wins over text editing, because during a drag of a text box
    /// both are live and the DRAG is what is being measured.
    fn d366_interaction(&self, preview: &PreviewState) -> (&'static str, &'static str) {
        match &preview.edit.gesture {
            // Rubber-band drawing / moving / resizing / erasing — named by the SHARED helper, so
            // the view channel and the update channel label one drag identically.
            Some(g) => annotate::d366_gesture_words(g, &preview.edit.annotations),
            // No pointer gesture: typing into a text box, or genuinely idle. The idle frames
            // between the owner's actions are the ONLY no-effect baseline this ticket gets.
            None => match &preview.edit.text_edit {
                Some(_) => ("type", "text"),
                None => ("idle", "-"),
            },
        }
    }

    /// The base still plus the effect + covermark overlays for the loaded-image view, fitted to
    /// `dw`×`dh`. Stack order (bottom→top) mirrors the bake: base, then the REGION EFFECTS
    /// (highlight / pixelate / blur) rendered in true z-order by the real-time GPU shader
    /// ([`crate::widgets::annotation_fx`], DRAGON-330 — no CPU raster, updates every frame as
    /// the user drags), then the covermark (its own persistent-texture `LayerStack`). All three
    /// ride the ZoomPan transform (in its content) so they zoom/pan locked to the picture and
    /// clip to the media viewport. Everything ABOVE the covermark in the bake — box / arrow /
    /// pen / badge as vector geometry AND the text boxes as per-item rasters — is drawn by the
    /// `AnnotationCanvas` over this surface, in ONE in-order pass (DRAGON-373), which is what
    /// makes the live stack the same composite as `bake_image`'s: dim → effects → covermark →
    /// the annotation scene in item order.
    ///
    /// `window_keys` is every layer key this WINDOW draws this frame, across the canvas's text
    /// layers as well as the stacks built here — the multi-stack prune contract in `layers.rs`.
    /// It may over-approximate (a key that ends up not drawn merely keeps a slot alive until the
    /// preview closes); it must never under-approximate, or a live layer's texture is freed.
    /// Portable path: a STABLE `widget::image` base. Windows OVERLAY
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
        window_keys: &[LayerKey],
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
                        // The owning window keys ALL of this shader's GPU state — iced's
                        // pipeline storage is per-process, shared by every window's renderer.
                        preview.window,
                        // ...and the OPEN set lets this prepare free a CLOSED preview's
                        // state, which nothing else can reach (DRAGON-336).
                        self.live_preview_windows(),
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
        // The covermark's raster overlay, stacked over the base/effects — a persistent texture
        // updated in place. The TEXT layers are NO LONGER here (DRAGON-373): they are drawn by
        // the `AnnotationCanvas`, interleaved with the vector kinds in true z-order, because a
        // layer stacked at a fixed depth can never be UNDER a rectangle drawn after it.
        let cm_layers: Vec<Layer> = preview
            .edit
            .cm_raster
            .frame()
            .map(|cm| vec![Layer::full(LayerKey::covermark(preview.window), cm.clone())])
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
        // surface, so base + covermark ride ONE LayerStack; the effects shader is a distinct
        // primitive type (its own per-window state), stacked on top —
        // over the covermark on Windows (a z-order deviation vs Linux/mac to VERIFY, as Linux
        // builds can't exercise this cfg arm). Note the text layers are NOT folded in here on
        // either platform: they are canvas-drawn (DRAGON-373), which keeps this arm's z-order
        // exactly as it was — text has always ridden above base, effects and covermark alike.
        #[cfg(windows)]
        let (base_element, cm_folded): (Element<'static, Msg>, bool) = if !preview
            .surface
            .is_window()
            && let Some(base) = super::layers::rgba_handle_frame(handle)
        {
            let mut layers = vec![Layer::full(LayerKey::video(preview.window), base)];
            layers.extend(cm_layers.iter().cloned());
            let base_ls = cosmic::iced::widget::shader::Shader::new(LayerStack::part(
                layers,
                self.live_preview_windows(),
                window_keys.to_vec(),
            ))
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
            let shader = cosmic::iced::widget::shader::Shader::new(LayerStack::part(
                cm_layers,
                self.live_preview_windows(),
                window_keys.to_vec(),
            ))
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
