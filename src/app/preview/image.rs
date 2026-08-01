//! Still-image preview: decode the screenshot off-thread and show it at native size
//! (downscaled only if it exceeds the monitor, never upscaled) with the shared
//! Save / Save As / Copy / Cancel action bar. Covermark edits recomposite from the
//! retained original pixels, so the display is exactly what a bake writes.

use super::layers::{Layer, LayerKey, LayerStack};
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Note ONCE per process that a content-aware pixelate cell hit the
/// [`annotate::PIXELATE_BLOCK_MAX`] ceiling — the block is safely clamped there (bounding the
/// GPU shader's O(block²) mosaic loop), so this is a perf heads-up, not an error. Once-guarded
/// so a live drag over coarse content can't spam the log.
///
/// DRAGON-453: at `debug`, not `warn`. Clamping here is the design working — a coarse-content
/// region resolves to the ceiling and both the display and the bake use it, so nothing is
/// degraded and there is nothing for the user to act on. At `warn` it printed to the terminal
/// during an ordinary pixelate drag, which reads as a fault and is not one. `debug` still puts
/// it in the DRAGON-419 debug log (our own records go to the file from `debug` up), which is
/// where a "why is my redaction this coarse?" question gets answered.
fn note_pixelate_cap() {
    static NOTED: AtomicBool = AtomicBool::new(false);
    if !NOTED.swap(true, Ordering::Relaxed) {
        log::debug!(
            "pixelate: content-aware cell clamped to the {}px ceiling (bounds the GPU mosaic \
             loop); very large redaction regions stay capped",
            annotate::PIXELATE_BLOCK_MAX
        );
    }
}

