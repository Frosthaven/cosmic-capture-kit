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

mod annotate;
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
pub(crate) use annotate::AnnotId;

use annotate::Reorder;
use edit::{Covermark, CovermarkKind, EditKind, EditState, Picker, ShareIntent};
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
    /// Whether THIS document holds a share of the process-wide "pause other apps' media"
    /// guard (DRAGON-336 phase 2). The guard itself is refcounted on `App`
    /// ([`DuckRefs`]); this flag is the per-document half, so a close/re-mint releases
    /// exactly one reference and several video previews can't un-mute each other.
    pub ducking: bool,
    /// Whether `window` is still a LIVE surface. A few paths tear the surface down while
    /// keeping the document loaded — the background bake and the overlay's Save As dialog
    /// (an exclusive layer surface would render over the file chooser) — and the document
    /// may be closed later without ever re-minting. [`App::close_preview`] reads this so it
    /// never issues a SECOND destroy for an already-dead surface, which used to be
    /// invisible only because the process exited immediately afterwards.
    pub surface_open: bool,
    /// This document was DEMOTED out of the fullscreen overlay when a second document
    /// opened (DRAGON-336), and stays windowed for the rest of the session — even once
    /// its siblings close and it is alone again. Silently re-entering fullscreen as
    /// windows close would be jarring, so the pin is sticky; only the user's own
    /// appearance toggle clears it. Read by [`overlay_taken`]; set in
    /// [`App::demote_preview_to_window`], where the decision is documented.
    pub demoted: bool,
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


/// Refcount for the "pause other apps' media" guard across MULTIPLE open previews
/// (DRAGON-336 phase 2). The guard is a single process-wide effect, but any number of
/// video previews may want it held, so it is engaged when the FIRST holder appears and
/// dropped only when the LAST one releases — a plain `Option` un-muted the desktop the
/// moment ONE of several previews stopped playing.
///
/// Holders are named by their preview's `window::Id`, so acquire/release are idempotent
/// per preview (a repeated engage from the same preview is not a second reference, and a
/// close that releases an already-released preview is a no-op).
#[derive(Default)]
pub(crate) struct DuckRefs {
    holders: Vec<window::Id>,
}

impl DuckRefs {
    /// Register `id` as a holder. Returns `true` when this is the FIRST holder — the
    /// caller must engage the guard now.
    pub(crate) fn acquire(&mut self, id: window::Id) -> bool {
        if self.holders.contains(&id) {
            return false;
        }
        let first = self.holders.is_empty();
        self.holders.push(id);
        first
    }

    /// Drop `id`'s hold. Returns `true` when that was the LAST holder — the caller must
    /// drop the guard now (and only now).
    pub(crate) fn release(&mut self, id: window::Id) -> bool {
        let Some(i) = self.holders.iter().position(|h| *h == id) else {
            return false;
        };
        self.holders.remove(i);
        self.holders.is_empty()
    }

    /// Whether anyone still wants the desktop ducked.
    pub(crate) fn held(&self) -> bool {
        !self.holders.is_empty()
    }

    /// A holder changed surface identity (a preview re-minted its window — the appearance
    /// toggle / cover→window swap). Move the hold WITHOUT engaging or dropping the guard;
    /// a no-op when `old` wasn't holding.
    pub(crate) fn rename(&mut self, old: window::Id, new: window::Id) {
        if let Some(h) = self.holders.iter_mut().find(|h| **h == old) {
            *h = new;
        }
    }
}


/// Position of the open preview whose surface is `id`. The one lookup rule behind
/// [`App::preview_for`] / [`App::preview_for_mut`] / [`App::close_preview`], kept a free
/// fn over the slice so it is unit-testable without an `App`.
pub(super) fn index_of(previews: &[PreviewState], id: window::Id) -> Option<usize> {
    previews.iter().position(|p| p.window == id)
}

/// The "last one out turns off the lights" decision (DRAGON-336 phase 2): closing `id`
/// leaves NOTHING open, so the process must end. False when `id` isn't open at all AND
/// something else still is — closing an already-gone document must not kill live ones.
pub(super) fn closing_is_last(previews: &[PreviewState], id: window::Id) -> bool {
    match index_of(previews, id) {
        Some(_) => previews.len() == 1,
        // Already removed: only the genuinely empty collection means "we're done".
        None => previews.is_empty(),
    }
}

/// Whether the fullscreen OVERLAY is UNAVAILABLE to the preview being minted, so it must
/// open as a WINDOW instead.
///
/// THE RULE (DRAGON-336): a preview may be the fullscreen overlay only while it is the
/// ONLY document. As soon as a SECOND document exists they are ALL windows — an overlay
/// sitting behind floating preview windows is a strange mixed state, and two mapped
/// `Exclusive` layer surfaces would fight over the keyboard grab besides (the DRAGON-109
/// hazard documented at the top of `shell.rs`). So this is true whenever ANY OTHER
/// document is open, whatever surface that one happens to be on — and the sibling still
/// holding an overlay is DEMOTED to a window in the same pass
/// ([`App::demote_preview_to_window`]), so the end state is either exactly one overlay or
/// N windows, never a mix.
///
/// It is also true for a document DEMOTED earlier in the session
/// ([`PreviewState::demoted`]): that one stays windowed even once it is alone again.
///
/// `existing` is the document being RE-minted (the appearance toggle, the cover→window
/// swap, the Save-As re-open) — it never blocks itself, since its old surface is torn
/// down in the same pass. `None` = a fresh document, not yet in `previews`.
pub(super) fn overlay_taken(previews: &[PreviewState], existing: Option<window::Id>) -> bool {
    previews.iter().any(|p| match existing {
        // The document being re-minted: only its own sticky demotion bars it.
        Some(e) if p.window == e => p.demoted,
        // Any other open document bars the overlay outright.
        _ => true,
    })
}

/// The open documents that must come DOWN from the fullscreen overlay because a preview is
/// being minted as a window beside them — the other half of [`overlay_taken`]'s rule, kept
/// a free fn over the slice so the selection is unit-testable without an `App`. The
/// demotion itself is [`App::demote_preview_to_window`].
///
/// A document whose surface was torn down while it stays loaded (a background bake, the
/// overlay's Save-As dialog) is NOT selected: there is nothing on screen to demote, and
/// re-minting a window for it would resurrect a surface that was closed on purpose. It
/// re-opens later through the same rule, as a window.
///
/// `minting` is the document being minted for (never its own sibling); `None` for a fresh
/// document, which is not yet in `previews`.
pub(super) fn overlay_siblings(
    previews: &[PreviewState],
    minting: Option<window::Id>,
) -> Vec<window::Id> {
    previews
        .iter()
        .filter(|p| !p.surface.is_window() && p.surface_open && Some(p.window) != minting)
        .map(|p| p.window)
        .collect()
}


impl App {
    /// The open preview whose surface is `id`, if any. THE lookup for every
    /// `PreviewMsg` handler and window-keyed path — never index `self.previews`.
    pub(in crate::app) fn preview_for(&self, id: window::Id) -> Option<&PreviewState> {
        index_of(&self.previews, id).map(|i| &self.previews[i])
    }

