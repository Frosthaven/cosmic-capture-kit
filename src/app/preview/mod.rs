//! The post-capture preview overlay (shared shell).
//!
//! When "show preview after capture" is on, a finished capture opens a fullscreen
//! `Layer::Overlay` on the capture's monitor instead of being saved+shared+exited
//! immediately, so the user can review it and choose an action. The overlay is shared
//! by two media kinds; the divergent parts live in submodules:
//!
//! * [`image`] — a still screenshot ([`ImagePreview`]): decode + show at native size.
//! * [`video`] — a recording ([`VideoPreview`]): a first-frame poster + Play, and the
//!   eventual home of the in-overlay micro editor (audio/video timelines, segment
//!   select/cut/delete/undo, crossfade-vs-hard-cut toggle).
//!
//! This module owns everything common to both: the dimmed overlay shell, the surface
//! lifecycle, the loading spinner + status copy, the capture-toolbar look, and the
//! media-agnostic Save / Save As / Copy / Cancel actions (every action then exits, as
//! the tool is one-shot — the capture is auto-saved, so Cancel deletes it).

use super::*;
use std::path::PathBuf;

mod chrome;
mod covermark;
mod edit;
mod image;
mod layers;
mod open;
mod playback;
mod share;
mod sizing;
mod surface;
mod timeline;
mod video;
mod viewport;

pub use image::ImagePreview;
pub use video::VideoPreview;
pub use layers::PixelFrame;
pub use edit::covermark_dir;

use edit::{Covermark, CovermarkKind, EditState, Picker, ShareIntent};
// The split-out halves of this module (DRAGON-115), glob-imported back so the
// `use super::*;` at the top of every sibling keeps resolving the same names.
use chrome::*;
use surface::*;
use viewport::*;


/// Playful "developing your shot" lines shown under the spinner while a still capture
/// is grabbed/encoded, mirroring the window-enumeration loading copy. One is picked at
/// random when the preview opens. (Recordings use [`video::PREVIEW_VIDEO_LOADING_MESSAGES`].)
pub(super) const PREVIEW_LOADING_MESSAGES: [&str; 20] = [
    "Developing your shot",
    "Polishing the pixels",
    "Bringing it into focus",
    "Warming up the preview",
    "Prepping your capture",
    "Dialing in the details",
    "Stitching it together",
    "Decoding the capture",
    "Getting the shot ready",
    "Tidying up the image",
    "Loading every last pixel",
    "Composing the preview",
    "Rendering your capture",
    "Pulling up your shot",
    "Setting up the preview",
    "Fetching your masterpiece",
    "Sharpening things up",
    "Putting it on the canvas",
    "Almost ready to show",
    "Lining up the pixels",
];

/// The lowercased extension of `path`, if any.
fn ext_of(path: &std::path::Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Whether `path`'s extension is a video container we preview as a recording.
pub(super) fn is_video_path(path: &std::path::Path) -> bool {
    matches!(
        ext_of(path).as_deref(),
        Some("mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v")
    )
}

/// Whether `path`'s extension is a still image we can decode.
pub(super) fn is_image_path(path: &std::path::Path) -> bool {
    matches!(
        ext_of(path).as_deref(),
        Some(
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "avif" | "qoi"
                | "ico"
        )
    )
}

/// A pseudo-random index into a 20-entry loading-message array, chosen per preview open
/// (same approach as the window-loading message).
fn random_loading_msg() -> usize {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0))
        % PREVIEW_LOADING_MESSAGES.len()
}


/// State for the open preview overlay — the parts common to both media kinds. The
/// media-specific payload (and its future editor state) lives in [`PreviewKind`].
pub struct PreviewState {
    /// The preview overlay surface.
    pub window: window::Id,
    /// Which kind of surface `window` actually is (window vs. fullscreen overlay) —
    /// recorded at open time; see [`PreviewSurface`].
    pub surface: PreviewSurface,
    /// A windowed preview opens with a transient `max_size` hint (it disarms
    /// cosmic-comp's per-axis 2/3 reshape of new floating toplevels); `true` until
    /// the window's first configure clears the hint so resizing stays free.
    pub max_hint_pending: bool,
    /// The capture's ON-SCREEN footprint (physical px), when known at open. For
    /// RECORDINGS this is what the preview sizes to — the recording is shown at
    /// the size it occupied on screen, with a resolution-capped encode upscaled
    /// back into that box (`contain_dims`) rather than the preview shrinking to
    /// the encode. For STILLS it's only the pre-decode estimate; the decoded
    /// dims (`edit.frame`) take over once known (stills display 1:1, never
    /// upscaled). `None` for external files (`--preview`) — no capture footprint.
    pub display_dims: Option<(u32, u32)>,
    /// The capture's on-disk path once saved — `None` while the grab/finalize is still
    /// running (the overlay is open showing a spinner before the file exists).
    pub path: Option<PathBuf>,
    /// The saved file's size in bytes (shown as a chip), once known.
    pub size: Option<u64>,
    /// `true` when previewing a pre-existing file (`--preview`) rather than a fresh
    /// capture: the file isn't ours to manage, so Save is hidden, Cancel doesn't delete,
    /// and Save As copies instead of moving.
    pub external: bool,
    /// The monitor's logical size — scales the content within the space above the toolbar.
    pub monitor: (u32, u32),
    /// The SOURCE display's point→pixel backing scale (2.0 on a Retina panel). The
    /// capture arrives in PHYSICAL pixels; dividing by this yields the LOGICAL points
    /// the picture occupied on screen, which is what the WINDOW preview opens at (so a
    /// Retina grab isn't shown 2× too large). Always `1.0` on Linux — the compositor's
    /// screencopy already lands logical-sized — so the open-fit math there is unchanged.
    pub source_scale: f32,
    /// Index into the kind's loading-message array (picked at open time).
    pub loading_msg: usize,
    /// The media-specific payload: a still image, or a video (poster + future editor).
    pub kind: PreviewKind,
    /// Pending edits (covermark) with their undo history and picker.
    pub edit: EditState,
    /// Zoom/pan of the displayed image (ctrl+scroll zoom, alt+scroll/drag pan). The whole
    /// composited image transforms together, so covermarks baked into it ride along.
    pub view: Viewport,
}