/// Warn ONCE per process that the scene holds more knockout rects than the dim/spotlight shader
/// can carry, so the LIVE view is showing fewer than the document has (the CPU bake has no cap
/// and stays faithful).
///
/// Kept at `warn` — unlike [`note_pixelate_cap`] this one says what you see is not what you get,
/// which is worth a terminal line. DRAGON-453 added the once-guard: this sits in the view build,
/// so before it the SAME line was emitted on every frame for as long as the document stayed over
/// the cap — a per-frame `warn!` that would flood a customer's debug log.
fn warn_knockout_cap(count: usize) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        log::warn!(
            "dim/spotlight: {count} knockout rects exceeds the {}-rect shader cap; only the \
             first {} are rendered (the bake stays faithful)",
            crate::widgets::annotation_fx::MAX_KNOCKOUTS,
            crate::widgets::annotation_fx::MAX_KNOCKOUTS,
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
        // DRAGON-454: the decode is the longest single step of the editor open on a large
        // capture, and it runs HERE — off the UI thread. Marking both ends is what lets a
        // reader tell "the picture was still decoding" from "the UI thread was busy".
        crate::util::timing_mark("preview: image decode thread (begin)");
        let payload = match ::image::open(&path) {
            Ok(img) => {
                // Wrap in Arc FIRST so the handle can SHARE the decoded pixel allocation
                // (via the zero-copy `shared_rgba_handle`) instead of cloning it — the
                // original stays available as the edit recomposite source either way.
                let original = Arc::new(img.into_rgba8());
                let handle = shared_rgba_handle(&original);
                if crate::util::timing_on() {
                    let (w, h) = original.dimensions();
                    crate::util::timing_mark(&format!(
                        "preview: image decode thread (done, {w}x{h})"
                    ));
                }
                (handle, Some(original))
            }
            // DRAGON-419: a decode failure is HANDLED (iced re-reads the file itself), so
            // this is not a session-ending path — but it is the first sign that a capture we
            // just wrote is unreadable, and it was discarded entirely. The error text is the
            // image crate's own ("unsupported colour type", "unexpected EOF"), not a path.
            Err(e) => {
                log::warn!("preview decode failed, falling back to a file-backed handle: {e}");
                (widget::image::Handle::from_path(&path), None)
            }
        };
        let _ = tx.send(payload);
    });
    Task::perform(rx, move |res| {
        cosmic::Action::App(Msg::Preview(pid, match res {
            Ok((handle, original)) => PreviewMsg::ImageReady(handle, original),
            // DRAGON-419 (silent-exit path S4). `Err` here means the DECODE THREAD DIED —
            // the oneshot sender was dropped without sending, which the arm above cannot
            // produce because it always sends. The capture is already on disk; the editor is
            // what was lost, so this presents as "the capture worked, the editor never
            // appeared" and nothing said so.
            Err(_) => {
                crate::diag::note_failure(
                    crate::diag::Failure::DecodeFailed,
                    "image decode worker died before reporting (panicked); the preview \
                     document is being cancelled and the file is already on disk",
                );
                // DRAGON-415: a distinct message rather than a plain `Cancel`, which is
                // indistinguishable from the user closing the window. Its handler reports
                // the failure and then closes exactly as `Cancel` does.
                PreviewMsg::LoadFailed
            }
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
        // it's sharp on hidpi. `source_scale == 1.0` (an unscaled output) makes points == physical
        // — byte-identical to the old `edit.frame` fit.
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
        // DRAGON-385: the DISPLAY frame — the crop's framing once applied, else the whole
        // picture (a crop SESSION shows the whole picture too, so it is un-cropped here). The fit
        // box, the ZoomPan content and the canvas mapping all key off it.
        let (ow, oh) = preview.display_frame_points();
        let view_crop = preview.view_crop();
        let image: Element<'a, Msg> = if ow > 0 && oh > 0 {
            let (avail_w, avail_h) = self.preview_viewport(preview);
            // The crop window's on-screen size (whole picture's when un-cropped).
            let (dw, dh) = video::fit_dims(ow, oh, avail_w, avail_h);
            // The media stack ALWAYS renders the WHOLE frame (base + effect passes); a crop just
            // frames a sub-region of it through a CropWindow. So render at the FULL frame's
            // on-screen size (the whole picture at the crop's scale), then clip to the crop window.
            // Un-cropped: render_dims == (dw, dh) and there is no wrapper — byte-identical to the
            // historical path.
            //
            // The COVERMARK is the exception (DRAGON-391): it spans THE IMAGE, and once a crop is
            // applied the image is the CROP RECT — not the source frame. A layer inside the media
            // stack is laid out at the source's size, so it can only ever cover the source (that is
            // what left an over-crop's extension bare). With a crop applied it therefore mounts as
            // a sibling filling the CropWindow — the one element that spans exactly the display
            // frame — so it covers the whole cropped image, extension included, and is scissored at
            // its edges so nothing paints beyond. Un-cropped the media stack IS the image, so the
            // covermark stays exactly where it always rode.
            //
            // ...and inside a crop SESSION it is not drawn at all (DRAGON-402,
            // `EditState::covermark_visible`). Only the media-stack mount can be reached there —
            // `view_crop()` is `None` for the duration, so `crop_wrap` is `None` and the sibling
            // below cannot arise — which is why suppressing it here is the whole change.
            let (render_w, render_h, crop_wrap) = match view_crop {
                Some(c) => {
                    // Aspect is preserved, so the width ratio is the single screen-px-per-source-px
                    // scale for both axes.
                    let cw = c.pixel_size().0;
                    let s = dw / (cw.max(1) as f32); // screen px per SOURCE px
                    let (fw, fh) = preview.edit.frame;
                    let full = (fw as f32 * s, fh as f32 * s);
                    (full.0, full.1, Some(((dw, dh), full, (-c.x * s, -c.y * s))))
                }
                None => (dw, dh, None),
            };
            // `false` withholds the covermark from the media stack — because it is mounted over the
            // crop window below instead (DRAGON-391), or because a crop session is live and it is
            // not drawn at all (DRAGON-402).
            let media = self.still_media(
                preview,
                handle,
                render_w,
                render_h,
                &window_keys,
                view_crop.is_none() && preview.edit.covermark_visible(),
            );
            match crop_wrap {
                Some((window, content, offset)) => {
                    let framed: Element<'a, Msg> =
                        crate::widgets::CropWindow::new(media, window, content, offset).into();
                    match self.covermark_stack(preview, &window_keys, window.0, window.1) {
                        Some(cm) => cosmic::iced::widget::stack(vec![framed, cm]).into(),
                        None => framed,
                    }
                }
                None => media,
            }
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
            // and scrollbars track the REAL displayed picture size (DRAGON-221) — of the
            // DISPLAY frame (the crop window when a crop is applied, DRAGON-385).
            let (iw, ih) = preview.display_frame_points();
            if iw > 0 && ih > 0 {
                let (avail_w, avail_h) = self.preview_viewport(preview);
                video::fit_dims(iw, ih, avail_w, avail_h)
            } else {
                (0.0, 0.0)
            }
        };
        // The DISPLAYED source dims + the view-crop offset the annotation canvas maps through
        // (DRAGON-385): the crop's size + origin once applied, else the whole frame + `(0, 0)`.
        // Model geometry stays FULL-source; this only shifts to the cropped on-screen content.
        let canvas_source = {
            let d = preview.display_frame();
            (d.0 as f32, d.1 as f32)
        };
        let canvas_offset = preview.display_offset();
        let image = crate::widgets::ZoomPan::new(
            slot,
            preview.view.zoom,
            preview.view.pan,
            preview.edit.pan_active(),
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
        let canvas_over: Element<'a, Msg> = if let Some(session) = preview
            .edit
            .crop_session
            .as_ref()
            .filter(|_| content_px.0 > 0.0 && preview.edit.frame.0 > 0)
        {
            // DRAGON-382/387: while a crop session is live the crop overlay OWNS the surface, but
            // it no longer shows BARE media. It wraps a DISPLAY-ONLY annotation canvas (DRAGON-387)
            // so the SAME composited scene the normal view builds is visible while the crop rect is
            // repositioned over the whole picture: the committed arrows/boxes/text/badges/pen + the
            // text-caption rasters, over the media stack (base + dim/spotlight + highlight/pixelate/
            // blur + covermark, which already ride the wrapped ZoomPan). The crop overlay then dims
            // outside the rect, draws the rule-of-thirds grid + handles in the accent, and maps
            // every drag to image SOURCE px on top of that. During a session `view_crop()` is None,
            // so `canvas_source`/`canvas_offset` are the whole frame + `(0, 0)` — the annotations
            // map exactly as the pre-crop normal view. The display canvas forwards every pointer
            // event to the ZoomPan (the crop overlay owns the pointer), so zoom/pan keep working.
            let composited = self.annotation_display_canvas(
                preview,
                image,
                content_px,
                canvas_source,
                canvas_offset,
                &window_keys,
            );
            let accent = crate::app::theme::accent(&cosmic::theme::active());
            let source = (preview.edit.frame.0 as f32, preview.edit.frame.1 as f32);
            let r = session.rect;
            crate::widgets::crop_canvas::CropCanvas::new(
                composited,
                preview.view.zoom,
                preview.view.pan,
                content_px,
                source,
                (r.x, r.y, r.w, r.h),
                accent,
                move |ev| {
                    use crate::widgets::crop_canvas::CropEvent;
                    Msg::Preview(pid, match ev {
                        CropEvent::DragBegin(h, x, y) => PreviewMsg::CropDragBegin(h, x, y),
                        CropEvent::DragTo(x, y, s) => PreviewMsg::CropDragTo(x, y, s),
                        CropEvent::DragEnd => PreviewMsg::CropDragEnd,
                    })
                },
            )
            .into()
        } else if content_px.0 > 0.0 && preview.edit.frame.0 > 0 {
            // The in-flight eraser's marked groups draw at half opacity (DRAGON-338) — the
            // preview of what releasing the button deletes.
            let items = annotate::widget_items(
                &preview.edit.annotations,
                // The curve radius is a POINT preset; the vector geometry is SOURCE px, so scale
                // it to this document's backing scale (DRAGON-383). Identity on an unscaled (1x) output.
                annotate::points_to_source_px(preview.edit.curve_radius(), preview.source_scale),
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
                canvas_source,
                preview.edit.pan_active(),
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
            // DRAGON-385: model geometry is FULL-source; when a crop frames the view the canvas
            // maps through the crop origin (`canvas_source` is then the crop's size). `(0, 0)`
            // un-cropped, so this is a no-op there.
            .crop_offset(canvas_offset)
            // DRAGON-397: the LIVE rubber-band preview reads the SAME rule the release path
            // commits with — `items_in_band`, handed in as a closure over this document's
            // annotations — so the boxes that light up as the band sweeps can never disagree with
            // what release actually selects (arrows by their shaft, pen groups by their strokes,
            // everything else by its drawn bounds). The canvas keeps the result in its own widget
            // state and only draws it: no message per motion event, so nothing here re-rasters.
            .band_hits(|x0, y0, x1, y1| {
                annotate::band_hit_ids(&preview.edit.annotations, x0, y0, x1, y1)
                    .into_iter()
                    .map(|id| id.0)
                    .collect()
            })
            .text_caret(text_caret)
            // The un-blinked caret drives the OS IME cursor area while editing (DRAGON-359).
            .ime_caret(ime_caret)
            // The id being edited + its text-selection rects (DRAGON-356 in-box shift guard +
            // DRAGON-354 item 12 drag-select). `editing_text` is set for the WHOLE edit, unlike
            // the blink-gated caret above.
            .text_editing(editing_text, text_selection)
            // The TEXT rasters (DRAGON-373): one passive, draw-only layer per text annotation,
            // handed to the canvas so it can draw each at its OWN place in the item order — which
            // is what makes a rectangle brought over one caption and under another render on screen
            // the way `rasterize_scene` bakes it. They are elements, not widgets in the tree: the
            // canvas never routes an event to them, so hit-testing stays with its own model. Built
            // by the shared helper so the crop-session display canvas (DRAGON-387) composes the
            // identical layers without duplicating the assembly.
            .text_layers(self.preview_text_layers(preview, &window_keys, canvas_offset, canvas_source, false))
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
        // Right: the document-info block (resolution over filesize), then the zoom scale
        // (Fit/%/presets). DRAGON-392: the crop control moved UP into the tray's Select/Hand/Crop
        // group and the pointer/pan pair went with the pan mode it toggled (panning is the Hand
        // TOOL now), so the info block sits immediately LEFT of the scale group — the same
        // builder and the same slot the VIDEO editor uses on its own bottom bar.
        let mut right: Vec<Element<'a, Msg>> = Vec::new();
        right.extend(tb.info_chip(Some(preview.display_frame()), preview.size));
        right.push(self.zoom_control(preview, tb));
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
            toasts,
        )
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
    /// `covermark` says whether the covermark layer belongs in this stack (DRAGON-391): `true` for
    /// the ordinary un-cropped mount, `false` when a crop frames the view, which moves it out to a
    /// sibling filling the crop window (see the caller) — this stack is laid out at the SOURCE's
    /// size, and once cropped the image the mark must cover is the crop rect instead.
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
        covermark: bool,
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
                        note_pixelate_cap();
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
        // `view_dim`, not the model's `dim`: a live crop session renders the picture undimmed
        // (DRAGON-410) without touching the document. Byte-identical outside a session.
        let dim = preview.edit.view_dim();
        let mut knockouts: Vec<[f32; 4]> = annotate::knockout_rects(&preview.edit.annotations)
            .into_iter()
            .map(|r| [r.x, r.y, r.w, r.h])
            .collect();
        if knockouts.len() > crate::widgets::annotation_fx::MAX_KNOCKOUTS {
            warn_knockout_cap(knockouts.len());
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
                        // POINT curve preset → SOURCE px, matching the source-px effect geometry
                        // (DRAGON-383). Identity on an unscaled (1x) output.
                        annotate::points_to_source_px(preview.edit.curve_radius(), preview.source_scale),
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
        // `covermark` is false when a crop frames the view — the mark then rides OVER the crop
        // window instead (DRAGON-391), since the image it spans is the crop rect, not this stack's
        // source — or when a crop SESSION is live, where it is not drawn at all (DRAGON-402;
        // `covermark_layer` enforces that second rule for every mount).
        let cm_layers: Vec<Layer> = preview
            .edit
            .covermark_layer()
            .filter(|_| covermark)
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
        // primitive type (its own per-window state), stacked on top.
        //
        // THE COST (DRAGON-395): that puts the effects OVER the covermark here, and UNDER it
        // on Linux and macOS. A real z-order deviation, accepted because the alternative was
        // a covermark that did not render at all. It is documented as a known platform
        // difference in `docs/ARCHITECTURE.md`.
        //
        // TO A/B IT: set `CCK_TEST_UNFOLD_COVERMARK=1` (see `layers::unfold_covermark`) and
        // this arm is skipped, so the overlay takes the portable path below — exactly what
        // Linux and macOS compile. Open the same capture with a covermark AND an effect on
        // both settings and compare. If the unfolded rendering is correct, the fold and this
        // comment can go; if the base or the covermark comes back blank, DRAGON-235 stands.
        // This used to say the arm could not be verified because Linux builds cannot compile
        // it — that is no longer the reason it is unverified: DRAGON-427 made this arm
        // Windows-11-only, and a Windows 11 machine can now run both sides.
        //
        // Note the text layers are NOT folded in here on either platform: they are
        // canvas-drawn (DRAGON-373), which keeps this arm's z-order exactly as it was — text
        // has always ridden above base, effects and covermark alike.
        // DRAGON-391: with a crop applied `cm_layers` is empty here on EVERY platform (the
        // covermark rides over the crop window instead), so this arm then folds the base alone —
        // the covermark still draws, through its own `LayerStack` one level up.
        #[cfg(windows)]
        let (base_element, cm_folded): (Element<'static, Msg>, bool) = if !preview
            .surface
            .is_window()
            && !super::layers::unfold_covermark()
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
        if !cm_folded
            && !cm_layers.is_empty()
            && let Some(cm) = self.covermark_stack(preview, window_keys, dw, dh)
        {
            children.push(cm);
        }
        if children.len() == 1 {
            return children.pop().expect("one child");
        }
        cosmic::iced::widget::stack(children).into()
    }

    /// The covermark's own persistent-texture `LayerStack` element, filling `w`×`h` — `None` when
    /// no covermark raster exists.
    ///
    /// ONE builder for both mounts (DRAGON-391): inside the media stack when un-cropped, and
    /// filling the crop window when a crop is applied. Both are the element that spans THE IMAGE
    /// the mark covers, so the layer is `full` in either case — the raster was rendered for exactly
    /// that canvas (see `EditState::raster_frame`), which is what keeps live and bake identical.
    /// Reads through `covermark_layer`, so neither mount can draw inside a crop session
    /// (DRAGON-402) — the crop-window mount cannot arise there anyway, but the rule lives at the
    /// read rather than depending on that.
    fn covermark_stack(
        &self,
        preview: &PreviewState,
        window_keys: &[LayerKey],
        w: f32,
        h: f32,
    ) -> Option<Element<'static, Msg>> {
        let cm = preview.edit.covermark_layer()?;
        let shader = cosmic::iced::widget::shader::Shader::new(LayerStack::part(
            vec![Layer::full(LayerKey::covermark(preview.window), cm.clone())],
            self.live_preview_windows(),
            window_keys.to_vec(),
        ))
        .width(Length::Fixed(w))
        .height(Length::Fixed(h));
        Some(widget::container(Element::new(shader)).center_x(Length::Fill).into())
    }

    /// The TEXT annotation raster layers (DRAGON-373) for the loaded-image view, as
    /// `(item id, draw-only element, the caption's SOURCE-px region)` — one persistent-texture
    /// [`LayerStack`] per caption. Factored out so the interactive editor canvas and the
    /// crop-session DISPLAY canvas (DRAGON-387) build the identical layers from ONE place — the
    /// stack assembly is never duplicated. See
    /// [`crate::widgets::annotation_canvas::AnnotationCanvas::text_layers`].
    ///
    /// Two PLACEMENT conventions, picked by `per_region` — they put the caption in the same place
    /// on screen (pinned by `annotation_canvas`'s
    /// `text_region_placement_matches_the_picture_fraction_form`), but they bound it differently:
    ///
    /// * `false` — the historical form (DRAGON-362/385): the layer is stretched across the whole
    ///   PICTURE with [`super::edit::TextLayerGeom::dest_in`] fractions locating the caption inside
    ///   it. A shader is clipped to its own widget rect, so this also bounds every caption BY the
    ///   picture — right for the editor, where the bake cuts at exactly that edge.
    /// * `true` — DRAGON-396, the crop SESSION: the layer fills a rect of its OWN, which the canvas
    ///   maps from the returned region. That is what lets a caption outside the image be drawn at
    ///   all, so the user can see what a wider crop would take back in.
    ///
    /// The region rides along either way; only the session reads it.
    fn preview_text_layers<'a>(
        &self,
        preview: &'a PreviewState,
        window_keys: &[LayerKey],
        canvas_offset: (f32, f32),
        canvas_source: (f32, f32),
        per_region: bool,
    ) -> Vec<crate::widgets::annotation_canvas::TextLayerMount<'a, Msg>> {
        preview
            .edit
            .text_layers
            .iter()
            .map(|l| {
                let key = LayerKey::text(preview.window, l.id.0);
                // The layer covers only its own caption REGION (DRAGON-362), so it is PLACED rather
                // than stretched (see `layers::Layer::dest`). Its fractions are of the DISPLAY frame
                // (DRAGON-385): a caption inside the crop lands right, one outside is clipped away;
                // `canvas_offset == (0, 0)` + `canvas_source == frame` un-cropped equals the old
                // `dest(frame)`. Per-region, the canvas does that placement instead (see above).
                let layer = if per_region {
                    Layer::full(key, l.frame.clone())
                } else {
                    Layer::at(key, l.frame.clone(), l.geom.dest_in(canvas_offset, canvas_source))
                };
                let stack = LayerStack::part(
                    vec![layer],
                    self.live_preview_windows(),
                    window_keys.to_vec(),
                );
                let r = l.geom.region;
                (
                    l.id.0,
                    Element::new(cosmic::iced::widget::shader::Shader::new(stack)),
                    (r.x, r.y, r.w, r.h),
                )
            })
            .collect()
    }

    /// A DISPLAY-ONLY annotation canvas (DRAGON-387) wrapping `content` (the preview's ZoomPan): it
    /// draws the committed vector annotations + each caption's raster layer over the media, but
    /// intercepts no pointer event and draws no selection chrome. Used by the crop SESSION, which
    /// owns the pointer through its own overlay yet must still show the annotations composited over
    /// the media. It shares the `items` + `text_layers` assembly with the interactive canvas in
    /// [`Self::image_loaded_view`] (via [`Self::preview_text_layers`]), so the composited scene is
    /// built the same way in both — no duplicated stack logic.
    fn annotation_display_canvas<'a>(
        &self,
        preview: &'a PreviewState,
        content: impl Into<Element<'a, Msg>>,
        content_px: (f32, f32),
        canvas_source: (f32, f32),
        canvas_offset: (f32, f32),
        window_keys: &[LayerKey],
    ) -> Element<'a, Msg> {
        let pid = preview.window;
        let items = annotate::widget_items(
            &preview.edit.annotations,
            annotate::points_to_source_px(preview.edit.curve_radius(), preview.source_scale),
            &preview.edit.erase_marks,
        );
        let accent = crate::app::theme::accent(&cosmic::theme::active());
        crate::widgets::annotation_canvas::AnnotationCanvas::new(
            content,
            items,
            // No selection and a neutral tool: a display-only canvas draws no chrome and never draws.
            Vec::new(),
            None,
            preview.view.zoom,
            preview.view.pan,
            content_px,
            canvas_source,
            preview.edit.pan_active(),
            accent,
            // Dead closure: `display_only(true)` forwards every event to the ZoomPan and never
            // emits, so this can never fire. Map to an inert message purely so the type checks.
            move |_| Msg::Preview(pid, PreviewMsg::AnnotMenuClose),
        )
        .display_only(true)
        // DRAGON-396: this canvas IS the crop session's, and there the marks must show OUTSIDE the
        // current image — the user is deciding what to crop back in ("so that we can easily recrop
        // later"). The crop overlay's own scrim, drawn above, is what distinguishes in-crop from
        // out-of-crop; the marks themselves render at full strength either side of that line.
        .marks_outside_image(true)
        .crop_offset(canvas_offset)
        // `true`: the session places each caption at its own rect, so one lying outside the image
        // is drawn instead of being bounded by the picture (DRAGON-396).
        .text_layers(self.preview_text_layers(preview, window_keys, canvas_offset, canvas_source, true))
        .into()
    }
}