    /// Mutable [`Self::preview_for`].
    pub(in crate::app) fn preview_for_mut(&mut self, id: window::Id) -> Option<&mut PreviewState> {
        index_of(&self.previews, id).map(|i| &mut self.previews[i])
    }

    /// The preview keyboard input belongs to when the event carries no usable window id
    /// (the keymap dispatch). In priority order: the framework's genuinely FOCUSED window
    /// when that is a preview, else the last preview we saw input for, else the most
    /// recently OPENED one. The final fallback is what makes this `Some` whenever ANY
    /// preview is open, keeping the single-preview behavior byte-identical to the old
    /// singular-`preview` gate.
    pub(in crate::app) fn focused_preview_id(&self) -> Option<window::Id> {
        use cosmic::Application as _;
        self.core()
            .focused_window()
            .filter(|id| self.preview_for(*id).is_some())
            .or_else(|| self.focused_preview.filter(|id| self.preview_for(*id).is_some()))
            .or_else(|| self.previews.last().map(|p| p.window))
    }

    /// Every OPEN preview surface, for the GPU shaders' closed-window eviction (DRAGON-336).
    ///
    /// iced keys a shader `Pipeline` by primitive TYPE and shares it across every window's
    /// renderer, and exposes NO external handle to that storage — so app code cannot free a
    /// closed preview's textures directly. Instead each `LayerStack` / `EffectsFx` carries
    /// this set into its `prepare`, which drops the state of any window no longer in it.
    /// It must be the OPEN set, never the drawn/focused one: a preview that has just opened
    /// and not yet rendered is open, and that is exactly what keeps another window's prepare
    /// from wiping it.
    pub(in crate::app) fn live_preview_windows(&self) -> Vec<window::Id> {
        self.previews.iter().map(|p| p.window).collect()
    }

    /// Whether the fullscreen OVERLAY is barred for the preview being minted — see
    /// [`overlay_taken`] for the rule (a preview is the overlay only while it is the ONLY
    /// document; a second document makes them ALL windows). [`Self::preview_surface_for`]
    /// consults this as the single enforcement point.
    ///
    /// `existing` is the document being RE-minted (its old surface is torn down in the
    /// same pass, so it never blocks itself).
    pub(in crate::app) fn overlay_barred(&self, existing: Option<window::Id>) -> bool {
        overlay_taken(&self.previews, existing)
    }

    /// Record which preview holds keyboard focus (an open, a focus event, or a close
    /// that promoted another document). Ignores ids that aren't previews.
    pub(in crate::app) fn note_preview_focus(&mut self, id: window::Id) {
        if self.preview_for(id).is_some() {
            self.focused_preview = Some(id);
        }
    }

    /// THE multi-document close seam (DRAGON-336 phase 2): this ONE preview is done.
    /// Destroys its surface, forgets its state, and — only when it was the LAST open
    /// preview — ends the process through [`App::finish_session`] (the unchanged
    /// one-shot lifecycle seam). With several previews open the others keep running and
    /// focus is handed to the most recent survivor.
    ///
    /// Every "THIS preview is finished" path routes through here; the paths that mean
    /// "the PROCESS is finished" (a capture error, a settings-window close, teardown)
    /// still call `finish_session` directly.
    pub(in crate::app) fn close_preview(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let Some(i) = index_of(&self.previews, id) else {
            // Already gone (a double close, or a stale async completion): if nothing is
            // left at all this is still the last-one-out case, so honor it.
            return if closing_is_last(&self.previews, id) {
                self.finish_session()
            } else {
                Task::none()
            };
        };
        // Stop this document's playback + release its share of the duck refcount BEFORE
        // it leaves the collection (both are keyed by its id).
        self.stop_preview_playback(id);
        let p = self.previews.remove(i);
        // Only destroy a surface that is still up (a bake / Save As dialog may have torn
        // it down already while keeping the document loaded).
        let close = if p.surface_open { p.surface.close(p.window) } else { Task::none() };
        if self.capture_preview == Some(id) {
            self.capture_preview = None;
        }
        if self.focused_preview == Some(id) {
            self.focused_preview = None;
        }
        if self.previews.is_empty() {
            // Last one out turns off the lights.
            return Task::batch([close, self.finish_session()]);
        }
        // Hand focus to the most recent survivor so the keymap dispatch keeps a target.
        self.focused_preview = self.previews.last().map(|p| p.window);
        close
    }