/// The media-specific half of a preview. Images and videos diverge sharply (a video
/// grows into a timeline editor), so each owns its own state struct.
pub enum PreviewKind {
    /// A still screenshot.
    Image(ImagePreview),
    /// A recording — poster frame now, micro-editor later.
    Video(VideoPreview),
}

impl PreviewState {
    /// Whether the content is still being prepared (decode / finalize + poster
    /// extraction) — drives the spinner and its tick subscription.
    pub fn is_loading(&self) -> bool {
        match &self.kind {
            PreviewKind::Image(i) => i.image.is_none(),
            PreviewKind::Video(v) => !v.extracted,
        }
    }

    /// Whether ANY pending edit needs a bake before sharing: a covermark
    /// (either media kind) or deleted timeline segments (video). Every
    /// bake/overwrite gate reads this, not `edit.dirty()` alone.
    pub fn dirty(&self) -> bool {
        self.edit.dirty() || self.timeline_edited()
    }

    /// Whether the video timeline has content DELETED (razor cuts alone leave
    /// the output identical, so they don't count).
    pub fn timeline_edited(&self) -> bool {
        match &self.kind {
            PreviewKind::Video(v) => v.timeline.as_ref().is_some_and(|t| t.edited()),
            PreviewKind::Image(_) => false,
        }
    }

    /// While playing, the frame-poll interval: ~2× the source fps so every new frame is
    /// picked up within half a frame (smooth motion, no beat/judder against a fixed
    /// timer), clamped to a sane range. `None` when not playing.
    pub fn playback_poll(&self) -> Option<std::time::Duration> {
        match &self.kind {
            PreviewKind::Video(v) if v.is_playing() => {
                let fps = v.fps().clamp(1.0, 120.0);
                let ms = (1000.0 / (fps * 2.0)).clamp(8.0, 33.0);
                Some(std::time::Duration::from_millis(ms as u64))
            }
            _ => None,
        }
    }

    /// The media size every SIZING path uses (the windowed open fit, the overlay's
    /// hugging viewport, the poster re-fit). VIDEOS size to their captured ON-SCREEN
    /// footprint (`display_dims`) — a res-capped encode upscales back into that box
    /// for display — falling back to the probed dims for external files. STILLS
    /// size to their decoded pixels (they display 1:1, never upscaled), with the
    /// pre-decode footprint standing in until the decode lands. `(0, 0)` when
    /// nothing is known yet (spinner) — callers fall back to their size-unknown
    /// behavior.
    fn sizing_media(&self) -> (u32, u32) {
        let known = |d: (u32, u32)| (d.0 > 0 && d.1 > 0).then_some(d);
        match &self.kind {
            PreviewKind::Video(_) => self
                .display_dims
                .and_then(known)
                .or_else(|| known(self.edit.frame))
                .unwrap_or((0, 0)),
            PreviewKind::Image(_) => known(self.edit.frame)
                .or_else(|| self.display_dims.and_then(known))
                .unwrap_or((0, 0)),
        }
    }

    /// [`Self::sizing_media`] converted from PHYSICAL pixels into the LOGICAL points
    /// the picture occupied on its SOURCE display — what the WINDOW preview opens to
    /// and re-fits against, so a high-DPI capture is shown at its true on-screen size
    /// rather than 2× (`source_scale` is the source display's backing scale). On Linux
    /// `source_scale` is always `1.0`, so this returns the physical dims unchanged and
    /// the open-fit math stays byte-identical.
    pub(super) fn sizing_media_points(&self) -> (u32, u32) {
        sizing::to_points(self.sizing_media(), self.source_scale)
    }

    /// The decoded frame (`edit.frame`, PHYSICAL capture pixels) in LOGICAL points —
    /// the media's true on-screen size. The DISPLAY fit caps at this (rule 2): a hidpi
    /// capture is never drawn larger than its natural size, even in a floored window
    /// whose canvas is bigger than the picture. `source_scale == 1.0` (all Linux 1x)
    /// returns the physical dims unchanged, so the fit is byte-identical there.
    pub(super) fn frame_points(&self) -> (u32, u32) {
        sizing::to_points(self.edit.frame, self.source_scale)
    }
}


impl App {


    pub(super) fn update_preview(&mut self, message: PreviewMsg) -> Task<cosmic::Action<Msg>> {
        // A bake is committing the edits to disk: hold every input except its own
        // completion so the file can't be shared/deleted mid-rewrite.
        if self.preview.as_ref().is_some_and(|p| p.edit.baking)
            && !matches!(message, PreviewMsg::BakeDone(_))
        {
            return Task::none();
        }
        match message {
            PreviewMsg::ImageReady(handle, original) => {
                if let Some(PreviewState { kind: PreviewKind::Image(img), edit, .. }) =
                    &mut self.preview
                {
                    if let Some(o) = &original {
                        // Aspect for the covermark preview raster (stacked over the image).
                        edit.frame = o.dimensions();
                    }
                    img.image = Some(handle);
                    img.original = original;
                }
                // Launch default is "Fit to screen" (the whole picture in view). When the shot
                // fits at native size, Fit and 100% are identical — so we keep the "Fit to
                // screen" label rather than relabelling it 100%.
                //
                // Re-fit the WINDOW to the decoded picture (DRAGON-221 follow-up): the
                // window opened sized to the SELECTION dims, but a composed window capture
                // gains padding/shadow/wallpaper margins, so its size and aspect can
                // differ — the stale width then reads as side gutters once the 80%
                // height cap bites. Same drift-gated resize the video-meta path uses
                // (post-open resizes are honored once the DRAGON-108 hint cleared on the
                // first configure).
                let refit = match self.preview.as_ref() {
                    Some(p) if p.surface.is_window() => {
                        let out = self.preview_output.as_ref().map(|(_, o)| *o);
                        // Logical (backing-scale-divided) size, so a hidpi capture
                        // re-fits to its true on-screen size (rule 6).
                        let target = p.sizing_media_points();
                        let want =
                            windowed_fit_size(target, out, transport_h_for(&p.kind, p.surface));
                        // Only when meaningfully off — the open-time size is composed-
                        // dims-exact on the deferred-swap path, so this is a belt for
                        // the hidpi/external cases (note: window::resize is dropped on
                        // COSMIC for these windows — the deferred swap is the real fix).
                        ((want.0 - p.monitor.0 as f32).abs() > 2.0
                            || (want.1 - p.monitor.1 as f32).abs() > 2.0)
                            .then(|| {
                                window::resize(
                                    p.window,
                                    cosmic::iced::Size::new(want.0, want.1),
                                )
                            })
                    }
                    _ => None,
                };
                // Keep the window focused as the spinner gives way to the image (the surface
                // teardown behind the load could otherwise steal focus).
                Task::batch([refit.unwrap_or_else(Task::none), self.focus_preview_window()])
            }
            PreviewMsg::Covermark => {
                let text = self.covermark_text.clone();
                if let Some(p) = &mut self.preview {
                    p.edit.picker = match p.edit.picker {
                        Some(_) => None,
                        None => {
                            // A leading "None" card (disable) then the real covermarks.
                            let mut entries = vec![None];
                            entries.extend(edit::covermark_entries(&text).into_iter().map(Some));
                            Some(Picker { entries, selected: 0 })
                        }
                    };
                }
                Task::none()
            }
            PreviewMsg::PickerNav(delta) => {
                if let Some(PreviewState { edit: EditState { picker: Some(pk), .. }, .. }) =
                    &mut self.preview
                    && !pk.entries.is_empty()
                {
                    let len = pk.entries.len() as i32;
                    pk.selected = ((pk.selected as i32 + delta).rem_euclid(len)) as usize;
                }
                Task::none()
            }
            PreviewMsg::PickerApply => {
                let idx = match &self.preview {
                    Some(PreviewState { edit: EditState { picker: Some(pk), .. }, .. }) => {
                        pk.selected
                    }
                    _ => return Task::none(),
                };
                self.update_preview(PreviewMsg::PickerPick(idx))
            }
            PreviewMsg::PickerPick(idx) => {
                // Each covermark choice is a toggle: picking the one already applied
                // (or the "None" card) turns it OFF; picking a different one replaces it.
                // A covermark starts at THAT option's remembered zoom + opacity (falling
                // back to the global last-used values the first time it's picked).
                let (entry, active_kind) = match &self.preview {
                    Some(p) => match &p.edit.picker {
                        Some(pk) => (
                            pk.entries.get(idx).cloned(),
                            p.edit.covermark.as_ref().map(|c| c.kind.clone()),
                        ),
                        None => return Task::none(),
                    },
                    None => return Task::none(),
                };
                let Some(entry) = entry else { return Task::none() };
                let next = match entry {
                    // "None" card, or re-picking the active mark: disable.
                    None => None,
                    Some(kind) if active_kind.as_ref() == Some(&kind) => None,
                    Some(kind) => {
                        let (zoom, opacity) = self
                            .covermark_prefs
                            .get(&kind.pref_key())
                            .copied()
                            .unwrap_or((self.covermark_zoom, self.covermark_opacity));
                        Some(Covermark { kind, zoom, opacity })
                    }
                };
                if let Some(p) = &mut self.preview {
                    p.edit.picker = None;
                    p.edit.set_covermark(next);
                }
                self.refresh_edit_display()
            }
            PreviewMsg::PickerClose => {
                if let Some(p) = &mut self.preview {
                    p.edit.picker = None;
                }
                Task::none()
            }
            #[cfg(target_os = "macos")]
            PreviewMsg::PinchPoll => {
                // Ensure the gesture recognizer is on the preview window (idempotent;
                // cheap once installed), then drain the accumulated pinch magnification
                // and apply it as a zoom toward the viewport centre. The recognizer
                // posts cumulative-gesture deltas; `1 + 0.12*step` (the Zoom handler)
                // matches the pinch 1:1 at `step = delta / 0.12`. No pinch → no-op.
                crate::platform::mac::pinch::install_pinch();
                let delta = crate::platform::mac::pinch::take_pinch();
                if delta != 0.0 && self.preview.is_some() {
                    return self.update_preview(PreviewMsg::Zoom(delta / 0.12, 0.0, 0.0));
                }
                Task::none()
            }
            PreviewMsg::Zoom(step, ux, uy) => {
                // Zoom toward the cursor (keep the point under it fixed), then edge-snap.
                let maxz = self.preview.as_ref().map(|p| self.max_view_zoom(p)).unwrap_or(Viewport::MAX);
                if let Some(p) = &mut self.preview {
                    let z0 = p.view.zoom;
                    let pan0 = p.view.pan;
                    let z1 = (z0 * (1.0 + 0.12 * step)).clamp(Viewport::MIN, maxz);
                    let ratio = z1 / z0;
                    p.view.zoom = z1;
                    p.view.pan = if z1 <= Viewport::FIT {
                        (0.0, 0.0)
                    } else {
                        (ux * (1.0 - ratio) + pan0.0 * ratio, uy * (1.0 - ratio) + pan0.1 * ratio)
                    };
                    p.view.zoom_preset = None;
                }
                // Edge-snap the pan to the (new-zoom) bounds so it can't go out of view.
                let b = self.preview.as_ref().map(|p| self.preview_pan_bounds(p));
                if let Some(((minx, maxx), (miny, maxy))) = b
                    && let Some(p) = &mut self.preview
                {
                    p.view.pan.0 = p.view.pan.0.clamp(minx, maxx);
                    p.view.pan.1 = p.view.pan.1.clamp(miny, maxy);
                }
                Task::none()
            }
            PreviewMsg::SetViewZoom(z) => {
                let maxz = self.preview.as_ref().map(|p| self.max_view_zoom(p)).unwrap_or(Viewport::MAX);
                if let Some(p) = &mut self.preview {
                    p.view.set_zoom(z.min(maxz));
                    p.view.zoom_preset = None;
                }
                Task::none()
            }
            PreviewMsg::ZoomPreset(i) => {
                // Presets are in VISUAL terms (100% = natural on-screen size); convert to the
                // viewport's fit-relative multiplier via the current visual scale. So "100%"
                // targets natural size on a 2× capture (zoom = 1/visual_scale = fit at natural),
                // and physical 1:1 lives at the "200%" preset there. "Fit to screen" (None) =
                // fit BOTH (whole picture between the toolbars, zoom 1.0) — never overflow.
                let visual = self
                    .preview
                    .as_ref()
                    .map(|p| self.preview_visual_scale(p))
                    .unwrap_or(1.0);
                if let Some(p) = &mut self.preview {
                    p.view.zoom_menu_open = false;
                    // Only real preset indices change the zoom (the combo also lists the
                    // current % as a synthetic trailing entry — selecting it is a no-op).
                    if let Some(visual_frac) = ZOOM_PRESET_VISUAL.get(i).copied() {
                        // visual fraction (1.0 = 100% = natural size): displayed =
                        // zoom*visual_scale → zoom = frac/visual_scale.
                        p.view.set_zoom(viewport::preset_zoom(visual_frac, visual));
                        p.view.zoom_preset = Some(i);
                    }
                }
                Task::none()
            }
            PreviewMsg::ToggleZoomMenu => {
                if let Some(p) = &mut self.preview {
                    p.view.zoom_menu_open = !p.view.zoom_menu_open;
                }
                Task::none()
            }
            PreviewMsg::Pan(dx, dy) => {
                // Clamp panning to the image's overflow beyond the (scrollbar-reserved)
                // viewport, so you can't scroll past the picture's edges.
                let bounds = self.preview.as_ref().map(|p| self.preview_pan_bounds(p));
                if let Some(((minx, maxx), (miny, maxy))) = bounds
                    && let Some(p) = &mut self.preview
                {
                    p.view.pan.0 = (p.view.pan.0 + dx).clamp(minx, maxx);
                    p.view.pan.1 = (p.view.pan.1 + dy).clamp(miny, maxy);
                }
                Task::none()
            }
            PreviewMsg::SetPanMode(on) => {
                if let Some(p) = &mut self.preview {
                    p.view.pan_mode = on;
                }
                Task::none()
            }
            PreviewMsg::SetZoom(zoom) => {
                // Live slider drag: update the value (so the thumb + eventual bake track it)
                // but DON'T re-raster yet — that happens once on release (CommitCovermarkEdit),
                // so dragging doesn't churn the GPU texture.
                if let Some(p) = &mut self.preview
                    && p.edit.covermark.is_some()
                {
                    p.edit.set_zoom(zoom);
                    self.remember_covermark_pref();
                    self.save_state();
                }
                Task::none()
            }
            PreviewMsg::SetOpacity(opacity) => {
                // Live slider drag: value only; re-raster on release (see SetZoom).
                if let Some(p) = &mut self.preview
                    && let Some(cm) = &mut p.edit.covermark
                {
                    cm.opacity = opacity;
                    p.edit.cm_raster.invalidate();
                    self.remember_covermark_pref();
                    self.save_state();
                }
                Task::none()
            }
            PreviewMsg::CommitCovermarkEdit => self.refresh_edit_display(),
            PreviewMsg::Undo => {
                // The shared history walks covermark AND timeline edits; only a
                // covermark change needs the async raster refresh (a timeline
                // change redraws on the next view for free).
                if let Some(PreviewState { kind, edit, .. }) = &mut self.preview {
                    let tl = match kind {
                        PreviewKind::Video(vid) => vid.timeline.as_mut(),
                        PreviewKind::Image(_) => None,
                    };
                    if edit.undo(tl) {
                        return self.refresh_edit_display();
                    }
                }
                Task::none()
            }
            PreviewMsg::Redo => {
                if let Some(PreviewState { kind, edit, .. }) = &mut self.preview {
                    let tl = match kind {
                        PreviewKind::Video(vid) => vid.timeline.as_mut(),
                        PreviewKind::Image(_) => None,
                    };
                    if edit.redo(tl) {
                        return self.refresh_edit_display();
                    }
                }
                Task::none()
            }
            PreviewMsg::CovermarkRasterReady(generation, frame) => {
                // The covermark overlay raster (stacked over the base image/video via the
                // persistent-texture shader) — NOT a full re-composite, so the base never
                // re-uploads. `finish` drops stale generations and reports whether another
                // refresh was requested while this one was in flight.
                let again = self
                    .preview
                    .as_mut()
                    .map(|p| p.edit.cm_raster.finish(generation, frame))
                    .unwrap_or(false);
                if again {
                    return self.refresh_edit_display();
                }
                Task::none()
            }
            PreviewMsg::BakeDone(size) => {
                // Captured before borrowing `p`: whether the editor stays open after the
                // share (the "auto close" setting is off).
                let keep_open = !self.auto_close_preview;
                let Some(p) = &mut self.preview else {
                    return Task::none();
                };
                p.edit.baking = false;
                let intent = p.edit.pending.take();
                let output = p.edit.pending_output.take();
                let is_video = matches!(p.kind, PreviewKind::Video(_));
                let saved_path = p.path.clone();
                match size {
                    Some(size) => {
                        self.stop_preview_playback();
                        match intent {
                            // Save baked the capture IN PLACE. When keeping the editor
                            // open, the edits are now part of the file, so commit them
                            // into the base: reset the edit state and reload the baked
                            // result as the new baseline (so further edits start clean).
                            Some(ShareIntent::Save) => {
                                if let Some(p) = &mut self.preview {
                                    p.size = Some(size);
                                    p.edit.covermark = None;
                                    p.edit.undo_stack.clear();
                                    p.edit.redo_stack.clear();
                                    // Timeline cuts are in the file now — the old
                                    // spans/probe describe a recording that no longer
                                    // exists. Drop them; the keep-open re-probe below
                                    // re-establishes meta + a fresh (uncut) timeline.
                                    if let PreviewKind::Video(vid) = &mut p.kind {
                                        vid.timeline = None;
                                        vid.waveform = None;
                                        vid.meta = None;
                                        vid.playback = None;
                                        vid.frame = None;
                                        vid.position = 0.0;
                                    }
                                }
                                if let Some(path) = &saved_path {
                                    crate::platform::services::notify(path, false);
                                }
                                if keep_open {
                                    // Reload the baked file so the display + base match
                                    // the on-disk result: images re-decode; a video
                                    // re-probes (fresh poster, duration, timeline —
                                    // its cuts/duration may have changed).
                                    match (is_video, saved_path) {
                                        (false, Some(path)) => image::decode_task(path),
                                        (true, Some(path)) => video::poster_task(path),
                                        _ => Task::none(),
                                    }
                                } else {
                                    self.finish_session()
                                }
                            }
                            // Copy baked to a TEMP (the saved file stays clean): the
                            // clipboard gets the edited temp; the notification reveals
                            // the untouched saved file. Keeping open leaves the pending
                            // edits intact (the saved file wasn't changed, so they're
                            // still "unsaved") — only clear them when we're closing.
                            Some(ShareIntent::Copy) => {
                                if let Some(temp) = &output {
                                    crate::platform::services::copy_to_clipboard(temp, is_video);
                                }
                                if let Some(path) = &saved_path {
                                    crate::platform::services::notify(path, true);
                                }
                                if keep_open {
                                    Task::none()
                                } else {
                                    if let Some(p) = &mut self.preview {
                                        p.edit.covermark = None;
                                        p.edit.undo_stack.clear();
                                        p.edit.redo_stack.clear();
                                    }
                                    self.finish_session()
                                }
                            }
                            None => Task::none(),
                        }
                    }
                    None => {
                        // Bake failed (ffmpeg / encode error). The overlay is already
                        // closed and the ORIGINAL file on disk is untouched, so finish
                        // gracefully — notify the (unedited) saved capture so it isn't
                        // lost silently.
                        log::warn!("preview edit bake failed; capture left unedited");
                        if let Some(path) = &saved_path {
                            crate::platform::services::notify(path, false);
                        }
                        self.finish_session()
                    }
                }
            }
            PreviewMsg::ConfirmOverwrite => {
                // The user OK'd overwriting the file: bake the edits into it in place
                // (background, behind the processing notification), then finish. `begin_bake`
                // uses the preview's own path, and BakeDone(Save) reveals + finishes/keeps.
                if let Some(p) = &mut self.preview {
                    p.edit.confirm_overwrite = false;
                }
                if let Some(task) = self.begin_bake(ShareIntent::Save) {
                    return task;
                }
                // No edits after all (shouldn't happen — the dialog only opens with edits):
                // nothing to write, just exit.
                self.finish_session()
            }
            PreviewMsg::CancelOverwrite => {
                if let Some(p) = &mut self.preview {
                    p.edit.confirm_overwrite = false;
                }
                Task::none()
            }
            PreviewMsg::PosterReady(poster, meta) => {
                let mut wave = Task::none();
                if let Some(PreviewState { kind: PreviewKind::Video(vid), edit, path, .. }) =
                    &mut self.preview
                {
                    vid.poster = poster;
                    vid.meta = meta;
                    vid.extracted = true;
                    if let Some(m) = meta {
                        edit.frame = (m.w, m.h);
                        // The probe fixes the duration → the timeline editor can
                        // exist. Fresh and uncut (also after a keep-open Save
                        // re-probe — the baked file IS the new baseline).
                        if m.duration > 0.0 {
                            vid.timeline = Some(timeline::Timeline::new(m.duration));
                        }
                        // Soundtrack peaks for the L/R lanes, off-thread.
                        if m.has_audio && let Some(path) = path.clone() {
                            wave = video::waveform_task(path);
                        }
                    }
                }
                // The probe just revealed the recording's true encoded dims. A windowed
                // preview whose CAPTURED footprint was never known (an external
                // `--preview` file, or a stop before the worker's first frame) opened
                // at the size-unknown fallback — re-fit it now that `sizing_media`
                // has something real (the footprint keeps precedence when present,
                // so a fitted recording never gets shrunk to a res-capped encode).
                // The overlay needs nothing: its hugging viewport re-reads live.
                let refit = match (self.preview.as_ref(), meta) {
                    (Some(p), Some(_)) if p.surface.is_window() => {
                        let out = self.preview_output.as_ref().map(|(_, o)| *o);
                        // Logical (backing-scale-divided) footprint so a Retina recording
                        // re-fits to its true on-screen size, matching the open fit.
                        let target = p.sizing_media_points();
                        let want =
                            windowed_fit_size(target, out, transport_h_for(&p.kind, p.surface));
                        // Only when meaningfully off — the open-time guess is often exact.
                        ((want.0 - p.monitor.0 as f32).abs() > 2.0
                            || (want.1 - p.monitor.1 as f32).abs() > 2.0)
                            .then(|| {
                                window::resize(
                                    p.window,
                                    cosmic::iced::Size::new(want.0, want.1),
                                )
                            })
                    }
                    _ => None,
                };
                // Keep the window focused as the spinner gives way to the poster.
                Task::batch([wave, refit.unwrap_or_else(Task::none), self.focus_preview_window()])
            }
            // Playback / scrub / frame-step / timeline edits are video-only (no-ops
            // otherwise); the logic lives in `video.rs` next to the playback state.
            PreviewMsg::Play => self.toggle_playback(),
            PreviewMsg::PlayerTick => self.playback_tick(),
            PreviewMsg::Seek(t) => self.seek(t),
            PreviewMsg::FrameStep(delta) => self.frame_step(delta),
            PreviewMsg::SeekFrameReady(handle) => self.on_seek_frame(handle),
            PreviewMsg::TimelineSeek(t) => self.timeline_seek(t),
            PreviewMsg::TimelineSelect(t, ctrl, shift) => self.timeline_select(t, ctrl, shift),
            PreviewMsg::TimelineBoxSelect(a, b, additive) => {
                self.timeline_box_select(a, b, additive)
            }
            PreviewMsg::TimelineCut(t) => self.timeline_cut(t),
            PreviewMsg::TimelineRazor(on) => self.timeline_set_razor(on),
            PreviewMsg::TimelineDelete => self.timeline_delete_selected(),
            PreviewMsg::TimelineMenuOpen(t, x, y) => self.timeline_menu_open(t, x, y),
            PreviewMsg::TimelineMenuClose => self.timeline_menu_close(),
            PreviewMsg::WaveformReady(peaks) => self.on_waveform(peaks),
            PreviewMsg::Save => {
                let external = self.preview.as_ref().is_some_and(|p| p.external);
                // Anything to bake into the file? A covermark or deleted timeline
                // segments bake new pixels. If so, Save always confirms the overwrite
                // first (no cleverness about "nothing changed") — via the in-app modal,
                // clickable over the overlay grab and in the window alike. The
                // background bake runs on ConfirmOverwrite.
                let would_write =
                    self.preview.as_ref().is_some_and(|p| p.dirty());
                if would_write {
                    if let Some(p) = &mut self.preview {
                        p.edit.confirm_overwrite = true;
                    }
                    return Task::none();
                }
                // Nothing to write: a `--preview` file is the user's and untouched; a fresh
                // capture already lives at its path. Reveal it and finish/keep.
                if !external
                    && let Some(path) = self.preview.as_ref().and_then(|p| p.path.as_ref())
                {
                    crate::platform::services::notify(path, false);
                }
                self.finish_or_keep_preview()
            }
            PreviewMsg::Copy => {
                // Pending edits bake first so the clipboard gets the edited capture.
                if let Some(task) = self.begin_bake(ShareIntent::Copy) {
                    return task;
                }
                let is_video = matches!(
                    self.preview.as_ref().map(|p| &p.kind),
                    Some(PreviewKind::Video(_))
                );
                if let Some(path) = self.preview.as_ref().and_then(|p| p.path.as_ref()) {
                    crate::platform::services::copy_to_clipboard(path, is_video);
                    crate::platform::services::notify(path, true);
                }
                self.finish_or_keep_preview()
            }
            PreviewMsg::Cancel => {
                // Close without deleting — the file stays where it is. Deleting is the
                // explicit Delete (trash) action.
                self.stop_preview_playback();
                self.finish_session()
            }
            PreviewMsg::ToggleAppearance => self.toggle_preview_appearance(),
            PreviewMsg::WindowDrag => match self.preview.as_ref() {
                Some(p) => window::drag(p.window),
                None => Task::none(),
            },
            PreviewMsg::WindowMaximize => match self.preview.as_ref() {
                Some(p) => window::toggle_maximize(p.window),
                None => Task::none(),
            },
            PreviewMsg::WindowMinimize => match self.preview.as_ref() {
                Some(p) => window::minimize(p.window, true),
                None => Task::none(),
            },
            PreviewMsg::Delete => {
                // Never delete a pre-existing `--preview` file (no trash button there).
                if self.preview.as_ref().is_some_and(|p| p.external) {
                    return Task::none();
                }
                // Explicitly delete the captured file, then close.
                self.stop_preview_playback();
                if let Some(path) = self.preview.as_ref().and_then(|p| p.path.as_ref()) {
                    let _ = std::fs::remove_file(path);
                }
                self.finish_session()
            }
            PreviewMsg::SaveAs => {
                // Ask WHERE to save first — no bake up front. The bake (if any) runs in the
                // background against the chosen destination in `SaveAsResult`, tracked by the
                // "Processing capture" notification, so the user isn't blocked before the
                // dialog.
                self.save_as_dialog()
            }
            PreviewMsg::SaveAsResult(opt) => {
                let Some(dest) = opt else {
                    // Cancelled. A window is still open (only overlays close for the dialog),
                    // so stay on it; an overlay was torn down for the dialog, so bring it
                    // BACK — the capture and its edits are still loaded, and a cancelled
                    // dialog must return the user to where they were, not exit
                    // (DRAGON-157).
                    return if self.preview.as_ref().is_some_and(|p| p.surface.is_window()) {
                        Task::none()
                    } else {
                        self.reopen_preview_surface()
                    };
                };
                // Gather everything the background worker needs, then release `self`.
                let external = self.preview.as_ref().is_some_and(|p| p.external);
                // Keep-open Save As is an EXPORT: the editor continues on its working
                // document (undo/redo intact), so the working file must SURVIVE the
                // save — copy semantics, never a move (see `SaveAsBaked`).
                let keep_open = !self.auto_close_preview;
                let (src, covermark, video, is_video) = match self.preview.as_ref() {
                    Some(p) => {
                        let Some(src) = p.path.clone() else {
                            return self.finish_session();
                        };
                        let is_video = matches!(p.kind, PreviewKind::Video(_));
                        // A video bake needs the probed metadata; without it we can only
                        // move it (share unedited). Images bake from their own pixels.
                        let video = match &p.kind {
                            PreviewKind::Image(_) => None,
                            PreviewKind::Video(vid) => vid.meta.map(|m| edit::VideoBake {
                                w: m.w,
                                h: m.h,
                                has_audio: m.has_audio,
                                keep: vid
                                    .timeline
                                    .as_ref()
                                    .filter(|t| t.edited())
                                    .map(|t| t.spans.clone()),
                            }),
                        };
                        (src, p.edit.covermark.clone(), video, is_video)
                    }
                    None => return self.finish_session(),
                };
                // Only bake when there's something to apply AND we can (video needs meta).
                let cuts = video.as_ref().is_some_and(|v| v.keep.is_some());
                let can_bake =
                    (covermark.is_some() || cuts) && (!is_video || video.is_some());
                // Export in the BACKGROUND: bake straight to the destination (behind the
                // processing notification), or plainly move/copy when nothing needs baking.
                // Await it via a task only so the app stays alive until the file lands.
                let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
                let bake_dest = dest.clone();
                std::thread::spawn(move || {
                    let dest = bake_dest;
                    let ok = if can_bake {
                        let result = crate::share::with_processing_notification(|| match &video {
                            Some(v) => edit::bake_video(&src, &dest, covermark.as_ref(), v),
                            None => edit::bake_image(&src, &dest, covermark.as_ref()),
                        });
                        // Log the real io::Error here — it's about to be discarded to a bool.
                        if let Err(e) = &result {
                            log::warn!("preview edit bake failed (Save As): {e}");
                        }
                        result.is_ok()
                    } else if external || keep_open {
                        // A pre-existing (`--preview`) file, or a keep-open export:
                        // copy, leave the original — the editor keeps working on it.
                        std::fs::copy(&src, &dest).is_ok()
                    } else if std::fs::rename(&src, &dest).is_err() {
                        // Move a fresh capture (copy + remove when rename can't cross FS).
                        std::fs::copy(&src, &dest).is_ok() && {
                            let _ = std::fs::remove_file(&src);
                            true
                        }
                    } else {
                        true
                    };
                    // A successful bake wrote the destination but left the fresh capture's
                    // original in place — remove it so Save As is a move, not a copy. But
                    // NOT when saving over the same file (dest == src): the bake wrote in
                    // place, so removing it would delete the just-saved capture.
                    let same_file = std::fs::canonicalize(&src)
                        .ok()
                        .zip(std::fs::canonicalize(&dest).ok())
                        .is_some_and(|(a, b)| a == b);
                    if ok && can_bake && !external && !same_file && !keep_open {
                        let _ = std::fs::remove_file(&src);
                    }
                    if ok {
                        crate::platform::services::notify(&dest, false);
                    }
                    let _ = tx.send(ok);
                });
                Task::perform(rx, move |res| {
                    // The reveal + write already happened on the worker; carry the dest so a
                    // keep-open session can reopen on it.
                    let done = matches!(res, Ok(true)).then(|| dest.clone());
                    cosmic::Action::App(Msg::Preview(PreviewMsg::SaveAsBaked(done)))
                })
            }
            PreviewMsg::SaveAsBaked(done) => {
                // Auto-close on → end the session (success or failure). Keep-open →
                // Save As is an EXPORT: the editor CONTINUES on its working document
                // with the covermark/timeline state and undo/redo history intact —
                // the saved copy is never re-opened. Only a fullscreen overlay needs
                // anything done (its surface was torn down for the dialog, so re-mint
                // it; a window never closed). One exception: an export aimed at the
                // working file ITSELF committed the pending edits in place, so the
                // preview reloads that file (the committed pixels) — continuing with
                // the old edit state would apply the edits twice.
                if self.auto_close_preview || self.preview.is_none() {
                    return self.finish_session();
                }
                let committed_in_place = match (
                    done.as_ref(),
                    self.preview.as_ref().and_then(|p| p.path.as_ref()),
                ) {
                    (Some(dest), Some(src)) => std::fs::canonicalize(dest)
                        .ok()
                        .zip(std::fs::canonicalize(src).ok())
                        .is_some_and(|(a, b)| a == b),
                    _ => false,
                };
                let reload = if committed_in_place {
                    let dest = done.expect("committed_in_place implies a destination");
                    let is_video = matches!(
                        self.preview.as_ref().map(|p| &p.kind),
                        Some(PreviewKind::Video(_))
                    );
                    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                    self.stop_preview_playback();
                    self.reload_preview_in_place(dest, size, is_video)
                } else {
                    Task::none()
                };
                let surface = if self.preview.as_ref().is_some_and(|p| p.surface.is_window()) {
                    Task::none()
                } else {
                    self.reopen_preview_surface()
                };
                Task::batch([reload, surface])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn ext_of_lowercases_and_handles_missing_extension() {
        assert_eq!(ext_of(std::path::Path::new("Foo.PNG")), Some("png".to_string()));
        assert_eq!(ext_of(std::path::Path::new("no_extension")), None);
    }

    #[test]
    fn video_and_image_paths_are_case_insensitive_and_mutually_exclusive() {
        for ext in ["mp4", "MKV", "WebM", "mov", "avi", "m4v"] {
            let p = std::path::PathBuf::from(format!("f.{ext}"));
            assert!(is_video_path(&p), "{ext} should be a video path");
            assert!(!is_image_path(&p), "{ext} should not be an image path");
        }
        for ext in ["png", "JPG", "jpeg", "gif", "bmp", "WebP", "tif", "tiff", "avif", "qoi", "ico"] {
            let p = std::path::PathBuf::from(format!("f.{ext}"));
            assert!(is_image_path(&p), "{ext} should be an image path");
            assert!(!is_video_path(&p), "{ext} should not be a video path");
        }
    }

    #[test]
    fn unknown_or_missing_extension_is_neither_video_nor_image() {
        let p = std::path::Path::new("no_extension");
        assert!(!is_video_path(p));
        assert!(!is_image_path(p));
    }

    /// A still preview whose decoded (physical) frame is `frame`, sourced from a
    /// display of the given backing `scale` — just enough state for the sizing math.
    fn still_at(frame: (u32, u32), scale: f32) -> PreviewState {
        let edit = EditState { frame, ..EditState::default() };
        PreviewState {
            window: window::Id::unique(),
            surface: PreviewSurface::Window,
            max_hint_pending: true,
            display_dims: None,
            path: None,
            size: None,
            external: false,
            monitor: (1920, 1080),
            source_scale: scale,
            loading_msg: 0,
            kind: PreviewKind::Image(ImagePreview::loading()),
            edit,
            view: Viewport::default(),
        }
    }

    /// The WINDOW open-fit sizes to LOGICAL points: a 2× Retina grab (physical frame)
    /// halves back to its true on-screen footprint. This is the DRAGON-130 DPI fix —
    /// the same physical pixels used to open the window 2× too large.
    #[test]
    fn sizing_media_points_divides_a_retina_capture_back_to_logical() {
        // 400×300 logical selection on a 2× display decodes to 800×600 physical.
        let p = still_at((800, 600), 2.0);
        assert_eq!(p.sizing_media(), (800, 600), "the raw media stays physical");
        assert_eq!(p.sizing_media_points(), (400, 300), "the open-fit sees logical points");
    }

    /// Scale 1.0 (every Linux capture, and non-Retina mac panels) is the IDENTITY —
    /// `sizing_media_points` returns the physical dims unchanged, so the shared
    /// `windowed_fit_size` math is byte-identical to before the fix.
    #[test]
    fn sizing_media_points_is_identity_at_scale_one() {
        for frame in [(1920u32, 1080u32), (1280, 720), (3840, 2160), (401, 299)] {
            let p = still_at(frame, 1.0);
            assert_eq!(p.sizing_media_points(), frame);
            assert_eq!(p.sizing_media_points(), p.sizing_media());
        }
    }

    /// Fractional backing scales round to the nearest logical point and never collapse
    /// a real capture to zero.
    #[test]
    fn sizing_media_points_rounds_fractional_scales_and_never_zeroes() {
        // 1.5× (a common HiDPI fractional): 900 physical → 600 logical.
        assert_eq!(still_at((900, 600), 1.5).sizing_media_points(), (600, 400));
        // A 1px capture at 2× must not floor to zero.
        assert_eq!(still_at((1, 1), 2.0).sizing_media_points(), (1, 1));
    }
}