    pub(super) fn update_preview(
        &mut self,
        id: window::Id,
        message: PreviewMsg,
    ) -> Task<cosmic::Action<Msg>> {
        // A bake is committing the edits to disk: hold every input except its own
        // completion so the file can't be shared/deleted mid-rewrite.
        if self.preview_for(id).is_some_and(|p| p.edit.baking)
            && !matches!(message, PreviewMsg::BakeDone(_))
        {
            return Task::none();
        }
        match message {
            PreviewMsg::ImageReady(handle, original) => {
                if let Some(PreviewState { kind: PreviewKind::Image(img), edit, .. }) =
                    self.preview_for_mut(id)
                {
                    if let Some(o) = &original {
                        // Aspect for the covermark preview raster (stacked over the image).
                        edit.frame = o.dimensions();
                        // Cache the base pixels for the real-time effects shader (DRAGON-330):
                        // one copy on decode, uploaded once to the GPU (seq-guarded) thereafter.
                        let (w, h) = o.dimensions();
                        edit.fx_base = Some(PixelFrame::new(o.as_raw().clone(), w, h));
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
                let refit = match self.preview_for(id) {
                    Some(p) if p.surface.is_window() => {
                        let out = self.preview_output.as_ref().map(|(_, o)| *o);
                        // Windows (DRAGON-288): an external `--preview` has no capture anchor,
                        // so `out` is None and the shared fit below would native-size the
                        // window (spilling a large picture off-screen). Fall back to the
                        // preview window's LIVE monitor so the media is bounded to it, exactly
                        // like the open fit — additive, Linux/mac keep `out` unchanged.
                        #[cfg(windows)]
                        let out = out.or_else(|| {
                            crate::platform::windows::window::preview_window_monitor_size(
                                super::shell::PREVIEW_WINDOW_TITLE,
                            )
                        });
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
                                // Windows (DRAGON-288): resize NATIVELY, then clamp+center to
                                // the window's live monitor WORK area so the re-fit can never
                                // exceed the monitor (the shared `window::resize` keeps the
                                // top-left and would push an over-tall fit into a dead zone).
                                // Linux/mac keep iced's `window::resize` byte-identical.
                                #[cfg(windows)]
                                {
                                    crate::platform::windows::window::resize_fit_clamped(
                                        super::shell::PREVIEW_WINDOW_TITLE,
                                        (want.0.round().max(1.0) as u32, want.1.round().max(1.0) as u32),
                                    );
                                    Task::none()
                                }
                                #[cfg(not(windows))]
                                window::resize(
                                    p.window,
                                    cosmic::iced::Size::new(want.0, want.1),
                                )
                            })
                    }
                    _ => None,
                };
                // The DISPLAY effects layer (DRAGON-330) is built on demand from the retained
                // base whenever an effect is drawn/edited — a fresh capture has no annotations
                // yet, so nothing to composite here.
                // Keep the window focused as the spinner gives way to the image (the surface
                // teardown behind the load could otherwise steal focus).
                Task::batch([refit.unwrap_or_else(Task::none), self.focus_preview_window(id)])
            }
            PreviewMsg::Covermark => {
                // Toggle the covermark flyout. Open with the APPLIED mark highlighted (its
                // keyboard index), falling back to the "None" card (index 0).
                let text = self.covermark_text.clone();
                if let Some(p) = self.preview_for_mut(id) {
                    if p.edit.flyout_kind() == Some(edit::FlyoutKind::Covermark) {
                        p.edit.close_flyout();
                    } else {
                        let mut entries = vec![None];
                        entries.extend(edit::covermark_entries(&text).into_iter().map(Some));
                        let selected = match p.edit.covermark.as_ref().map(|c| &c.kind) {
                            Some(kind) => {
                                entries.iter().position(|e| e.as_ref() == Some(kind)).unwrap_or(0)
                            }
                            None => 0,
                        };
                        let len = entries.len();
                        p.edit.picker = Some(Picker { entries });
                        p.edit.open_flyout(edit::FlyoutKind::Covermark, Some(selected), len);
                    }
                }
                Task::none()
            }
            // ── Shared toolbar-flyout keyboard nav (covermark picker + color palette) ────
            PreviewMsg::FlyoutNav(delta) => {
                if let Some(p) = self.preview_for_mut(id)
                    && let Some(f) = &mut p.edit.flyout
                {
                    f.nav(delta);
                }
                Task::none()
            }
            PreviewMsg::FlyoutApply => {
                let recents = self.annot_recent_colors.clone();
                let flyout = self.preview_for(id).and_then(|p| p.edit.flyout);
                match flyout {
                    Some(edit::FlyoutNav { kind: edit::FlyoutKind::Covermark, selected: Some(i), .. }) => {
                        self.update_preview(id, PreviewMsg::PickerPick(i))
                    }
                    Some(edit::FlyoutNav { kind: edit::FlyoutKind::Color, selected: Some(i), .. }) => {
                        match annotate::palette_entries(&recents).get(i) {
                            Some(annotate::PaletteEntry::Color(c)) => {
                                self.update_preview(id, PreviewMsg::SetAnnotColor(*c))
                            }
                            Some(annotate::PaletteEntry::Custom) => {
                                self.update_preview(id, PreviewMsg::AnnotColorEditor(true))
                            }
                            None => Task::none(),
                        }
                    }
                    _ => Task::none(),
                }
            }
            PreviewMsg::FlyoutClose => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.close_flyout();
                }
                Task::none()
            }
            PreviewMsg::PickerPick(idx) => {
                // Each covermark choice is a toggle: picking the one already applied
                // (or the "None" card) turns it OFF; picking a different one replaces it.
                // A covermark starts at THAT option's remembered zoom + opacity (falling
                // back to the global last-used values the first time it's picked).
                let (entry, active_kind) = match self.preview_for(id) {
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
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.close_flyout();
                    p.edit.set_covermark(next);
                }
                self.refresh_edit_display(id)
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
                if delta != 0.0 && self.preview_for(id).is_some() {
                    return self.update_preview(id, PreviewMsg::Zoom(delta / 0.12, 0.0, 0.0));
                }
                Task::none()
            }
            PreviewMsg::Zoom(step, ux, uy) => {
                // Zoom toward the cursor (keep the point under it fixed), then edge-snap.
                let maxz = self.preview_for(id).map(|p| self.max_view_zoom(p)).unwrap_or(Viewport::MAX);
                let minz = self.preview_for(id).map(|p| self.min_view_zoom(p)).unwrap_or(Viewport::MIN);
                let visual = self.preview_for(id).map(|p| self.preview_visual_scale(p)).unwrap_or(1.0);
                if let Some(p) = self.preview_for_mut(id) {
                    let z0 = p.view.zoom;
                    let pan0 = p.view.pan;
                    let z1 = viewport::snap_to_hundred(
                        (z0 * (1.0 + 0.12 * step)).clamp(minz, maxz),
                        visual,
                    );
                    let ratio = z1 / z0;
                    p.view.zoom = z1;
                    p.view.pan = if z1 <= Viewport::FIT {
                        (0.0, 0.0)
                    } else {
                        (ux * (1.0 - ratio) + pan0.0 * ratio, uy * (1.0 - ratio) + pan0.1 * ratio)
                    };
                    let z100 = viewport::preset_zoom(Some(1.0), visual);
                    p.view.zoom_preset = if (z1 - z100).abs() < 1e-3 { Some(1) } else { None };
                }
                // Edge-snap the pan to the (new-zoom) bounds so it can't go out of view.
                let b = self.preview_for(id).map(|p| self.preview_pan_bounds(p));
                if let Some(((minx, maxx), (miny, maxy))) = b
                    && let Some(p) = self.preview_for_mut(id)
                {
                    p.view.pan.0 = p.view.pan.0.clamp(minx, maxx);
                    p.view.pan.1 = p.view.pan.1.clamp(miny, maxy);
                }
                // Sharpen the covermark for the new zoom (no-op when unchanged / no mark).
                self.refresh_covermark_for_zoom(id)
            }
            PreviewMsg::SetViewZoom(z) => {
                let maxz = self.preview_for(id).map(|p| self.max_view_zoom(p)).unwrap_or(Viewport::MAX);
                let minz = self.preview_for(id).map(|p| self.min_view_zoom(p)).unwrap_or(Viewport::MIN);
                let visual = self.preview_for(id).map(|p| self.preview_visual_scale(p)).unwrap_or(1.0);
                if let Some(p) = self.preview_for_mut(id) {
                    // Clamp to the 50%-display floor, then magnetically snap to exactly 100%.
                    let snapped = viewport::snap_to_hundred(z.clamp(minz, maxz), visual);
                    p.view.set_zoom(snapped);
                    let z100 = viewport::preset_zoom(Some(1.0), visual);
                    p.view.zoom_preset = if (p.view.zoom - z100).abs() < 1e-3 { Some(1) } else { None };
                }
                self.refresh_covermark_for_zoom(id)
            }
            PreviewMsg::ZoomPreset(i) => {
                // Presets are in VISUAL terms (100% = natural on-screen size); convert to the
                // viewport's fit-relative multiplier via the current visual scale. So "100%"
                // targets natural size on a 2× capture (zoom = 1/visual_scale = fit at natural),
                // and physical 1:1 lives at the "200%" preset there. "Fit to screen" (None) =
                // fit BOTH (whole picture between the toolbars, zoom 1.0) — never overflow.
                let visual = self.preview_for(id)
                    .map(|p| self.preview_visual_scale(p))
                    .unwrap_or(1.0);
                if let Some(p) = self.preview_for_mut(id) {
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
                self.refresh_covermark_for_zoom(id)
            }
            PreviewMsg::ToggleZoomMenu => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.view.zoom_menu_open = !p.view.zoom_menu_open;
                }
                Task::none()
            }
            PreviewMsg::Pan(dx, dy) => {
                // Clamp panning to the image's overflow beyond the (scrollbar-reserved)
                // viewport, so you can't scroll past the picture's edges.
                let bounds = self.preview_for(id).map(|p| self.preview_pan_bounds(p));
                if let Some(((minx, maxx), (miny, maxy))) = bounds
                    && let Some(p) = self.preview_for_mut(id)
                {
                    p.view.pan.0 = (p.view.pan.0 + dx).clamp(minx, maxx);
                    p.view.pan.1 = (p.view.pan.1 + dy).clamp(miny, maxy);
                }
                Task::none()
            }
            PreviewMsg::SetPanMode(on) => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.view.pan_mode = on;
                }
                Task::none()
            }
            PreviewMsg::TogglePanMode => {
                // The `V` hotkey: flip pan mode. The pointer/pan seg-toggle button reads
                // the same `view.pan_mode`, so UI + hotkey stay in sync.
                if let Some(p) = self.preview_for_mut(id) {
                    p.view.pan_mode = !p.view.pan_mode;
                }
                Task::none()
            }
            PreviewMsg::SetZoom(zoom) => {
                // Live slider drag: update the value AND re-raster LIVE through the shared
                // live-slider path — the covermark re-renders continuously while dragging,
                // debounced by the RasterSlot's coalescing (one raster in flight, ticks collapse
                // to one pending re-run), so a fast drag never thrashes the GPU.
                if let Some(p) = self.preview_for_mut(id)
                    && p.edit.covermark.is_some()
                {
                    p.edit.set_zoom(zoom);
                    self.remember_covermark_pref(id);
                    self.save_state();
                    return self.refresh_live_edit(id, edit::LiveEdit::Covermark);
                }
                Task::none()
            }
            PreviewMsg::SetOpacity(opacity) => {
                // Live slider drag: value + LIVE coalesced re-raster (see SetZoom).
                if let Some(p) = self.preview_for_mut(id)
                    && let Some(cm) = &mut p.edit.covermark
                {
                    cm.opacity = opacity;
                    p.edit.cm_raster.invalidate();
                    self.remember_covermark_pref(id);
                    self.save_state();
                    return self.refresh_live_edit(id, edit::LiveEdit::Covermark);
                }
                Task::none()
            }
            // Slider RELEASE: a final settle through the same shared path (renders the last value
            // in case the trailing tick coalesced).
            PreviewMsg::CommitCovermarkEdit => self.refresh_live_edit(id, edit::LiveEdit::Covermark),
            PreviewMsg::SetDim(dim) => {
                // Live slider drag: update the global dim; the GPU dim pass re-renders from the
                // model on the next view build (no off-thread raster). Latch the pre-drag value
                // on the FIRST tick so the whole drag coalesces into ONE undo entry on release.
                if let Some(p) = self.preview_for_mut(id) {
                    if p.edit.dim_drag_start.is_none() {
                        p.edit.dim_drag_start = Some(p.edit.dim);
                    }
                    p.edit.dim = dim.clamp(0.0, 1.0);
                }
                Task::none()
            }
            PreviewMsg::CommitDimEdit => {
                // Slider RELEASE: push ONE undo entry for the whole drag if the value moved.
                if let Some(p) = self.preview_for_mut(id)
                    && let Some(start) = p.edit.dim_drag_start.take()
                    && (start - p.edit.dim).abs() > f32::EPSILON
                {
                    p.edit.push_dim(start);
                }
                Task::none()
            }
            PreviewMsg::Undo => {
                // The shared history walks covermark, timeline AND annotation edits; only a
                // covermark change needs its async raster refreshed (timeline + annotation
                // changes redraw on the next view build for free — annotations as vectors).
                let kind = if let Some(PreviewState { kind, edit, .. }) = self.preview_for_mut(id) {
                    let tl = match kind {
                        PreviewKind::Video(vid) => vid.timeline.as_mut(),
                        PreviewKind::Image(_) => None,
                    };
                    edit.undo(tl)
                } else {
                    None
                };
                match kind {
                    Some(EditKind::Covermark) => self.refresh_edit_display(id),
                    // Box/arrow redraw as vectors for free (DRAGON-324); the effect layer
                    // (highlight/pixelate/blur) re-renders through the GPU shader from the
                    // restored model on the next view build (DRAGON-330). A dim change likewise
                    // re-renders via the GPU dim pass for free (DRAGON-329).
                    Some(EditKind::Annotations) | Some(EditKind::Dim) => Task::none(),
                    _ => Task::none(),
                }
            }
            PreviewMsg::Redo => {
                let kind = if let Some(PreviewState { kind, edit, .. }) = self.preview_for_mut(id) {
                    let tl = match kind {
                        PreviewKind::Video(vid) => vid.timeline.as_mut(),
                        PreviewKind::Image(_) => None,
                    };
                    edit.redo(tl)
                } else {
                    None
                };
                match kind {
                    Some(EditKind::Covermark) => self.refresh_edit_display(id),
                    // Box/arrow redraw as vectors for free (DRAGON-324); the effect layer
                    // (highlight/pixelate/blur) re-renders through the GPU shader from the
                    // restored model on the next view build (DRAGON-330). A dim change likewise
                    // re-renders via the GPU dim pass for free (DRAGON-329).
                    Some(EditKind::Annotations) | Some(EditKind::Dim) => Task::none(),
                    _ => Task::none(),
                }
            }
            PreviewMsg::CovermarkRasterReady(generation, frame) => {
                // The covermark overlay raster (stacked over the base image/video via the
                // persistent-texture shader) — NOT a full re-composite, so the base never
                // re-uploads. `finish` drops stale generations and reports whether another
                // refresh was requested while this one was in flight.
                let again = self.preview_for_mut(id)
                    .map(|p| p.edit.cm_raster.finish(generation, frame))
                    .unwrap_or(false);
                if again {
                    return self.refresh_edit_display(id);
                }
                Task::none()
            }
            // ── Annotation editor (IMAGES only) ──────────────────────────────────────
            PreviewMsg::SelectTool(tool) => {
                self.select_annot_tool(id, tool);
                Task::none()
            }
            PreviewMsg::ToolPressed(tool) => {
                // A tray BUTTON press (DRAGON-339): ask the double-click detector first — the
                // second press of the same button inside the window ALSO drops a ready-made item
                // in the middle of the picture (one undo entry, selected), on top of the ordinary
                // arm-the-tool behavior every press has.
                let double = self.preview_for_mut(id)
                    .is_some_and(|p| p.edit.tool_clicks.press(tool, std::time::Instant::now()));
                self.select_annot_tool(id, tool);
                if double {
                    self.spawn_annotation(id, tool);
                }
                Task::none()
            }
            PreviewMsg::SetAnnotColor(color) => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.annot_color = Some(color);
                    p.edit.close_flyout();
                }
                // Picking a color also recolors the SELECTED colorable item immediately (one
                // undo entry) — same effect as the right-click "Set to current color".
                self.recolor_selected_annotation(id, color);
                // Persist so the next preview opens with this color.
                self.annot_color = Some(color);
                self.save_state();
                // Recoloring a highlight re-renders through the GPU shader on the next view
                // build (DRAGON-330).
                Task::none()
            }
            PreviewMsg::SetAnnotStrokeW(w) => {
                self.apply_annot_stroke_w(id, w);
                Task::none()
            }
            PreviewMsg::CycleAnnotStrokeW => {
                // The `L` hotkey: advance to the next width preset (2 → 5 → 8 → 2), applying to
                // the selection + persisting, exactly like clicking the next segment.
                let current = self.preview_for(id)
                    .map(|p| p.edit.stroke())
                    .unwrap_or(annotate::DEFAULT_ANNOT_STROKE);
                self.apply_annot_stroke_w(id, annotate::cycle_stroke_width(current));
                Task::none()
            }
            PreviewMsg::ToggleAnnotPalette => {
                // Toggle the COLOR palette flyout. Open with the ACTIVE color highlighted
                // (matched across ALL swatches, incl. the custom MRU); no highlight if the
                // active color isn't present.
                let entries = annotate::palette_entries(&self.annot_recent_colors);
                if let Some(p) = self.preview_for_mut(id) {
                    if p.edit.flyout_kind() == Some(edit::FlyoutKind::Color) {
                        p.edit.close_flyout();
                    } else {
                        let current =
                            p.edit.annot_color.unwrap_or_else(annotate::default_annot_color);
                        let sel = entries.iter().position(|e| e.matches_color(current));
                        p.edit.picker = None;
                        p.edit.open_flyout(edit::FlyoutKind::Color, sel, entries.len());
                    }
                }
                Task::none()
            }
            PreviewMsg::AnnotColorEditor(open) => {
                if let Some(p) = self.preview_for_mut(id) {
                    if open {
                        // Seed the wheel from the current color; close the palette flyout.
                        let bytes =
                            p.edit.annot_color.unwrap_or_else(annotate::default_annot_color);
                        let init = cosmic::iced::Color::from_rgb(
                            bytes[0] as f32 / 255.0,
                            bytes[1] as f32 / 255.0,
                            bytes[2] as f32 / 255.0,
                        );
                        p.edit.annot_picker = Some(cosmic::widget::ColorPickerModel::new(
                            "Hex",
                            "RGB",
                            None,
                            Some(init),
                        ));
                        p.edit.close_flyout();
                    } else {
                        p.edit.annot_picker = None;
                    }
                }
                Task::none()
            }
            PreviewMsg::AnnotColorPickerUpdate(u) => {
                if let Some(p) = self.preview_for_mut(id)
                    && let Some(model) = &mut p.edit.annot_picker
                {
                    // Drive the libcosmic picker; its returned Task only ever writes the hex to
                    // the clipboard (the copy button), which is fine to run.
                    return model.update::<Msg>(u).map(cosmic::Action::App);
                }
                Task::none()
            }
            PreviewMsg::AnnotColorApply => {
                // Read the wheel's current color, apply + persist + push the MRU + close.
                // The model must process `AppliedColor` FIRST (DRAGON-348): our own Apply
                // button bypasses the picker's internal save affordance, and until that
                // action runs `get_applied_color()` still holds the INITIAL color the picker
                // opened with — applying then pushed/selected the STALE color, which read as
                // "nothing rotated, nothing added, nothing selected". The returned Task is
                // droppable (only the copy button's update does real work).
                let picked = self.preview_for_mut(id)
                    .and_then(|p| p.edit.annot_picker.as_mut())
                    .and_then(|m| {
                        let _ = m.update::<Msg>(
                            cosmic::widget::color_picker::ColorPickerUpdate::AppliedColor,
                        );
                        m.get_applied_color()
                    })
                    .map(|c| {
                        [
                            (c.r * 255.0).round() as u8,
                            (c.g * 255.0).round() as u8,
                            (c.b * 255.0).round() as u8,
                            255,
                        ]
                    });
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.annot_picker = None;
                    if let Some(c) = picked {
                        p.edit.annot_color = Some(c);
                    }
                }
                if let Some(c) = picked {
                    // Applying a custom wheel color also recolors the SELECTED colorable item.
                    self.recolor_selected_annotation(id, c);
                    self.annot_color = Some(c);
                    self.push_recent_color(c);
                    self.save_state();
                }
                // A recolored highlight re-renders through the GPU shader (DRAGON-330).
                Task::none()
            }
            PreviewMsg::SelectAnnotation(annot) => {
                if let Some(p) = self.preview_for_mut(id) {
                    match annot {
                        Some(annot) => p.edit.sel.set_one(annot),
                        None => p.edit.sel.clear(),
                    }
                    p.edit.annot_menu = None;
                }
                Task::none()
            }
            PreviewMsg::ToggleAnnotationSelected(annot) => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.sel.toggle(annot);
                    p.edit.annot_menu = None;
                }
                Task::none()
            }
            PreviewMsg::BandSelectAnnotations(x0, y0, x1, y1, additive) => {
                self.band_select_annotations(id, x0, y0, x1, y1, additive);
                Task::none()
            }
            PreviewMsg::SelectAllAnnotations => {
                self.select_all_annotations(id);
                Task::none()
            }
            PreviewMsg::AnnotDrawBegin(tool, x, y) => self.annot_draw_begin(id, tool, x, y),
            PreviewMsg::AnnotGrabBegin(grab, x, y) => self.annot_grab_begin(id, grab, x, y),
            PreviewMsg::AnnotGestureTo(x, y) => self.annot_gesture_to(id, x, y),
            PreviewMsg::AnnotGestureEnd => self.annot_gesture_end(id),
            PreviewMsg::DeleteSelected => self.annot_delete_selected(id),
            PreviewMsg::DuplicateSelected => self.duplicate_selected_annotation(id),
            PreviewMsg::SetSelectedColor => {
                // Recolor the selected colorable item to the CURRENT annotation color (one undo
                // entry) — a highlight keeps its 45% tint at the new hue; pixelate/blur (no
                // color) are skipped. Shares the recolor path with picking a color while selected.
                let color = self.preview_for(id)
                    .and_then(|p| p.edit.annot_color)
                    .unwrap_or_else(annotate::default_annot_color);
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.annot_menu = None;
                }
                self.recolor_selected_annotation(id, color);
                // Recoloring a highlight re-renders through the GPU shader (DRAGON-330).
                Task::none()
            }
            PreviewMsg::RaiseSelected => self.annot_reorder(id, Reorder::Up),
            PreviewMsg::LowerSelected => self.annot_reorder(id, Reorder::Down),
            PreviewMsg::SelectionToFront => self.annot_reorder(id, Reorder::Front),
            PreviewMsg::SelectionToBack => self.annot_reorder(id, Reorder::Back),
            PreviewMsg::AnnotMenuOpen(x, y) => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.annot_menu = Some((x, y));
                }
                Task::none()
            }
            PreviewMsg::AnnotMenuClose => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.annot_menu = None;
                }
                Task::none()
            }
            PreviewMsg::BakeDone(size) => {
                // Captured before borrowing `p`: whether the editor stays open after the
                // share (the "auto close" setting is off).
                let keep_open = !self.auto_close_preview;
                let Some(p) = self.preview_for_mut(id) else {
                    return Task::none();
                };
                p.edit.baking = false;
                let intent = p.edit.pending.take();
                let output = p.edit.pending_output.take();
                let is_video = matches!(p.kind, PreviewKind::Video(_));
                let saved_path = p.path.clone();
                match size {
                    Some(size) => {
                        self.stop_preview_playback(id);
                        match intent {
                            // Save baked the capture IN PLACE. When keeping the editor
                            // open, the edits are now part of the file, so commit them
                            // into the base: reset the edit state and reload the baked
                            // result as the new baseline (so further edits start clean).
                            Some(ShareIntent::Save) => {
                                if let Some(p) = self.preview_for_mut(id) {
                                    p.size = Some(size);
                                    p.edit.covermark = None;
                                    p.edit.undo_stack.clear();
                                    p.edit.redo_stack.clear();
                                    // Annotations are baked into the file now — clear the
                                    // scene so the re-decoded baseline (which already shows
                                    // them) isn't double-marked.
                                    p.edit.annotations.clear();
                                    p.edit.sel.clear();
                                    p.edit.gesture = None;
                                    p.edit.annot_snapshot = None;
                                    p.edit.annot_menu = None;
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
                                        (false, Some(path)) => image::decode_task(id, path),
                                        (true, Some(path)) => video::poster_task(id, path),
                                        _ => Task::none(),
                                    }
                                } else {
                                    // THIS document is done (the process only ends with
                                    // the last one — DRAGON-336 phase 2).
                                    self.close_preview(id)
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
                                    if let Some(p) = self.preview_for_mut(id) {
                                        p.edit.covermark = None;
                                        p.edit.undo_stack.clear();
                                        p.edit.redo_stack.clear();
                                    }
                                    self.close_preview(id)
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
                        self.close_preview(id)
                    }
                }
            }
            PreviewMsg::ConfirmOverwrite => {
                // The user OK'd overwriting the file: bake the edits into it in place
                // (background, behind the processing notification), then finish. `begin_bake`
                // uses the preview's own path, and BakeDone(Save) reveals + finishes/keeps.
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.confirm_overwrite = false;
                }
                if let Some(task) = self.begin_bake(id, ShareIntent::Save) {
                    return task;
                }
                // No edits after all (shouldn't happen — the dialog only opens with edits):
                // nothing to write, just close this document.
                self.close_preview(id)
            }
            PreviewMsg::CancelOverwrite => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.confirm_overwrite = false;
                }
                Task::none()
            }
            PreviewMsg::PosterReady(poster, meta) => {
                let mut wave = Task::none();
                if let Some(PreviewState { kind: PreviewKind::Video(vid), edit, path, .. }) =
                    self.preview_for_mut(id)
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
                            wave = video::waveform_task(id, path);
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
                let refit = match (self.preview_for(id), meta) {
                    (Some(p), Some(_)) if p.surface.is_window() => {
                        let out = self.preview_output.as_ref().map(|(_, o)| *o);
                        // Windows (DRAGON-288): an external `--preview` video has no capture
                        // anchor (`out` None) — bound the fit to the preview window's LIVE
                        // monitor instead of native-sizing it off-screen. Additive; Linux/mac
                        // keep `out` unchanged.
                        #[cfg(windows)]
                        let out = out.or_else(|| {
                            crate::platform::windows::window::preview_window_monitor_size(
                                super::shell::PREVIEW_WINDOW_TITLE,
                            )
                        });
                        // Logical (backing-scale-divided) footprint so a Retina recording
                        // re-fits to its true on-screen size, matching the open fit.
                        let target = p.sizing_media_points();
                        let want =
                            windowed_fit_size(target, out, transport_h_for(&p.kind, p.surface));
                        // Only when meaningfully off — the open-time guess is often exact.
                        ((want.0 - p.monitor.0 as f32).abs() > 2.0
                            || (want.1 - p.monitor.1 as f32).abs() > 2.0)
                            .then(|| {
                                // Windows (DRAGON-288): resize NATIVELY + clamp+center to the
                                // window's live monitor WORK area (see the image path above);
                                // Linux/mac keep iced's `window::resize` byte-identical.
                                #[cfg(windows)]
                                {
                                    crate::platform::windows::window::resize_fit_clamped(
                                        super::shell::PREVIEW_WINDOW_TITLE,
                                        (want.0.round().max(1.0) as u32, want.1.round().max(1.0) as u32),
                                    );
                                    Task::none()
                                }
                                #[cfg(not(windows))]
                                window::resize(
                                    p.window,
                                    cosmic::iced::Size::new(want.0, want.1),
                                )
                            })
                    }
                    _ => None,
                };
                // Keep the window focused as the spinner gives way to the poster.
                Task::batch([wave, refit.unwrap_or_else(Task::none), self.focus_preview_window(id)])
            }
            // Playback / scrub / frame-step / timeline edits are video-only (no-ops
            // otherwise); the logic lives in `video.rs` next to the playback state.
            PreviewMsg::Play => self.toggle_playback(id),
            PreviewMsg::PlayerTick => self.playback_tick(id),
            PreviewMsg::Seek(t) => self.seek(id, t),
            PreviewMsg::FrameStep(delta) => self.frame_step(id, delta),
            PreviewMsg::SeekFrameReady(handle) => self.on_seek_frame(id, handle),
            PreviewMsg::TimelineSeek(t) => self.timeline_seek(id, t),
            PreviewMsg::TimelineSelect(t, ctrl, shift) => self.timeline_select(id, t, ctrl, shift),
            PreviewMsg::TimelineBoxSelect(a, b, additive) => {
                self.timeline_box_select(id, a, b, additive)
            }
            PreviewMsg::TimelineCut(t) => self.timeline_cut(id, t),
            PreviewMsg::TimelineRazor(on) => self.timeline_set_razor(id, on),
            PreviewMsg::TimelineDelete => self.timeline_delete_selected(id),
            PreviewMsg::TimelineMenuOpen(t, x, y) => self.timeline_menu_open(id, t, x, y),
            PreviewMsg::TimelineMenuClose => self.timeline_menu_close(id),
            PreviewMsg::WaveformReady(peaks) => self.on_waveform(id, peaks),
            PreviewMsg::Save => {
                let external = self.preview_for(id).is_some_and(|p| p.external);
                // Anything to bake into the file? A covermark or deleted timeline
                // segments bake new pixels. If so, Save always confirms the overwrite
                // first (no cleverness about "nothing changed") — via the in-app modal,
                // clickable over the overlay grab and in the window alike. The
                // background bake runs on ConfirmOverwrite.
                let would_write =
                    self.preview_for(id).is_some_and(|p| p.dirty());
                if would_write {
                    if let Some(p) = self.preview_for_mut(id) {
                        p.edit.confirm_overwrite = true;
                    }
                    return Task::none();
                }
                // Nothing to write: a `--preview` file is the user's and untouched; a fresh
                // capture already lives at its path. Reveal it and finish/keep.
                if !external
                    && let Some(path) = self.preview_for(id).and_then(|p| p.path.as_ref())
                {
                    crate::platform::services::notify(path, false);
                }
                self.finish_or_keep_preview(id)
            }
            PreviewMsg::Copy => {
                // Pending edits bake first so the clipboard gets the edited capture.
                if let Some(task) = self.begin_bake(id, ShareIntent::Copy) {
                    return task;
                }
                let is_video = matches!(
                    self.preview_for(id).map(|p| &p.kind),
                    Some(PreviewKind::Video(_))
                );
                if let Some(path) = self.preview_for(id).and_then(|p| p.path.as_ref()) {
                    crate::platform::services::copy_to_clipboard(path, is_video);
                    crate::platform::services::notify(path, true);
                }
                self.finish_or_keep_preview(id)
            }
            PreviewMsg::Cancel => {
                // Close without deleting — the file stays where it is. Deleting is the
                // explicit Delete (trash) action.
                self.stop_preview_playback(id);
                self.close_preview(id)
            }
            PreviewMsg::ToggleAppearance => self.toggle_preview_appearance(id),
            PreviewMsg::WindowDrag => match self.preview_for(id) {
                Some(p) => window::drag(p.window),
                None => Task::none(),
            },
            PreviewMsg::WindowMaximize => {
                // Windows (DRAGON-258): the windowed preview is a frameless, natively-managed
                // toplevel, so iced's `window::toggle_maximize` is a no-op for it. Route to the
                // native Win32 helper keyed on the preview window title; keep Linux/mac on iced.
                #[cfg(windows)]
                crate::platform::windows::window::toggle_maximize(super::shell::PREVIEW_WINDOW_TITLE);
                #[cfg(windows)]
                return Task::none();
                #[cfg(not(windows))]
                match self.preview_for(id) {
                    Some(p) => window::toggle_maximize(p.window),
                    None => Task::none(),
                }
            }
            PreviewMsg::WindowMinimize => {
                // Windows (DRAGON-258): iced's `window::minimize` is likewise a no-op for the
                // frameless preview toplevel — native `ShowWindow(SW_MINIMIZE)` instead.
                #[cfg(windows)]
                crate::platform::windows::window::minimize(super::shell::PREVIEW_WINDOW_TITLE);
                #[cfg(windows)]
                return Task::none();
                #[cfg(not(windows))]
                match self.preview_for(id) {
                    Some(p) => window::minimize(p.window, true),
                    None => Task::none(),
                }
            }
            PreviewMsg::Delete => {
                // Never delete a pre-existing `--preview` file (no trash button there).
                if self.preview_for(id).is_some_and(|p| p.external) {
                    return Task::none();
                }
                // Explicitly delete the captured file, then close.
                self.stop_preview_playback(id);
                if let Some(path) = self.preview_for(id).and_then(|p| p.path.as_ref()) {
                    let _ = std::fs::remove_file(path);
                }
                self.close_preview(id)
            }
            PreviewMsg::SaveAs => {
                // Ask WHERE to save first — no bake up front. The bake (if any) runs in the
                // background against the chosen destination in `SaveAsResult`, tracked by the
                // "Processing capture" notification, so the user isn't blocked before the
                // dialog.
                self.save_as_dialog(id)
            }
            PreviewMsg::SaveAsResult(opt) => {
                let Some(dest) = opt else {
                    // Cancelled. A window is still open (only overlays close for the dialog),
                    // so stay on it; an overlay was torn down for the dialog, so bring it
                    // BACK — the capture and its edits are still loaded, and a cancelled
                    // dialog must return the user to where they were, not exit
                    // (DRAGON-157).
                    return if self.preview_for(id).is_some_and(|p| p.surface.is_window()) {
                        Task::none()
                    } else {
                        self.reopen_preview_surface(id)
                    };
                };
                // Gather everything the background worker needs, then release `self`.
                let external = self.preview_for(id).is_some_and(|p| p.external);
                // Keep-open Save As is an EXPORT: the editor continues on its working
                // document (undo/redo intact), so the working file must SURVIVE the
                // save — copy semantics, never a move (see `SaveAsBaked`).
                let keep_open = !self.auto_close_preview;
                let (src, covermark, annotations, annot_curve, dim, video, is_video) = match self.preview_for(id) {
                    Some(p) => {
                        let Some(src) = p.path.clone() else {
                            return self.close_preview(id);
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
                        // Annotations + dim are IMAGES only; a video never accumulates them.
                        (src, p.edit.covermark.clone(), p.edit.annotations.clone(), p.edit.curve_radius(), p.edit.dim, video, is_video)
                    }
                    None => return self.close_preview(id),
                };
                // Only bake when there's something to apply AND we can (video needs meta).
                let cuts = video.as_ref().is_some_and(|v| v.keep.is_some());
                let can_bake = (covermark.is_some() || cuts || !annotations.is_empty() || dim > 0.0)
                    && (!is_video || video.is_some());
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
                            None => edit::bake_image(
                                &src,
                                &dest,
                                covermark.as_ref(),
                                &annotations,
                                annot_curve,
                                dim,
                            ),
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
                    cosmic::Action::App(Msg::Preview(id, PreviewMsg::SaveAsBaked(done)))
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
                if self.auto_close_preview || self.preview_for(id).is_none() {
                    return self.close_preview(id);
                }
                let committed_in_place = match (
                    done.as_ref(),
                    self.preview_for(id).and_then(|p| p.path.as_ref()),
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
                        self.preview_for(id).map(|p| &p.kind),
                        Some(PreviewKind::Video(_))
                    );
                    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                    self.stop_preview_playback(id);
                    self.reload_preview_in_place(id, dest, size, is_video)
                } else {
                    Task::none()
                };
                let surface = if self.preview_for(id).is_some_and(|p| p.surface.is_window()) {
                    Task::none()
                } else {
                    self.reopen_preview_surface(id)
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
            ducking: false,
            surface_open: true,
            demoted: false,
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

    // ── Multi-document previews (DRAGON-336 phase 2) ────────────────────────────

    /// A preview open on `surface`, carrying only what the routing/close rules read.
    fn doc(surface: PreviewSurface) -> PreviewState {
        PreviewState { surface, ..still_at((100, 100), 1.0) }
    }

    /// Lookup is BY SURFACE ID, not by position: each document is found at its own id
    /// and an id nobody owns resolves to nothing (a stale async completion is a no-op,
    /// never a hit on the wrong document).
    #[test]
    fn previews_are_looked_up_by_window_id() {
        let docs = vec![doc(PreviewSurface::Window), doc(PreviewSurface::Window)];
        assert_eq!(index_of(&docs, docs[0].window), Some(0));
        assert_eq!(index_of(&docs, docs[1].window), Some(1));
        assert_eq!(index_of(&docs, window::Id::unique()), None, "an unknown id matches nothing");
        assert_eq!(index_of(&[], docs[0].window), None, "nothing is open, nothing matches");
    }

    /// The close decision: only the LAST open preview ends the process. Closing one of
    /// several leaves the others (and the process) alive.
    #[test]
    fn closing_the_last_preview_ends_the_process_but_closing_one_of_many_does_not() {
        let one = vec![doc(PreviewSurface::Window)];
        assert!(closing_is_last(&one, one[0].window), "the only document is the last one out");

        let many = vec![doc(PreviewSurface::Window), doc(PreviewSurface::Window)];
        assert!(!closing_is_last(&many, many[0].window), "a sibling is still open");
        assert!(!closing_is_last(&many, many[1].window));
    }

    /// A DOUBLE close (or a stale completion for an already-removed document) must not
    /// kill live siblings — but with nothing left at all it still means "we're done", so
    /// a re-entrant close of the final document can't strand a windowless process.
    #[test]
    fn closing_an_unknown_id_only_ends_the_process_when_nothing_is_open() {
        let gone = window::Id::unique();
        let many = vec![doc(PreviewSurface::Window)];
        assert!(!closing_is_last(&many, gone), "an unknown id must not close live documents");
        assert!(closing_is_last(&[], gone), "nothing open at all is still last-one-out");
    }

    /// THE LONE DOCUMENT'S APPEARANCE: a preview may be the fullscreen overlay only while
    /// it is the ONLY one open. Opening a SECOND document bars the overlay for the newcomer
    /// AND selects the sibling holding it for demotion — so once the pass is done NOBODY is
    /// an overlay, rather than one overlay sitting behind a floating window.
    #[test]
    fn a_second_document_leaves_nobody_on_the_overlay() {
        let none_open: Vec<PreviewState> = Vec::new();
        assert!(!overlay_taken(&none_open, None), "nothing open: the overlay is free");
        assert!(overlay_siblings(&none_open, None).is_empty(), "nobody to demote");

        // One document holds the overlay; a fresh one is about to open beside it.
        let mut docs = vec![doc(PreviewSurface::Overlay)];
        let held = docs[0].window;
        assert!(overlay_taken(&docs, None), "the newcomer must be a window");
        assert_eq!(overlay_siblings(&docs, None), vec![held], "and the holder comes down too");

        // Apply the demotion the way `demote_preview_to_window` does, then add the
        // newcomer: the end state is N windows, with no overlay anywhere.
        docs[0].surface = PreviewSurface::Window;
        docs[0].demoted = true;
        docs.push(doc(PreviewSurface::Window));
        assert!(
            docs.iter().all(|p| p.surface.is_window()),
            "never a mix: with two documents open they are ALL windows"
        );
        assert!(overlay_siblings(&docs, None).is_empty(), "nothing left to demote");
    }

    /// A document being RE-minted never blocks itself (its old surface is torn down in the
    /// same pass), so a LONE preview still round-trips overlay→overlay — the whole
    /// single-document path is unchanged. It is also never its own demotion sibling.
    #[test]
    fn a_single_document_may_still_hold_the_overlay() {
        let docs = vec![doc(PreviewSurface::Overlay)];
        let me = docs[0].window;
        assert!(!overlay_taken(&docs, Some(me)), "a lone document may re-mint as the overlay");
        assert!(overlay_siblings(&docs, Some(me)).is_empty(), "and is never its own sibling");
        assert!(overlay_taken(&docs, Some(window::Id::unique())), "but it blocks everyone else");
    }

    /// ONCE DEMOTED, ALWAYS WINDOWED: a document that was brought down from the overlay
    /// keeps the sticky pin, so when its siblings close and it is alone again it does NOT
    /// silently re-enter fullscreen.
    #[test]
    fn a_demoted_document_is_not_promoted_back_when_it_is_alone_again() {
        let mut alone = doc(PreviewSurface::Window);
        alone.demoted = true;
        let me = alone.window;
        let docs = vec![alone];
        assert!(
            overlay_taken(&docs, Some(me)),
            "the only document open, but demoted earlier — it stays a window"
        );
        // A document that was never demoted is unaffected by the pin.
        let fresh = vec![doc(PreviewSurface::Overlay)];
        assert!(!overlay_taken(&fresh, Some(fresh[0].window)));
    }

    /// A document whose SURFACE was torn down while it stays loaded (a background bake, the
    /// overlay's Save-As dialog) is never demoted: there is nothing on screen to bring
    /// down, and re-minting one would resurrect a surface that was closed on purpose.
    #[test]
    fn a_torn_down_surface_is_not_demoted() {
        let mut hidden = doc(PreviewSurface::Overlay);
        hidden.surface_open = false;
        let docs = vec![hidden, doc(PreviewSurface::Window)];
        assert!(overlay_siblings(&docs, None).is_empty(), "a dead surface is not demoted");
        assert!(overlay_taken(&docs, None), "but it still bars the overlay for a newcomer");
    }

    /// The audio-duck refcount: the guard is engaged for the FIRST holder and dropped
    /// only for the LAST. This is what stops one of several video previews un-muting the
    /// desktop under the others when it stops.
    #[test]
    fn duck_engages_on_the_first_holder_and_drops_on_the_last() {
        let (a, b) = (window::Id::unique(), window::Id::unique());
        let mut refs = DuckRefs::default();
        assert!(!refs.held(), "nothing holds the guard to begin with");

        assert!(refs.acquire(a), "the first holder engages the guard");
        assert!(!refs.acquire(b), "a second holder must NOT re-engage it");
        assert!(refs.held());

        assert!(!refs.release(a), "releasing one of two must NOT drop the guard");
        assert!(refs.held(), "the desktop stays muted under the surviving preview");
        assert!(refs.release(b), "the last release drops it");
        assert!(!refs.held());
    }

    /// Acquire/release are IDEMPOTENT per document: a repeated engage is not a second
    /// reference (it would strand the guard forever), and releasing a non-holder is a
    /// no-op that can never drop a guard someone else still wants.
    #[test]
    fn duck_refs_are_idempotent_per_document() {
        let (a, b) = (window::Id::unique(), window::Id::unique());
        let mut refs = DuckRefs::default();
        assert!(refs.acquire(a));
        assert!(!refs.acquire(a), "the same document twice is still one reference");
        assert!(refs.release(a), "so one release is enough to drop it");
        assert!(!refs.held());

        let mut refs = DuckRefs::default();
        refs.acquire(a);
        assert!(!refs.release(b), "releasing a non-holder never drops the guard");
        assert!(refs.held());
    }

    /// A document that re-mints its surface keeps its hold under the NEW id — the guard
    /// is neither engaged nor dropped by an appearance toggle / cover→window swap.
    #[test]
    fn duck_hold_follows_a_document_across_a_surface_re_mint() {
        let (old, new) = (window::Id::unique(), window::Id::unique());
        let mut refs = DuckRefs::default();
        refs.acquire(old);
        refs.rename(old, new);
        assert!(refs.held(), "the re-mint must not drop the guard");
        assert!(!refs.release(old), "the stale id no longer holds anything");
        assert!(refs.held());
        assert!(refs.release(new), "the new id owns the hold");
        assert!(!refs.held());
    }
}
