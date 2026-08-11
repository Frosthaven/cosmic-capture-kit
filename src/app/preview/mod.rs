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
// `pub(in crate::app)`: the colour picker window borrows `chrome::flyout` (the shared
// chip + popover recipe) for its mode menu (DRAGON-630), so the module has to be
// reachable from a sibling; everything else in it stays `pub(super)`.
pub(in crate::app) mod chrome;
mod covermark;
mod crop;
// `pub(crate)` for ONE caller outside this module: `subscriptions::sub_upload_poll` reads
// `edit::upload_needs_poll` to decide whether a document still needs its 500ms upload tick
// (DRAGON-514). The alternative was re-implementing that predicate in the subscription, which
// is exactly the drift the pure decision exists to prevent.
pub(crate) mod edit;
mod image;
mod layers;
mod naming;
mod open;
mod playback;
mod share;
mod sizing;
mod toast;
mod surface;
pub(crate) mod text_annot;
mod timeline;
mod video;
mod viewport;

pub use image::ImagePreview;
pub use video::VideoPreview;
// The persistent-GPU-texture layer stack. `PixelFrame` was the only name that had to leave
// this module until DRAGON-TBD; the colour picker's magnifier now draws through the same
// shader (its raster was churning iced's atlas on every pointer move), and `mod layers` is
// private to `preview`, so the three names its view builds a stack from come out here too.
// Nothing about the stack was preview-specific, see `layers`' own module doc.
pub use layers::{Layer, LayerKey, LayerStack, PixelFrame};
pub use edit::covermark_dir;
pub(crate) use annotate::AnnotId;
// The STAGED escape (DRAGON-468): `keyboard.rs` asks what a press gives up before the keymap
// sees it. Re-exported rather than reached into, like every other cross-module preview item.
pub(crate) use annotate::{escape_stage, EscapeStage};

use annotate::Reorder;
use edit::{Covermark, CovermarkKind, EditKind, EditState, Picker};
use toast::{ToastKind, Toasts};
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

/// Playful lines shown under the editor's PROCESSING spinner while a bake / export
/// re-encodes (DRAGON-353). The editor no longer vanishes behind a desktop "Processing
/// capture" notification for that work — it stays up and dims itself behind this — so the
/// wait needed its own copy set, in the same voice as the load-time lines above.
pub(super) const PREVIEW_PROCESSING_MESSAGES: [&str; 20] = [
    "Baking in your edits",
    "Committing your changes",
    "Working the edits in",
    "Re-encoding your capture",
    "Making the edits permanent",
    "Pressing the changes in",
    "Applying every mark",
    "Writing it all down",
    "Folding the edits together",
    "Rendering the final cut",
    "Sealing in your work",
    "Flattening the layers",
    "Putting it all together",
    "Finishing the export",
    "Setting your edits",
    "Merging it into the file",
    "Locking in the changes",
    "Processing your capture",
    "Nearly through the encode",
    "Wrapping up the export",
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

/// [`random_loading_msg`] for the PROCESSING copy set — picked per bake, so two exports in
/// one session don't repeat the same line.
fn random_processing_msg() -> usize {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0))
        % PREVIEW_PROCESSING_MESSAGES.len()
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
    ///
    /// **This is the MEDIA source and it never moves** (DRAGON-353 follow-up). It is what
    /// the player plays, what a seek decodes from, and what a bake reads — the untouched
    /// base the live scene (annotations, covermark, timeline) is composited over. A save
    /// writes somewhere ELSE and records that in [`Self::saved_path`]; it does not repoint
    /// this. That separation is what lets the undo history survive a save: the document
    /// still renders base + scene, so undo still means something. Repointing this at a
    /// baked file would double-apply every edit on the next undo.
    pub path: Option<PathBuf>,
    /// The PRISTINE media every bake reads, when it is no longer [`Self::path`]
    /// (DRAGON-467 review, blocker 1). `None` = `path` is still pristine, which is the
    /// normal case.
    ///
    /// It becomes `Some` exactly once: when a STILL is saved over its own source, the bytes
    /// are snapshotted into the session runtime directory first and this is repointed at the
    /// snapshot. Everything downstream keeps compositing the live scene onto untouched
    /// pixels, so undo still works and a second save does not double every annotation.
    /// See [`edit::bake_prep`] for the whole rule, including why a RECORDING commits instead
    /// of snapshotting.
    pub bake_src: Option<PathBuf>,
    /// Where this document last SAVED, when it has (DRAGON-353 follow-up) — the destination
    /// the user picked in the Save dialog. `None` = never saved.
    ///
    /// It is the document's IDENTITY on disk (what a further Save offers to overwrite, what a
    /// Copy puts on the clipboard, what the toasts name), while [`Self::path`] stays the media
    /// it renders from. [`App::preview_save_target`] resolves the next save target from this
    /// plus the user's configured save folder.
    pub saved_path: Option<PathBuf>,
    // DRAGON-467 removed `written` from here, with `note_written` and `delete_paths`. The
    // three existed solely so Delete could unlink every file this document produced (the
    // capture plus each save destination, wherever the user pointed them), tracked rather
    // than derived so a neighbouring file from an earlier session could never be swept up.
    // The editor does not delete anything any more, so nothing read them. If a delete ever
    // comes back it needs this again: deriving the set from a name pattern is what the
    // tracking existed to avoid.
    /// The saved file's size in bytes (shown as a chip), once known.
    pub size: Option<u64>,
    /// `true` when previewing a pre-existing file (`--preview`) rather than a fresh
    /// capture: the file isn't ours to manage, so it is never auto-copied to the clipboard
    /// on open and a Save offers its OWN folder rather than the capture folder.
    pub external: bool,
    /// The monitor's logical size — scales the content within the space above the toolbar.
    pub monitor: (u32, u32),
    /// The SOURCE display's point→pixel backing scale (2.0 on a Retina panel). The
    /// capture arrives in PHYSICAL pixels; dividing by this yields the LOGICAL points
    /// the picture occupied on screen, which is what the WINDOW preview opens at (so a
    /// Retina grab isn't shown 2× too large). Resolved on EVERY platform from the SOURCE
    /// output's own reported scale (`App::scale_for_selection`: the COSMIC output's buffer
    /// scale on Linux, `NSScreen.backingScaleFactor` on macOS, `GetDpiForMonitor / 96` on
    /// Windows) — `1.0` only on an unscaled panel, on a `--preview` file, or when the
    /// source output can't be resolved. On those `1.0` cases every consumer stays
    /// byte-identical, which is what makes this safe to thread everywhere.
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
    ///
    /// It is also the "WE did this" signal ([`surface_closed`], DRAGON-469). Off Linux a
    /// preview surface is a real winit window and `window::close` echoes a
    /// `window::Event::Closed` back for our own teardown, indistinguishable from a window
    /// manager destroying it — so every path that tears a surface down while keeping the
    /// document MUST clear this BEFORE issuing the close.
    pub surface_open: bool,
    /// This document's transient success / error notices (DRAGON-353) — always
    /// PER-DOCUMENT, so a toast renders in the surface whose button produced it. See
    /// [`toast`]'s module doc.
    pub toasts: Toasts,
    // DRAGON-467 removed `save_in_place` from here. It recorded "the user chose this path",
    // which was the input that told `naming::save_target` to write straight back instead of
    // deriving a fresh `-edited` sibling. With the suffix gone and Save asking for a
    // destination every time, the same question is answered by whether `saved_path` is set at
    // all, so the flag had one writer and no readers left.
    /// The open-time automatic clipboard copy already ran for this document (DRAGON-353).
    /// The path can arrive later than the surface (a pre-opened spinner), so the copy is
    /// attempted at several seams; this makes it happen exactly once.
    pub copied_on_open: bool,
    /// `lab/flatpak`: the open-time automatic copy is DEFERRED, waiting for this document's
    /// window to take keyboard focus.
    ///
    /// Only ever set on the [`crate::share::CopyRoute::ThisWindow`] route, where the
    /// selection is served by our own focused surface: the copy is started while the surface
    /// is still an open task ("surface minted; open task queued, not yet created"), so there
    /// is no window to write through yet. Cleared by whichever of the two arrivals comes
    /// first — the focus, which writes and toasts the outcome, or the bounded
    /// [`AUTO_COPY_FOCUS_BUDGET`], which reports the failure honestly rather than claiming a
    /// copy that never happened. Always `false` on a data-control session, on macOS and on
    /// Windows, whose copies do not need us at all.
    pub auto_copy_waiting: bool,
    /// This document was DEMOTED out of the fullscreen overlay when a second document
    /// opened (DRAGON-336), and stays windowed for the rest of the session — even once
    /// its siblings close and it is alone again. Silently re-entering fullscreen as
    /// windows close would be jarring, so the pin is sticky; only the user's own
    /// appearance toggle clears it. Read by [`overlay_taken`]; set in
    /// [`App::demote_preview_to_window`], where the decision is documented.
    pub demoted: bool,
}


// DRAGON-371 — there is NO timed auto-close state here any more, and a new one needs a real
// reason. `PendingClose { started, hold }` used to hold a document open for
// `share::COPY_CLOSE_HOLD` (1s) after a close that copied or deleted, so the SUCCESS toast
// could be read; DRAGON-365 generalised it to carry its own duration for a closing fade that
// was then removed as unbuildable (see the note above `chrome::compose_preview`). Both are
// gone: the share now closes the instant its work is done. Keeping the editor up when
// something FAILED is a different mechanism entirely — the early returns in `share_now`
// (`share::copy_failure_aborts` and friends) — and it is unaffected.

/// The media-specific half of a preview. Images and videos diverge sharply (a video
/// grows into a timeline editor), so each owns its own state struct.
pub enum PreviewKind {
    /// A still screenshot.
    Image(ImagePreview),
    /// A recording — poster frame now, micro-editor later.
    Video(VideoPreview),
}

impl PreviewState {
    /// Record that this document's surface is (about to be) GONE while the document itself
    /// stays loaded — THE only way [`Self::surface_open`] is ever cleared (DRAGON-469).
    ///
    /// It is a named method rather than a bare `p.surface_open = false` so the invariant is
    /// greppable: `rg mark_surface_torn_down` lists every teardown in the codebase, and
    /// [`surface_closed`] reads the flag those calls set. The ordering that matters (clear
    /// BEFORE the destroy is issued, or our own echoed `Closed` reads as a lost surface and
    /// ends the process) is made structural by [`App::hide_preview_surface`], which mints the
    /// flag clear and the close task together; call that rather than this wherever a task is
    /// what you want.
    ///
    /// Idempotent, and safe on a document whose surface is already down.
    pub fn mark_surface_torn_down(&mut self) {
        self.surface_open = false;
    }

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

    /// Whether the scene holds work the file on disk does NOT (DRAGON-353 follow-up) —
    /// THE gate for the warning tint and the dirty-close dialog.
    ///
    /// Distinct from [`Self::dirty`] on purpose. `dirty()` asks "must an export re-encode?"
    /// and stays true after a save (the scene still has annotations in it); this asks "is
    /// there anything to LOSE?" and goes false the moment a save captures the current
    /// history position — then true again if the user undoes past it. The rule is
    /// [`edit::unsaved_at`].
    pub fn unsaved(&self) -> bool {
        edit::unsaved_at(self.edit.saved_depth, self.edit.undo_stack.len(), self.dirty())
    }

    /// THE PRISTINE MEDIA every bake composites the live scene onto (DRAGON-467 review,
    /// blocker 1): [`Self::bake_src`] once a still has been saved over its own source, and
    /// [`Self::path`] otherwise.
    ///
    /// Every bake reads THIS, never `path` directly and never the last save. The saved file
    /// already has the scene burned into it, so baking from it would double every annotation
    /// and re-apply a recording's cut to a file the cut has already been taken out of.
    pub fn bake_source(&self) -> Option<&std::path::Path> {
        self.bake_src.as_deref().or(self.path.as_deref())
    }

    /// Whether the video timeline has content DELETED (razor cuts alone leave
    /// the output identical, so they don't count).
    pub fn timeline_edited(&self) -> bool {
        match &self.kind {
            PreviewKind::Video(v) => v.timeline.as_ref().is_some_and(|t| t.edited()),
            PreviewKind::Image(_) => false,
        }
    }

    /// Whether the video timeline is holding a SEGMENT selection (DRAGON-468). It is a visible
    /// selection like any other — highlighted segments in the lane — so the staged Escape
    /// (`annotate::escape_stage`) counts it, and one press gives it up before the press that
    /// reaches the close decision. Always false for a still: there is no timeline to select in.
    pub fn timeline_selected(&self) -> bool {
        match &self.kind {
            PreviewKind::Video(v) => v.timeline.as_ref().is_some_and(|t| !t.selected.is_empty()),
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
            // The DISPLAY frame (DRAGON-385): the crop's size once applied, else the decoded
            // frame — so the windowed open fit and the overlay hug both size to the crop.
            PreviewKind::Image(_) => known(self.display_frame())
                .or_else(|| self.display_dims.and_then(known))
                .unwrap_or((0, 0)),
        }
    }

    /// [`Self::sizing_media`] converted from PHYSICAL pixels into the LOGICAL points
    /// the picture occupied on its SOURCE display — what the WINDOW preview opens to
    /// and re-fits against, so a high-DPI capture is shown at its true on-screen size
    /// rather than 2× (`source_scale` is the source display's backing scale, resolved on
    /// every platform — see [`PreviewState::source_scale`]). On an UNSCALED output
    /// `source_scale` is `1.0`, so this returns the physical dims unchanged and the
    /// open-fit math stays byte-identical.
    pub(super) fn sizing_media_points(&self) -> (u32, u32) {
        sizing::to_points(self.sizing_media(), self.source_scale)
    }

    /// The decoded frame (`edit.frame`, PHYSICAL capture pixels) in LOGICAL points —
    /// the media's true on-screen size. The DISPLAY fit caps at this (rule 2): a hidpi
    /// capture is never drawn larger than its natural size, even in a floored window
    /// whose canvas is bigger than the picture. `source_scale == 1.0` (an unscaled output)
    /// returns the physical dims unchanged, so the fit is byte-identical there.
    pub(super) fn frame_points(&self) -> (u32, u32) {
        sizing::to_points(self.edit.frame, self.source_scale)
    }

    /// The DISPLAY frame (DRAGON-385): the pixel dims the editor FRAMES to — the committed
    /// crop's size once a crop is applied, else the whole decoded frame. THE seam every sizing /
    /// fit / pan / canvas-mapping path reads, so "a crop to the bottom right shows only the
    /// bottom right" is honoured everywhere by one accessor rather than scattered `if let
    /// Some(crop)` branches.
    ///
    /// A crop is IGNORED while its session is live: the tool shows the whole image so the rect
    /// stays repositionable (the crop overlay owns the surface). So this is the full frame
    /// whenever `crop_session` is open, exactly reproducing the un-cropped framing for the
    /// duration of the edit. An un-cropped document returns [`EditState::frame`] unchanged —
    /// byte-identical to before.
    pub(super) fn display_frame(&self) -> (u32, u32) {
        match self.view_crop() {
            Some(c) => c.pixel_size(),
            None => self.edit.frame,
        }
    }

    /// The committed crop applied to the DISPLAY right now, or `None` (DRAGON-385): the crop when
    /// one is set AND no crop session is live (a session reveals the full image). The one place
    /// the "session hides the applied crop" rule lives.
    pub(super) fn view_crop(&self) -> Option<crop::CropRect> {
        // ONE definition (DRAGON-391): the model side reads the same rule for the covermark's
        // canvas, so "what is framed" can never diverge between the two.
        self.edit.view_crop()
    }

    /// The DISPLAY frame's top-left offset within the full source (SOURCE px) — the crop origin
    /// when a crop frames the view, else `(0, 0)`. Threaded into the annotation canvas coordinate
    /// map so full-source annotation coords place correctly against the cropped view.
    pub(super) fn display_offset(&self) -> (f32, f32) {
        match self.view_crop() {
            Some(c) => (c.x, c.y),
            None => (0.0, 0.0),
        }
    }

    /// [`Self::display_frame`] in LOGICAL points (the physical dims divided by the source backing
    /// scale) — the sizing input for the windowed open fit and the overlay hug, matching
    /// [`Self::frame_points`] for an un-cropped document.
    pub(super) fn display_frame_points(&self) -> (u32, u32) {
        sizing::to_points(self.display_frame(), self.source_scale)
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


// DRAGON-467 removed `copy_intent` and `delete_intent` from here. They fanned the
// "Automatically save on copy" / "Automatically close on copy" / "Automatically copy to
// clipboard on delete" settings out into the compound `ShareIntent` flavours that are also
// gone. There is one action left to run (a copy) and no setting left to consult, so the
// toolbar messages name it directly through `App::run_copy`.

/// One media kind's three preview-editor settings — the triple every editor decision reads
/// (DRAGON-420's split, re-pointed at DRAGON-467's rows).
///
/// A named triple rather than three loose bools at the call site: the whole point of the
/// feature is that there are TWO of these and a document must read exactly one of them, so
/// passing them as a unit is what makes "the video document accidentally read an image field"
/// a thing the type system can be asked about instead of a typo nobody notices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PreviewAutomation {
    /// Put the edited result on the clipboard as the editor closes.
    pub copy_on_exit: bool,
    /// Write the capture into the user's configured folder as it is taken (off = the session
    /// runtime directory until the user saves).
    pub save_originals: bool,
    /// Ask before closing over edits that were never saved.
    pub ask_to_save: bool,
}

/// WHICH triple a document reads: the Video Editor group's for a recording, the Image
/// Editor group's for a still (DRAGON-420) — the ONE place the fork lives.
///
/// Pure and total, so the "these settings are independent" property is directly testable:
/// for every combination, flipping a field of one triple can never move the other's answer.
pub(super) fn preview_automation(
    is_video: bool,
    image: PreviewAutomation,
    video: PreviewAutomation,
) -> PreviewAutomation {
    if is_video { video } else { image }
}

/// THE dirty-close gate (DRAGON-353, settings-gated since DRAGON-467): should a close attempt
/// raise the unsaved-changes dialog instead of closing?
///
/// Three terms, and each removes a different way of getting it wrong:
///
/// * `ask` is the "Ask to save edited screenshots" / "…videos" setting for this document's
///   media kind. OFF means close straight away and let the un-baked edits go, which is a
///   coherent choice next to "Automatically copy changes on exit" — the edited result still
///   reaches the clipboard, it just never reaches the disk.
/// * `dirty` is `PreviewState::unsaved`: there is something to lose. A clean document always
///   closes straight away, whatever the setting says, because the file on disk already IS
///   what the editor is showing.
/// * `already_confirming` is what makes the dialog's own buttons work: they re-enter the very
///   close paths this guards, so without it a close could bounce off its own dialog forever.
///
/// Pure; unit-tested below.
pub(super) fn close_needs_confirmation(ask: bool, dirty: bool, already_confirming: bool) -> bool {
    ask && dirty && !already_confirming
}

/// Whether two paths name the SAME file on disk, resolving symlinks and `..` so a save over
/// the source is recognised however the two were spelled.
///
/// Non-existent is not "the same": a destination the user is about to create cannot be the
/// source, and `canonicalize` fails on it, so the `zip` yields `None` and the answer is
/// false. That is the wanted behaviour for every ordinary Save As.
pub(super) fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    std::fs::canonicalize(a)
        .ok()
        .zip(std::fs::canonicalize(b).ok())
        .is_some_and(|(a, b)| a == b)
}

/// Where a recording renders while it is replacing itself: a hidden sibling of `dest`
/// (DRAGON-467 review, blocker 1).
///
/// Beside the destination rather than in a temp dir, so the rename that swaps it in stays
/// within one filesystem and is therefore atomic. A cross-device rename would fall back to a
/// full copy, which is exactly the multi-GB cost this whole arm exists to avoid.
pub(super) fn bake_temp_path(dest: &std::path::Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "capture".to_string());
    dest.with_file_name(format!(".{name}.cck-saving"))
}

/// Copy a still's PRISTINE bytes into the session runtime directory and return the snapshot,
/// so a save over the capture's own file cannot cost the document its editable source
/// (DRAGON-467 review, blocker 1). `None` when the copy failed, which REFUSES the save.
///
/// The name is fixed per document rather than unique: one snapshot is all a document can
/// need (the first in-place save takes it, and the bake source stays pointed at it
/// thereafter), and a fixed name keeps the runtime directory bounded.
fn snapshot_bake_source(src: &std::path::Path) -> Option<PathBuf> {
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or(naming::PNG_EXT);
    let dst = PathBuf::from(crate::util::runtime_dir()).join(format!("cck-source.{ext}"));
    match std::fs::copy(src, &dst) {
        Ok(_) => {
            log::debug!("preview: snapshotted the pristine source before an in-place save");
            Some(dst)
        }
        Err(e) => {
            log::warn!("preview: could not snapshot the source before an in-place save: {e}");
            None
        }
    }
}

/// THE exit-copy gate (DRAGON-467): as this document closes, should its edited state go on
/// the clipboard?
///
/// Three terms, and each removes a different piece of pointless work:
///
/// * `copy_on_exit` is the setting for this document's media kind.
/// * `dirty` is not an optimisation: the untouched capture went on the clipboard when it was
///   taken, so a clean document's exit copy would re-copy identical bytes and (on Linux) hand
///   the selection to a fresh worker process for no reason at all.
/// * `clipboard_current` ([`edit::clipboard_is_current`]) is the DRAGON-467 review's major 4:
///   an explicit toolbar Copy followed straight away by Escape must not copy twice. The
///   clipboard already holds exactly this state.
///
/// Pure; unit-tested below.
pub(super) fn exit_copies_changes(
    copy_on_exit: bool,
    dirty: bool,
    clipboard_current: bool,
) -> bool {
    copy_on_exit && dirty && !clipboard_current
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

/// What an iced `window::Event::Closed` for a STILL-REGISTERED preview document means
/// (DRAGON-469). See [`surface_closed`] for the rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::app) enum SurfaceClosed {
    /// WE tore this surface down on purpose and kept the document loaded, so the event is
    /// the echo of our own request. Ignore it entirely.
    Ours,
    /// A bake / Save As export is mid-write. The document must close, but only once the
    /// worker is done (`close_after_bake`).
    DeferToBake,
    /// The surface went away WITHOUT routing through the editor — the window manager or
    /// compositor took it. Close the document (and, if it was the last one, the process).
    LostOutOfBand,
}

/// Pure, unit-tested: what to do when a preview surface reports `Closed` while its document
/// is still registered.
///
/// **Why this is a decision and not an `if` at the call site (DRAGON-469).** Off Linux a
/// preview surface is a real winit window, and `window::close` pushes
/// `window::Event::Closed` for OUR OWN teardown — the runtime cannot tell us apart from the
/// WM. On Linux the overlay is a layer surface, whose destroy reports a Wayland
/// `LayerEvent::Done` that this app never subscribes to, so the same teardown is silent
/// there. That asymmetry is what made the Save As dialog fatal on Windows 11:
/// [`App::save_as_dialog`] tears the fullscreen overlay down (a file chooser cannot render
/// over it), keeps the document loaded for [`App::reopen_preview_surface`], and the echoed
/// `Closed` was read as "the surface was lost" — so the document closed, it was the last
/// one, and `finish_session` ENDED THE PROCESS while the chooser was still on screen. The
/// user never got back to the editor and the export never ran.
///
/// `surface_open` is the document's own record of whether `window` is a LIVE surface, and it
/// is cleared BEFORE the destroy is issued by every path that keeps the document. It
/// therefore dominates: a cleared flag means the teardown was ours, whatever else is going
/// on — including a bake, whose `close_after_bake` must not be armed for a surface we
/// intend to re-mint.
///
/// The re-minting paths (the appearance toggle, the demotion, the cover→window swap) do not
/// arrive here at all: they re-point the document onto the new surface in the same pass, so
/// the dead id is no longer any document's `window`.
pub(in crate::app) fn surface_closed(surface_open: bool, baking: bool) -> SurfaceClosed {
    if !surface_open {
        SurfaceClosed::Ours
    } else if baking {
        SurfaceClosed::DeferToBake
    } else {
        SurfaceClosed::LostOutOfBand
    }
}

/// Pure, unit-tested: the `preview_windowed` value the appearance toggle must adopt, given
/// the surface that is ACTUALLY OPEN (DRAGON-469).
///
/// The toggle used to INVERT the persisted setting, which is the same answer only while the
/// setting and the open surface agree. Reading the OPEN surface instead makes the button mean
/// what it says: whatever is up, mint the other one.
///
/// **Every state where the two disagree**, because "only a demotion" was the first version of
/// this note and it was wrong:
///
/// * **A FORCED demotion**, setting says overlay while a WINDOW is open.
///   [`App::demote_preview_to_window`] deliberately leaves the appearance alone (the demotion
///   is forced, not chosen). Inverting there asked for a window, so the open window was torn
///   down and an identical one minted: the ticket's "clicking the button just seems to reload
///   our window". Two demotions produce this state and they are NOT equally reachable. The
///   Settings-from-preview one ([`App::open_settings_from_preview`]) reaches the toggle
///   immediately, and is the live trigger. The DRAGON-336 second-document one is blocked
///   WHILE the sibling is open, by the early return above `toggle_preview_appearance`'s flip
///   — but that early return asks whether a sibling is open NOW, so once the sibling closes
///   the survivor is alone, still demoted, and reaches the bug too.
/// * **A window-pick PRE-OPEN cover**, setting says window while an OVERLAY is open. All
///   three platforms force the overlay branch for the cover that hides the grab (Linux
///   `window_spinner_neutral`, macOS `mac_preview_preopen`, Windows `win_preview_preopen` —
///   see `preview_surface_for`). No toggle can be pressed there today, because the cover
///   renders the loading view and that view carries no appearance chrome. It is listed
///   because the new answer is also the SAFE one if that ever changes: it asks for the window
///   the cover is about to be swapped for anyway, and re-persists the setting's existing
///   value, where the old inversion would have silently written "overlay" as the user's
///   default off the back of a transient cover.
///
/// Byte-identical whenever the setting and the surface agree, which is every other state.
pub(in crate::app) fn toggled_preview_windowed(open: PreviewSurface) -> bool {
    !open.is_window()
}

// DRAGON-469 put an `overlay_promotion_blocked(to_overlay, settings_open)` refusal here: the
// appearance toggle would not promote a document back to the fullscreen overlay while a
// settings pane was open, and toasted "Close the settings window to go fullscreen" instead.
// DRAGON-488 DELETED it, on the owner's report. The premise was that the two cannot coexist,
// and that is only half true: they coexist fine, the overlay simply sits ON TOP (and, on
// Linux, holds the keyboard — the DRAGON-109 `Exclusive` hazard). What actually matters is
// that nobody gets LOCKED OUT of settings, and that is guaranteed from the other side: every
// route to settings from the editor demotes the document off the overlay first (see
// [`settings_activation`] and [`App::open_settings_from_preview`]). So the way back to a
// buried pane is the same gear that opened it, and a refusal buys nothing but a red toast on
// a button the user just pressed on purpose. Do not re-add it; fix the demotion instead if
// this ever regresses.

/// Pure, unit-tested: is this document ACTUALLY sitting on the fullscreen overlay, so
/// bringing it down to a window is a real teardown-and-re-mint rather than a no-op?
///
/// ONE expression behind every reader of that rule: [`overlay_siblings`]'s selection (the
/// DRAGON-336 second-document sweep) and [`App::demote_preview_to_window`]'s own guard, which
/// is what the settings path ([`App::open_settings_from_preview`]) leans on. They each spelt
/// it out separately, which is one edit away from disagreeing about what "on the overlay"
/// means.
///
/// `surface_open` is load-bearing, not defensive: a document whose surface was torn down
/// while it stays loaded (a background bake, the overlay's Save-As chooser) has NOTHING on
/// screen to demote, and minting a window for it would resurrect a surface that was closed on
/// purpose. It comes back as a window later through [`App::reopen_preview_surface`], which
/// consults [`overlay_taken`].
pub(super) fn overlay_demotion_needed(surface: PreviewSurface, surface_open: bool) -> bool {
    !surface.is_window() && surface_open
}

/// What activating Settings from the preview editor must do about the PANE itself: DRAGON-353's
/// three cases, named as a type in DRAGON-488 so the demotion could be lifted out of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::app) enum SettingsActivation {
    /// THIS process already owns a pane: focus that window and spawn nothing.
    FocusOwn,
    /// A pane is open in ANOTHER instance: spawn the `--focus-settings` helper, so the
    /// activation outlives our own spawn.
    FocusOther,
    /// No pane anywhere: spawn a `--settings` child.
    Spawn,
}

/// Pure, unit-tested: which of the three [`SettingsActivation`] cases a Settings press in the
/// preview editor lands in. `own_pane` DOMINATES: a pane we own would also answer the
/// cross-process probe (a `flock` is not re-takeable from a second descriptor in the same
/// process), and focusing the window we already have beats spawning a helper to poke it.
///
/// **The DEMOTION is deliberately NOT part of this answer (DRAGON-488).** Every arm demotes,
/// so the caller issues it once, unconditionally, before it looks at the pane at all. Folding
/// it in as a `bool` would invite an arm that quietly skips it, and skipping it is exactly the
/// bug this ticket closed: the `FocusOwn` arm used to return early, leaving a same-process
/// pane focused UNDERNEATH a fullscreen overlay that owns the keyboard. Demoting first is also
/// what lets the promotion the other way be allowed at all (see the deleted-refusal note
/// above): the gear is always a way back out of fullscreen, so fullscreen is never a trap.
pub(in crate::app) fn settings_activation(
    own_pane: bool,
    pane_elsewhere: bool,
) -> SettingsActivation {
    match (own_pane, pane_elsewhere) {
        (true, _) => SettingsActivation::FocusOwn,
        (false, true) => SettingsActivation::FocusOther,
        (false, false) => SettingsActivation::Spawn,
    }
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
        .filter(|p| {
            overlay_demotion_needed(p.surface, p.surface_open) && Some(p.window) != minting
        })
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

    /// The account id the Upload flyout's current highlight names, for this document
    /// (DRAGON-493): the panel's own keyboard highlight when the flyout is open, falling back
    /// to the SAME preselection rule the flyout opened with. Shared by [`PreviewMsg::UploadStart`]
    /// and the visibility handler, so both panel actions can never disagree
    /// about which account they are acting on.
    fn upload_selected_account_id(&self, id: window::Id) -> Option<String> {
        let p = self.preview_for(id)?;
        let by_highlight =
            p.edit.flyout.filter(|f| f.kind == edit::FlyoutKind::Upload).and_then(|f| f.selected);
        let idx = by_highlight.or_else(|| {
            edit::upload_preselect(&p.edit.cloud_accounts, self.cloud_last_account.as_deref())
        })?;
        p.edit.cloud_accounts.get(idx).map(|a| a.id.clone())
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

    /// A window took focus: if it is a preview, re-read the connected cloud accounts into
    /// its snapshot (DRAGON-482).
    ///
    /// This is what un-greys the toolbar's Upload button after the user connects an account
    /// in Settings. The button reads `EditState::cloud_accounts`, a snapshot, because the
    /// toolbar is rebuilt every frame and `accounts::list` is a file read plus a TOML parse.
    /// Coming BACK to the editor is the moment the answer can have changed and the only one
    /// the app can see without polling, so it joins the document's open and the Upload
    /// flyout's toggle as the third (and last) refresh point.
    ///
    /// Cheap by construction: it does nothing at all for the settings window, an overlay, or
    /// any other surface, and a preview is focused a handful of times in a session.
    pub(in crate::app) fn refresh_preview_cloud_accounts(&mut self, id: window::Id) {
        let Some(is_video) = self.preview_for(id).map(|p| matches!(p.kind, PreviewKind::Video(_)))
        else {
            return;
        };
        self.note_preview_focus(id);
        // Filtered to this document's own media kind (DRAGON-493): a video-only provider
        // (YouTube) has no business in an image session's list, and vice versa.
        let accounts = edit::accounts_for_kind(crate::cloud::accounts::list(), is_video);
        if let Some(preview) = self.preview_for_mut(id) {
            preview.edit.cloud_accounts = accounts;
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


    /// This document's editor settings, chosen by MEDIA KIND (DRAGON-420, carried into
    /// DRAGON-467): a recording reads the Video Editor group, a still reads the Image Editor
    /// group. A document that has already vanished falls back to the image triple — no action
    /// can run against it anyway, and the arms below re-check for its existence.
    pub(super) fn preview_automation(&self, id: window::Id) -> PreviewAutomation {
        let image = PreviewAutomation {
            copy_on_exit: self.preview_copy_on_exit,
            save_originals: self.preview_save_originals,
            ask_to_save: self.preview_ask_to_save,
        };
        let video = PreviewAutomation {
            copy_on_exit: self.preview_video_copy_on_exit,
            save_originals: self.preview_video_save_originals,
            ask_to_save: self.preview_video_ask_to_save,
        };
        let is_video = self
            .preview_for(id)
            .is_some_and(|p| matches!(p.kind, PreviewKind::Video(_)));
        preview_automation(is_video, image, video)
    }

    /// Give up EVERYTHING the editor is holding (DRAGON-468): the annotation selection, settling
    /// a live text edit on the way, plus the video timeline's segment selection. THE action
    /// behind [`annotate::EscapeStage::Deselect`], so the two Escape lanes that can produce it
    /// (the live text edit, then the selection lane) can never disagree about what "deselect
    /// everything" covers.
    ///
    /// It COMPOSES the two existing per-domain deselect messages rather than reaching into state:
    /// `SelectAnnotation(None)` is the canvas's own deselect (and already settles a live edit
    /// before it clears), `TimelineSelect(None, false, false)` is the timeline's own click-away
    /// deselect, a no-op on a still. No new message and no second deselect route.
    ///
    /// Ctrl+D ([`crate::shortcuts::Action::PreviewDeselectAll`]) deliberately stays
    /// annotation-only for now: it is an annotation-tray binding, and widening it is a
    /// behaviour change this ticket was not asked to make.
    pub(super) fn deselect_everything(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let annotations = self.update_preview(id, PreviewMsg::SelectAnnotation(None));
        let timeline = self.update_preview(id, PreviewMsg::TimelineSelect(None, false, false));
        Task::batch([annotations, timeline])
    }

    pub(super) fn update_preview(
        &mut self,
        id: window::Id,
        message: PreviewMsg,
    ) -> Task<cosmic::Action<Msg>> {
        // A bake (or a Save As export — same single-flight `baking` guard, DRAGON-352)
        // is committing the edits to disk: hold every input except its own completion
        // so the file can't be shared/deleted mid-rewrite.
        if self.preview_for(id).is_some_and(|p| p.edit.baking)
            && !matches!(message, PreviewMsg::BakeDone(_) | PreviewMsg::SaveAsBaked(_))
        {
            return Task::none();
        }
        // (DRAGON-371: there used to be a second guard here, dropping every input but its own
        // tick while a document sat in its 1s close-after-copy hold — the window in which a
        // second action could double-close or act on files that were already unlinked. The
        // hold is gone, and with it that window: the close is issued synchronously in
        // `share_now`, so no message can arrive between the decision and the surface's death.)
        // Hands-on with the document ⇒ its toasts get out of the way early (DRAGON-353
        // follow-up). Applied BEFORE the handler runs, so a toast the handler goes on to
        // post always starts with the full TTL and an action can never cancel its own
        // confirmation. Per document by construction: `id` selects exactly one.
        if message.is_document_interaction()
            && let Some(p) = self.preview_for_mut(id)
        {
            p.toasts.shorten_to(toast::TOAST_INTERACTION_TTL, std::time::Instant::now());
        }
        match message {
            PreviewMsg::ImageReady(handle, original) => {
                // DRAGON-454: back on the UI thread with the decoded pixels. The body below
                // COPIES them again for the effects shader, which on a 5K capture is tens of
                // megabytes of memcpy in the update loop — worth its own pair of marks.
                crate::util::timing_mark("preview: ImageReady on the UI thread (begin)");
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
                crate::util::timing_mark(
                    "preview: ImageReady stored (fx_base copied; the media is now loaded)",
                );
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
                        // LOGICAL POINTS (DRAGON-449), the same space the OPEN fit measured in
                        // and the same space `target` below is in. Leaving this bound in CAPTURE
                        // space would make the re-fit disagree with the size the window just
                        // opened at on a scaled display, and "fix" it every time.
                        let out = self
                            .preview_output
                            .as_ref()
                            .map(|(n, o)| monitor_fit_points(*o, monitor_point_scale(Some(n))));
                        // Windows (DRAGON-288): an external `--preview` has no capture anchor,
                        // so `out` is None and the shared fit below would native-size the
                        // window (spilling a large picture off-screen). Fall back to the
                        // preview window's LIVE monitor so the media is bounded to it, exactly
                        // like the open fit — additive, Linux/mac keep `out` unchanged.
                        #[cfg(windows)]
                        let out = out.or_else(|| {
                            crate::platform::windows::window::preview_window_monitor_points(
                                super::shell::PREVIEW_WINDOW_TITLE,
                            )
                        });
                        // Logical (backing-scale-divided) size, so a hidpi capture
                        // re-fits to its true on-screen size (rule 6).
                        let target = p.sizing_media_points();
                        let want = windowed_fit_size(
                            target,
                            out,
                            transport_h_for(&p.kind, p.surface),
                            self.preview_toolbar_labels,
                        );
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
            PreviewMsg::AutoCopied(ok) => {
                // The open-time copy's worker finished (DRAGON-454). Its whole visible effect
                // is this toast — which is exactly why the write itself no longer sits on the
                // UI thread. A document that closed while the copy was in flight simply has no
                // toast queue left, and `preview_toast_icon` is already a no-op for one.
                crate::util::timing_mark("preview: auto-copy outcome landed (toast posted)");
                self.toast_copy_outcome(id, ok);
                Task::none()
            }
            PreviewMsg::AutoCopyDeadline => {
                // `lab/flatpak`: the bounded wait for this document's window to take focus
                // (`AUTO_COPY_FOCUS_BUDGET`). Normally the focus arrives long first and
                // `flush_deferred_auto_copy` has already cleared the latch, so this is a
                // no-op — that is the common path, not the exception.
                let waiting = self.preview_for(id).is_some_and(|p| p.auto_copy_waiting);
                if !waiting {
                    return Task::none();
                }
                if let Some(p) = self.preview_for_mut(id) {
                    p.auto_copy_waiting = false;
                }
                // Nothing was written. Say so — in the log for us, and in the toast for the
                // user — rather than claiming a copy that never happened. The capture itself
                // is on disk; only the courtesy copy is lost, and the Copy button still works
                // the moment the editor has focus.
                log::warn!(
                    "clipboard: the open-time copy waited {}s for the editor window to take \
                     focus and it never did, so this session's window-served selection could \
                     not be written",
                    share::AUTO_COPY_FOCUS_BUDGET.as_secs(),
                );
                self.toast_copy_outcome(id, false);
                Task::none()
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
                    Some(edit::FlyoutNav { kind: edit::FlyoutKind::TextSize, selected: Some(i), .. }) => {
                        match text_annot::TEXT_SIZES.get(i) {
                            Some(&s) => self.update_preview(id, PreviewMsg::SetTextSize(s)),
                            None => Task::none(),
                        }
                    }
                    Some(edit::FlyoutNav { kind: edit::FlyoutKind::TextFont, selected: Some(i), .. }) => {
                        let f = if i == 0 {
                            text_annot::TextFont::Hand
                        } else {
                            text_annot::TextFont::Clean
                        };
                        self.update_preview(id, PreviewMsg::SetTextFont(f))
                    }
                    // DRAGON-482: Enter PICKS the highlighted account, exactly as it picks a
                    // covermark or a text size. It deliberately does not start the upload:
                    // see `FlyoutKind::Upload`; the panel's own button is the commit.
                    Some(edit::FlyoutNav { kind: edit::FlyoutKind::Upload, selected: Some(i), .. }) => {
                        match self
                            .preview_for(id)
                            .and_then(|p| p.edit.cloud_accounts.get(i).map(|a| a.id.clone()))
                        {
                            Some(account) => {
                                self.update_preview(id, PreviewMsg::UploadAccountSelected(account))
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
                    // Scroll snaps in VALUE space: its steps are multiplicative and fine, so
                    // there is no rail geometry to measure against (DRAGON-400).
                    let z1 = viewport::snap_to_hundred(
                        (z0 * (1.0 + 0.12 * step)).clamp(minz, maxz),
                        visual,
                        viewport::SNAP_SCROLL_PCT,
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
                Task::batch([self.refresh_covermark_for_view(id), self.refresh_text_for_zoom(id)])
            }
            PreviewMsg::SetViewZoom(z) => {
                let maxz = self.preview_for(id).map(|p| self.max_view_zoom(p)).unwrap_or(Viewport::MAX);
                let minz = self.preview_for(id).map(|p| self.min_view_zoom(p)).unwrap_or(Viewport::MIN);
                let visual = self.preview_for(id).map(|p| self.preview_visual_scale(p)).unwrap_or(1.0);
                // The slider snaps in RAIL space (DRAGON-400): the detent is a fixed number of
                // rail PIXELS wide, so raising the ceiling to 500% cannot shrink it below what a
                // drag can land on. Same rail geometry the control is built from — the range in
                // displayed percent, over the shared toolbar rail width.
                let tol = viewport::rail_snap_pct(
                    viewport::displayed_percent(minz, visual) as f32,
                    viewport::displayed_percent(maxz, visual) as f32,
                    chrome::ZOOM_SLIDER_W,
                );
                if let Some(p) = self.preview_for_mut(id) {
                    // Clamp to the 50%-display floor, then magnetically snap to exactly 100%.
                    let snapped = viewport::snap_to_hundred(z.clamp(minz, maxz), visual, tol);
                    p.view.set_zoom(snapped);
                    let z100 = viewport::preset_zoom(Some(1.0), visual);
                    p.view.zoom_preset = if (p.view.zoom - z100).abs() < 1e-3 { Some(1) } else { None };
                }
                Task::batch([self.refresh_covermark_for_view(id), self.refresh_text_for_zoom(id)])
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
                // DRAGON-401: `Viewport::set_zoom` only knows the BACKSTOP (`Viewport::MAX`),
                // so a preset was the one zoom path that could step past the device's viewport
                // limit and take the process down. Clamp it to the same ceiling the slider and
                // the wheel already respect. (The menu below no longer OFFERS an unreachable
                // preset, so this normally clamps nothing — it is what makes that a display
                // detail rather than the only thing standing between a click and a panic.)
                let maxz = self.preview_for(id).map(|p| self.max_view_zoom(p)).unwrap_or(Viewport::MAX);
                if let Some(p) = self.preview_for_mut(id) {
                    p.view.zoom_menu_open = false;
                    // Only real preset indices change the zoom (the combo also lists the
                    // current % as a synthetic trailing entry — selecting it is a no-op).
                    if let Some(visual_frac) = ZOOM_PRESET_VISUAL.get(i).copied() {
                        // visual fraction (1.0 = 100% = natural size): displayed =
                        // zoom*visual_scale → zoom = frac/visual_scale.
                        p.view.set_zoom(viewport::preset_zoom(visual_frac, visual).min(maxz));
                        p.view.zoom_preset = Some(i);
                    }
                }
                Task::batch([self.refresh_covermark_for_view(id), self.refresh_text_for_zoom(id)])
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
                    // restored model on the next view build (DRAGON-330). TEXT is a raster layer,
                    // so it must be re-rendered from the restored model; any in-flight edit ends.
                    Some(EditKind::Annotations) => {
                        if let Some(p) = self.preview_for_mut(id) {
                            p.edit.text_edit = None;
                        }
                        self.refresh_text_display(id)
                    }
                    // A dim change re-renders via the GPU dim pass for free (DRAGON-329).
                    Some(EditKind::Dim) => Task::none(),
                    // A crop change (DRAGON-385) reframes the view to the restored crop's framing
                    // so undo/redo shows the cropped (or whole) picture, not a stale pan/zoom —
                    // and moves the covermark's canvas with it (DRAGON-391).
                    Some(EditKind::Crop) => {
                        self.crop_reframe(id);
                        self.refresh_covermark_for_view(id)
                    }
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
                    // TEXT is a raster layer, re-rendered from the restored model; any in-flight
                    // edit ends. Box/arrow redraw as vectors; effects re-render via the GPU shader.
                    Some(EditKind::Annotations) => {
                        if let Some(p) = self.preview_for_mut(id) {
                            p.edit.text_edit = None;
                        }
                        self.refresh_text_display(id)
                    }
                    // A dim change re-renders via the GPU dim pass for free (DRAGON-329).
                    Some(EditKind::Dim) => Task::none(),
                    // A crop change (DRAGON-385) reframes the view to the restored crop's framing —
                    // and moves the covermark's canvas with it (DRAGON-391).
                    Some(EditKind::Crop) => {
                        self.crop_reframe(id);
                        self.refresh_covermark_for_view(id)
                    }
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
            PreviewMsg::CycleToolSlot(slot) => {
                self.cycle_tool_slot(id, slot);
                Task::none()
            }
            PreviewMsg::SetAnnotColor(color) => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.annot_color = Some(color);
                    // A direct color pick breaks any pending companion-swap pair (DRAGON-386):
                    // the next X operates on THIS color, not a stale remembered partner. The X
                    // path re-arms `color_swap_back` right AFTER this handler runs.
                    p.edit.color_swap_back = None;
                    p.edit.close_flyout();
                }
                // Picking a color also recolors the SELECTED colorable item immediately (one
                // undo entry) — same effect as the right-click "Set to current color".
                self.recolor_selected_annotation(id, color);
                // Persist so the next preview opens with this color.
                self.annot_color = Some(color);
                self.save_state();
                // Recoloring a highlight re-renders through the GPU shader on the next view
                // build (DRAGON-330); a recolored text box re-renders its raster layer (DRAGON-354).
                self.refresh_text_display(id)
            }
            PreviewMsg::SetAnnotStrokeW(w) => {
                self.apply_annot_stroke_w(id, w);
                // A restroked TEXT box re-renders its raster layer (its width is the glyph
                // outline weight, DRAGON-358); box/arrow/badge redraw as vectors, so this is a
                // no-op when nothing text is selected.
                self.refresh_text_display(id)
            }
            PreviewMsg::CycleAnnotStrokeW => {
                // The `L` hotkey: advance to the next width preset (1 → 2 → 4 → 6 → 8 → 10 → 12 →
                // 1), applying to the selection + persisting, exactly like clicking the next
                // segment.
                let current = self.preview_for(id)
                    .map(|p| p.edit.stroke())
                    .unwrap_or(annotate::DEFAULT_ANNOT_STROKE);
                self.apply_annot_stroke_w(id, annotate::cycle_stroke_width(current));
                // As above: a restroked text box needs its raster refreshed (DRAGON-358).
                self.refresh_text_display(id)
            }
            PreviewMsg::ToggleTextSizeFlyout => {
                // Toggle the TEXT-size dropdown (DRAGON-354), highlighting the current size.
                let current = self
                    .preview_for(id)
                    .map(|p| p.edit.text_size())
                    .unwrap_or(text_annot::DEFAULT_TEXT_SIZE);
                if let Some(p) = self.preview_for_mut(id) {
                    if p.edit.flyout_kind() == Some(edit::FlyoutKind::TextSize) {
                        p.edit.close_flyout();
                    } else {
                        // DRAGON-367/368: only an ON-preset size highlights a row. Handle scaling
                        // is CONTINUOUS and reaches past both ends of the listed range, so an
                        // off-preset size is now the usual case rather than the exception —
                        // highlighting the nearest row for a 192px box would claim it is 128px
                        // while the chip beside it reads 192px.
                        let sel = text_annot::text_size_preset_index(current);
                        p.edit.picker = None;
                        p.edit.open_flyout(
                            edit::FlyoutKind::TextSize,
                            sel,
                            text_annot::TEXT_SIZES.len(),
                        );
                    }
                }
                Task::none()
            }
            PreviewMsg::ToggleTextFontFlyout => {
                // Toggle the TEXT-font dropdown (DRAGON-357 item 16), highlighting the current
                // family (Hand = index 0, Clean = index 1).
                let current = self.preview_for(id).map(|p| p.edit.annot_text_font);
                if let Some(p) = self.preview_for_mut(id) {
                    if p.edit.flyout_kind() == Some(edit::FlyoutKind::TextFont) {
                        p.edit.close_flyout();
                    } else {
                        let sel = current
                            .map(|f| if f == text_annot::TextFont::Hand { 0 } else { 1 });
                        p.edit.picker = None;
                        p.edit.open_flyout(edit::FlyoutKind::TextFont, sel, 2);
                    }
                }
                Task::none()
            }
            PreviewMsg::SetTextSize(size) => self.set_text_size(id, size),
            PreviewMsg::SetTextFont(font) => self.set_text_font(id, font),
            PreviewMsg::EditText(annot) => self.edit_existing_text(id, annot),
            PreviewMsg::TextCaretBlink => {
                self.text_caret_blink(id);
                Task::none()
            }
            PreviewMsg::TextClickAt { x, y, extend, word, all } => {
                self.text_click_at(id, x, y, extend, word, all)
            }
            PreviewMsg::TextDragTo(x, y) => self.text_drag_to(id, x, y),
            PreviewMsg::TextImeCommit(s) => self.text_edit_ime_commit(id, s),
            // DRAGON-572: the window-route paste's delivery. It inserts through the
            // IME-commit lane — the same normalize + cap + replace-selection + own-undo-step
            // the worker-read paste does inline — so the two routes cannot drift. `None`
            // (no text on the clipboard) inserts nothing, exactly like that path.
            PreviewMsg::TextPasted(t) => match t {
                Some(s) => self.text_edit_ime_commit(id, s),
                None => Task::none(),
            },
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
            PreviewMsg::AnnotColorCompanionSwap => {
                // Photoshop's X: swap the ACTIVE annotation color to its companion (its
                // complement), and swap back on the next press (DRAGON-386). Reuse the flyout's
                // SetAnnotColor path VERBATIM so this recolors any selection + persists exactly
                // like picking the companion swatch would; SetAnnotColor clears `color_swap_back`,
                // so we re-arm the exact swap-back partner AFTER it runs (double-X returns to the
                // starting color even where the complement round-trip rounds). This action never
                // fires during a live text edit or crop session — both own the keyboard ahead of
                // the keymap (see `preview_modal_key`).
                let Some((target, remember)) = self.preview_for(id).map(|p| {
                    let current =
                        p.edit.annot_color.unwrap_or_else(annotate::default_annot_color);
                    annotate::companion_swap(current, p.edit.color_swap_back)
                }) else {
                    return Task::none();
                };
                let task = self.update_preview(id, PreviewMsg::SetAnnotColor(target));
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.color_swap_back = Some(remember);
                }
                task
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
                }
                match picked {
                    Some(c) => self.apply_custom_annot_color(id, c),
                    // A recolored highlight re-renders through the GPU shader (DRAGON-330); a
                    // recolored text box re-renders its raster layer (DRAGON-354).
                    None => self.refresh_text_display(id),
                }
            }
            PreviewMsg::SelectAnnotation(annot) => {
                // Clicking AWAY from an edited text box settles it first (DRAGON-354) — unless
                // the click is on the very box being edited.
                let editing_other = self
                    .preview_for(id)
                    .and_then(|p| p.edit.text_edit.as_ref().map(|t| t.id))
                    .is_some_and(|eid| annot != Some(eid));
                if editing_other {
                    let _ = self.settle_text_edit(id);
                }
                if let Some(p) = self.preview_for_mut(id) {
                    match annot {
                        Some(annot) => p.edit.sel.set_one(annot),
                        None => p.edit.sel.clear(),
                    }
                    p.edit.annot_menu = None;
                }
                // The font/size dropdowns follow the newly selected text box (DRAGON-364 task 3).
                // DISPLAY only — selecting is not a preference change, so the persisted default
                // is untouched (see `annotate.rs`'s display-vs-remember comment).
                self.sync_text_style_to_selection(id, annotate::TextStyleSource::SelectionSync);
                Task::none()
            }
            PreviewMsg::ToggleAnnotationSelected(annot) => {
                // A shift-click that toggles a DIFFERENT annotation settles a live text edit
                // first (DRAGON-356), mirroring the click-away settle in SelectAnnotation — the
                // box being edited is never the one toggled here (the canvas leaves an in-box
                // shift-press to the text editor), so settling can't drop the toggled item.
                let editing_other = self
                    .preview_for(id)
                    .and_then(|p| p.edit.text_edit.as_ref().map(|t| t.id))
                    .is_some_and(|eid| eid != annot);
                let task = if editing_other {
                    self.settle_text_edit(id)
                } else {
                    Task::none()
                };
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.sel.toggle(annot);
                    p.edit.annot_menu = None;
                }
                // The dropdowns follow the new primary — with a multi-selection that is the
                // LAST-toggled item (DRAGON-364 task 3). DISPLAY only.
                self.sync_text_style_to_selection(id, annotate::TextStyleSource::SelectionSync);
                task
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
            PreviewMsg::AnnotGestureTo(x, y, scale_type) => {
                self.annot_gesture_to(id, x, y, scale_type)
            }
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
                // Recoloring a highlight re-renders through the GPU shader (DRAGON-330); a
                // recolored text box re-renders its raster layer (DRAGON-354).
                self.refresh_text_display(id)
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
            // ── Crop tool (DRAGON-382; IMAGES only) ──────────────────────────────────
            PreviewMsg::CropEnter => {
                // The single toolbar icon both OPENS and CONFIRMS: a press while a session is
                // already live accepts it, so the icon toggles the tool on/off.
                if self.preview_for(id).is_some_and(|p| p.edit.crop_session.is_some()) {
                    self.crop_accept(id)
                } else {
                    self.crop_enter(id)
                }
            }
            PreviewMsg::CropAccept => self.crop_accept(id),
            PreviewMsg::CropCancel => self.crop_cancel(id),
            PreviewMsg::CropDragBegin(handle, x, y) => {
                self.crop_drag_begin(id, handle, x, y);
                Task::none()
            }
            PreviewMsg::CropDragTo(x, y, suppress) => {
                self.crop_drag_to(id, x, y, suppress);
                Task::none()
            }
            PreviewMsg::CropDragEnd => {
                self.crop_drag_end(id);
                Task::none()
            }
            PreviewMsg::BakeDone(baked) => {
                // The bake thread reported the file it wrote (or `None` on failure). Clear
                // the single-flight guard, then hand the intent to the ONE completion seam
                // — `finish_share_intent` owns save/copy/delete/close ordering for every
                // share action, baked or not (DRAGON-353).
                let Some(p) = self.preview_for_mut(id) else {
                    return Task::none();
                };
                p.edit.baking = false;
                // A `WindowClosed` arrived mid-bake (DRAGON-352) and was deferred so the
                // process couldn't exit with the bake thread mid-write. The surface is
                // gone, so the document must close once the bake has landed — a surfaceless
                // document would otherwise linger as a zombie.
                let deferred_close = std::mem::take(&mut p.edit.close_after_bake);
                let output = p.edit.pending_output.take();
                // Which completion this bake feeds (DRAGON-474, DRAGON-482): the clipboard,
                // the share sheet, or a cloud upload. Taken unconditionally so a failed share
                // or upload bake cannot leave the intent armed for an unrelated later copy.
                let intent = std::mem::take(&mut p.edit.bake_intent);
                if deferred_close
                    && let Some(p) = self.preview_for_mut(id)
                {
                    p.edit.close_after_share = true;
                }
                match baked {
                    Some(_) => {
                        // DRAGON-467 review, major 4: remember WHAT was rendered and from
                        // WHERE in the history, so a copy standing on this same state (the
                        // exit copy right after an explicit Copy) serves the artifact instead
                        // of running the encoder again.
                        if let (Some(p), Some(out)) = (self.preview_for_mut(id), output.as_ref()) {
                            p.edit.mark_baked(out);
                        }
                        // `pending_output` is where the bake wrote; the size it reported is
                        // re-read from disk by the completion seam.
                        match intent {
                            edit::BakeIntent::Copy => self.finish_copy(id, output),
                            // A share bake whose surface died mid-flight has no window to
                            // anchor a sheet to — honour the deferred close instead of
                            // flashing a failure toast at a dead document.
                            edit::BakeIntent::ShareSheet if deferred_close => {
                                self.close_preview(id)
                            }
                            edit::BakeIntent::ShareSheet => self.finish_share_sheet(id, output),
                            // An UPLOAD is not anchored to the surface at all: it stages the
                            // artifact and hands it to a detached child, so a document whose
                            // surface died mid-bake still delivers, and only then closes. That
                            // is the whole point of the detached design: the user asked for
                            // this capture to reach their account, and losing the window is
                            // not them changing their mind. The toast it posts lands on a
                            // document that is about to go, which costs nothing.
                            edit::BakeIntent::Upload { account, auto_share } => {
                                let task = self.finish_upload(id, output, &account, auto_share);
                                if deferred_close {
                                    Task::batch(vec![task, self.close_preview(id)])
                                } else {
                                    task
                                }
                            }
                        }
                    }
                    None => {
                        // Bake failed (ffmpeg / encode error). The ORIGINAL file on disk is
                        // untouched and the editor is still up with its edits intact, so
                        // this is recoverable — say so and stay put (DRAGON-353: a failed
                        // export no longer ends the session).
                        log::warn!("preview edit bake failed; the capture is unchanged");
                        self.preview_toast_icon(
                            id,
                            ToastKind::Error,
                            "Couldn't process the capture — your edits are still here",
                            "save-off-symbolic",
                        );
                        // If this action came from the unsaved-changes dialog, a toast is
                        // not enough: the user asked to LEAVE, and a window that simply
                        // stays put looks like nothing happened. Re-raise the dialog with
                        // the reason and the choice (retry / Exit anyway / Continue
                        // editing) — the one failure path all four dialog actions share.
                        self.fail_close_action(
                            id,
                            "The edits couldn't be rendered, so nothing was saved. The \
                             encoder rejected the export; your edits are untouched.",
                        );
                        if deferred_close { self.close_preview(id) } else { Task::none() }
                    }
                }
            }
            // ── Unsaved-changes close guard (DRAGON-353) ─────────────────────────────
            PreviewMsg::KeepEditing => {
                // "Continue editing": back into the document, dialog and any failure notice
                // cleared. The edits, the history and the retarget state are untouched.
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.dismiss_close_dialog();
                }
                Task::none()
            }
            PreviewMsg::DiscardAndClose => {
                // "Close without saving" / "Exit anyway" — ONE discard route for both, so a
                // deliberate exit after a failed save is the same code path as a deliberate
                // exit before one. Only the pending edits are abandoned as FILES; every file
                // on disk is untouched.
                //
                // DRAGON-467 review, major 2: it still honours "Automatically copy changes on
                // exit". "Without saving" is about the DISK, and the setting is about the
                // CLIPBOARD, so skipping the copy here contradicted both the setting's own
                // description and the exit path two arms up — the user answered a question
                // about files and silently lost the clipboard behaviour they had switched on.
                // For a recording that means an encode behind the spinner on the way out,
                // which is the cost of the toggle they enabled.
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.dismiss_close_dialog();
                }
                let a = self.preview_automation(id);
                let carry = self.preview_for(id).is_some_and(|p| {
                    exit_copies_changes(
                        a.copy_on_exit,
                        p.dirty(),
                        edit::clipboard_is_current(p.edit.copied_depth, p.edit.undo_stack.len()),
                    )
                });
                if carry {
                    // The same "act, then close" route the exit path takes, so a failed copy
                    // keeps the editor up rather than discarding over a clipboard miss.
                    return self.share_then_close(id, PreviewMsg::Copy);
                }
                self.stop_preview_playback(id);
                self.close_preview(id)
            }
            // The dialog's action buttons ACT and THEN close: arm `close_after_share` and
            // delegate to the plain TOOLBAR message, so the share flow never learns about
            // closing and the two entry points can never drift.
            PreviewMsg::SaveAndClose => self.share_then_close(id, PreviewMsg::Save),
            PreviewMsg::ToastTick => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.toasts.expire(std::time::Instant::now());
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
                        // LOGICAL POINTS (DRAGON-449) — the open fit's space; see the still
                        // path's note above for why the two must agree.
                        let out = self
                            .preview_output
                            .as_ref()
                            .map(|(n, o)| monitor_fit_points(*o, monitor_point_scale(Some(n))));
                        // Windows (DRAGON-288): an external `--preview` video has no capture
                        // anchor (`out` None) — bound the fit to the preview window's LIVE
                        // monitor instead of native-sizing it off-screen. Additive; Linux/mac
                        // keep `out` unchanged.
                        #[cfg(windows)]
                        let out = out.or_else(|| {
                            crate::platform::windows::window::preview_window_monitor_points(
                                super::shell::PREVIEW_WINDOW_TITLE,
                            )
                        });
                        // Logical (backing-scale-divided) footprint so a Retina recording
                        // re-fits to its true on-screen size, matching the open fit.
                        let target = p.sizing_media_points();
                        let want = windowed_fit_size(
                            target,
                            out,
                            transport_h_for(&p.kind, p.surface),
                            self.preview_toolbar_labels,
                        );
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
                // DRAGON-467: Save IS Save As. The picker opens pre-filled with the path a
                // plain overwrite-save would have written — the configured save folder plus
                // the capture's own name, or the document's own file once it has saved once
                // (`naming::save_prefill`) — and the native dialog's replace prompt is what
                // guards an existing file. That is the Snipping Tool's flow, and it retires
                // both the silent `-edited` sibling and the separate Save As button.
                if self.refuse_unbakeable_save_as(id) {
                    return Task::none();
                }
                self.save_as_dialog(id)
            }
            PreviewMsg::Copy => {
                // The clipboard gets the CURRENT state: pending edits bake first (to a
                // throwaway temp, so nothing on disk moves). Nothing else happens —
                // DRAGON-467 removed the save-first and close-after settings that used to
                // layer onto this, so a Copy is exactly a copy and the editor stays up.
                //
                // `run_share` is still the one entry point, so a video Copy that OWES a bake
                // it cannot produce (a recording whose ffprobe never landed) is refused there,
                // whole, with the editor up and the reason on it (DRAGON-398).
                self.run_copy(id)
            }
            PreviewMsg::Share => {
                // DRAGON-474 fix: share the document's CURRENT STATE, not the base file.
                // The first wiring handed `preview_current_file` straight to the sheet —
                // the last save, or the pristine capture — so every unsaved edit was
                // silently absent from what the share target received. `run_share_sheet`
                // runs the same refuse / reuse / bake funnel as Copy, so the two actions
                // can never disagree about what "the current state" means.
                self.run_share_sheet(id)
            }
            PreviewMsg::ShareDone(result) => {
                // DRAGON-480 (macOS only — see `finish_share_sheet`): the picker finished
                // presenting off the synchronous `update()` path, so its outcome arrives
                // here instead of inline. Same toast Windows/Linux show inline on `Err`;
                // `Ok` is silent there too, so this stays consistent across platforms.
                if let Err(reason) = result {
                    self.preview_toast_icon(id, ToastKind::Error, reason, "share-symbolic");
                }
                Task::none()
            }
            // ── Upload (DRAGON-482) ──────────────────────────────────────────────────────
            PreviewMsg::UploadFlyoutToggle => {
                // The accounts are RE-READ here: the settings window is a separate document
                // (and, when it is a detached `--settings` child, a separate process), so an
                // account connected since this editor opened has to appear the first time the
                // user asks to see the list. This is the only refresh point besides the
                // document's own open; the toolbar reads the snapshot every frame. Filtered to
                // this document's media kind (DRAGON-493) BEFORE the count below decides
                // whether Upload has anything to offer, so a doc with only wrong-kind accounts
                // connected is treated the same as having none.
                let is_video =
                    self.preview_for(id).is_some_and(|p| matches!(p.kind, PreviewKind::Video(_)));
                let accounts = edit::accounts_for_kind(crate::cloud::accounts::list(), is_video);
                let open_now = self
                    .preview_for(id)
                    .is_some_and(|p| p.edit.flyout_kind() == Some(edit::FlyoutKind::Upload));
                // Both routes here (the toolbar button and primary+U) go through ONE decision,
                // so a key press can never do something the button would have refused. That
                // now includes the one-at-a-time rule (DRAGON-514): the button draws itself
                // disabled off `edit::upload_in_flight`, and this is where the keybind meets
                // the same answer.
                let busy = self
                    .preview_for(id)
                    .is_some_and(|p| edit::upload_in_flight(&p.edit.uploads));
                let action = edit::upload_toggle(open_now, accounts.len(), busy);
                let selected = edit::upload_preselect(&accounts, self.cloud_last_account.as_deref());
                let len = accounts.len();
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.cloud_accounts = accounts;
                    match action {
                        edit::UploadToggle::Close => p.edit.close_flyout(),
                        // The keyboard highlight opens ON the preselected account, so arrows
                        // move from where the user left off and Enter re-picks it harmlessly.
                        edit::UploadToggle::Open => {
                            if let Some(sel) = selected {
                                p.edit.open_flyout(edit::FlyoutKind::Upload, Some(sel), len);
                            }
                        }
                        edit::UploadToggle::Refuse | edit::UploadToggle::Busy => {}
                    }
                }
                // Only the keybinding can arrive at either of these: the toolbar button is not
                // pressable in either state. Say why rather than swallowing the press, because
                // the disabled button's tooltip is the only other place that answer lives and a
                // keystroke never shows a tooltip.
                if action == edit::UploadToggle::Refuse {
                    log::debug!("preview: upload asked for with no connected accounts");
                    self.preview_toast_icon(
                        id,
                        ToastKind::Error,
                        "No cloud accounts yet. Connect one in Settings.",
                        "upload-symbolic",
                    );
                }
                if action == edit::UploadToggle::Busy {
                    // DRAGON-514. Not an error: the user asked for something reasonable at a
                    // moment it cannot happen, and the meter beside them is already showing
                    // why.
                    log::debug!("preview: upload asked for while one is already running");
                    self.preview_toast_icon(
                        id,
                        ToastKind::Success,
                        "An upload is already running.",
                        "upload-symbolic",
                    );
                }
                Task::none()
            }
            PreviewMsg::UploadFlyoutClose => {
                // The popover's outside-click dismiss. Escape reaches `FlyoutClose` instead
                // (the shared modal-key lane), and both mean the same thing here: close it,
                // upload nothing, keep the account choice for next time.
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.close_flyout();
                }
                Task::none()
            }
            PreviewMsg::UploadAccountSelected(account) => {
                // Remembered immediately, not at upload time: picking a destination is itself
                // a preference, and it should survive an editor closed without uploading.
                let moved = self
                    .preview_for(id)
                    .map(|p| p.edit.cloud_accounts.iter().position(|a| a.id == account));
                self.cloud_last_account = Some(account);
                if let (Some(p), Some(Some(i))) = (self.preview_for_mut(id), moved) {
                    // Keep the keyboard highlight on the row the pointer just picked, so the
                    // two ways of choosing cannot leave the panel showing two answers.
                    if let Some(f) = &mut p.edit.flyout {
                        f.selected = Some(i);
                    }
                }
                // A pick collapses the nested picker back to its chip (DRAGON-489 follow-up),
                // the same "pick closes the dropdown" behaviour `SetTextSize`/`SetTextFont`
                // already have.
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.upload_account_menu_open = false;
                }
                self.save_state();
                Task::none()
            }
            PreviewMsg::UploadAccountMenu(open) => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.upload_account_menu_open = open;
                }
                Task::none()
            }
            PreviewMsg::UploadAutoShareToggled(on) => {
                self.cloud_auto_share = on;
                self.save_state();
                Task::none()
            }
            // ── Visibility (DRAGON-493) ─────────────────────────────────────────────────
            //
            // It applies to whichever account the panel currently highlights (the same
            // resolution `UploadStart` uses, [`Self::upload_selected_account_id`]), and
            // persists straight onto that `CloudAccount`, the same way `UploadAccountSelected`
            // persists a folder choice: a preference set here must survive an editor closed
            // without uploading. It never reaches the network: this is a local TOML write via
            // `cloud::accounts::upsert`.
            PreviewMsg::UploadVisibilityMenu(open) => {
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.upload_visibility_menu_open = open;
                }
                Task::none()
            }
            PreviewMsg::UploadVisibilitySelected(visibility) => {
                let Some(account_id) = self.upload_selected_account_id(id) else {
                    return Task::none();
                };
                let Some(p) = self.preview_for_mut(id) else {
                    return Task::none();
                };
                let Some(account) = p.edit.cloud_accounts.iter_mut().find(|a| a.id == account_id)
                else {
                    return Task::none();
                };
                account.visibility = Some(visibility);
                let saved = account.clone();
                // A pick collapses the nested menu (DRAGON-489 follow-up's own pattern), the
                // same "pick closes the dropdown" behaviour the account picker has.
                p.edit.upload_visibility_menu_open = false;
                if let Err(e) = crate::cloud::accounts::upsert(saved) {
                    self.preview_toast_icon(id, ToastKind::Error, e, "upload-symbolic");
                }
                Task::none()
            }
            PreviewMsg::UploadStart => {
                // The DESTINATION is resolved through the same pure rule the flyout opened
                // with, against this document's snapshot, so a remembered account that has
                // been disconnected since falls back to the first one rather than uploading
                // into a hole, and the panel's highlighted row is what actually receives it.
                let account = self.upload_selected_account_id(id);
                let Some(account) = account else {
                    log::warn!("preview: upload started with no account to send it to");
                    self.preview_toast_icon(
                        id,
                        ToastKind::Error,
                        "No cloud accounts yet. Connect one in Settings.",
                        "upload-symbolic",
                    );
                    return Task::none();
                };
                let auto_share = self.cloud_auto_share;
                // Persist the effective choice: the account may never have been CLICKED (the
                // flyout opened on it), and that is still the one the user is uploading to.
                self.cloud_last_account = Some(account.clone());
                self.save_state();
                // The flyout closes BEFORE the work starts, so the bake's processing spinner
                // is not drawn under a panel that has nothing left to offer.
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.close_flyout();
                }
                self.run_upload(id, account, auto_share)
            }
            PreviewMsg::UploadAnimTick => {
                // One step of the finalize stripe sweep (DRAGON-537). The advance wraps at
                // the stripe pitch inside `upload_stripes::advance`, and the re-render this
                // message causes is the whole animation; the meter reads the phase in
                // `chrome::upload_progress_track`.
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.upload_anim = crate::widgets::upload_stripes::advance(p.edit.upload_anim);
                }
                Task::none()
            }
            PreviewMsg::UploadPoll => {
                // Drain terminal outcomes into `finished` under a SHORT borrow of `p`, then
                // act on them with `self` free again: `preview_toast_icon` re-borrows `self`,
                // which cannot overlap a `p` still held from `preview_for_mut` above.
                let mut finished: Vec<(edit::UploadWatch, crate::cloud::session::UploadState)> =
                    Vec::new();
                if let Some(p) = self.preview_for_mut(id) {
                    // `retain_mut` (not `retain`): a non-terminal read updates the titlebar's
                    // own tracked percentage in place, which is why this needs `&mut` on the
                    // entry rather than just deciding whether to keep it.
                    p.edit.uploads.retain_mut(|w| {
                        // ENDING (DRAGON-507): the user pressed the X. This entry is on a
                        // half-second timer of its own and nothing can change its face any
                        // more, so it is neither read nor reported — a late `Canceled` from
                        // the child must not resurrect a meter that is already leaving.
                        if w.ending.is_some() {
                            return !edit::upload_ending_over(w.ending_for());
                        }
                        // Already finished: this entry is now the meter's PERSISTENT readout
                        // (DRAGON-495 for the outcome, DRAGON-514 for the persistence). Its
                        // session is cleared and its toast is out, so it must NOT be read or
                        // reported again, and it is no longer retired on a timer either: it
                        // stays until this document starts another upload
                        // (`UploadWatch::in_flight`, applied at the push in `run_upload`) or
                        // the editor closes with it. The poll keeps ticking only until it
                        // settles (`edit::upload_needs_poll`), so a meter that never leaves
                        // does not mean a timer that never stops.
                        if w.finished.is_some() {
                            return true;
                        }
                        match crate::cloud::session::read_state(&w.session_id) {
                            // Seeing a `Percent` state at ALL is proof the child has already
                            // switched to determinate (DRAGON-490 dynamic follow-up: the child
                            // only ever writes one once `tray::still_indeterminate` says a
                            // genuine value arrived), so this arm clears the spinner flag
                            // explicitly rather than leaving it at whatever an earlier poll
                            // set — the ONE place this document's watch ever un-sets it.
                            Some(crate::cloud::session::UploadState::Percent(n)) => {
                                w.last_percent = Some(n);
                                w.indeterminate = false;
                                true
                            }
                            // Still nothing genuine (every upload starts here, and most polls
                            // while true just re-confirm it): keep watching, spinner showing.
                            Some(crate::cloud::session::UploadState::Indeterminate) => {
                                w.indeterminate = true;
                                true
                            }
                            // Every OTHER `UploadState` variant is terminal by construction
                            // (`Percent`/`Indeterminate` are the non-terminal cases, matched
                            // above), so this arm covers Done/Failed/Canceled without needing
                            // `is_terminal` to tell them apart.
                            Some(state) => {
                                // DRAGON-507: keep the file id BEFORE the state is handed on,
                                // so the finish-hold's undo has something to name. An old
                                // build's done state carries none, and the X is then not drawn
                                // at all (`UploadWatch::undoable`).
                                // DRAGON-520: the share LINK is kept the same way and at the
                                // same moment, so the meter's copy control has something to
                                // put back on the clipboard. An upload that made no link
                                // (the option was off, or the provider cannot) leaves this
                                // `None` and draws no copy control at all
                                // (`UploadWatch::copyable`).
                                if let crate::cloud::session::UploadState::Done {
                                    file_id,
                                    url,
                                    ..
                                } = &state
                                {
                                    if let Some(fid) = file_id {
                                        w.file_id = Some(fid.clone());
                                    }
                                    if let Some(link) = url {
                                        w.share_url = Some(link.clone());
                                    }
                                }
                                finished.push((w.clone(), state.clone()));
                                // The outcome is reported exactly once, here. Whether the
                                // entry then STAYS is `upload_outcome`'s decision
                                // (DRAGON-495): a Done or a Failed enters a finished HOLD so
                                // the meter can show what happened for a few seconds instead
                                // of vanishing mid-transfer, while a CANCEL clears at once,
                                // because the user already knows. The `finished.is_some()`
                                // guard at the top of this closure is what stops the next poll
                                // reporting a held entry a second time.
                                let Some(outcome) = edit::upload_outcome(&state) else {
                                    return false; // canceled: the meter clears now
                                };
                                w.finished = Some(outcome);
                                w.finished_at = Some(std::time::Instant::now());
                                // A completed upload reads 100%: the child's own progress
                                // states stop at 99 (`cloud::tray::counter`), so without this
                                // the success meter would hold at whatever the last bucket was.
                                if outcome == edit::UploadOutcome::Done {
                                    w.last_percent = Some(100);
                                    w.indeterminate = false;
                                }
                                true
                            }
                            // Nothing new to report yet (a torn read, or the child has not
                            // written anything since the last poll): keep watching, unchanged.
                            None => true,
                        }
                    });
                }
                // DRAGON-553: a link the CHILD could not copy (this session serves the
                // clipboard from a focused window, which a detached child has none of) is
                // written here instead, through this document's own window. Collected across
                // the loop below and batched at the end, because the loop is a `for` over
                // owned values with no task of its own.
                let mut link_copies: Vec<Task<cosmic::Action<Msg>>> = Vec::new();
                for (watch, state) in finished {
                    // `retain_mut` above only ever pushes a non-`Percent` state; state as an
                    // invariant here rather than leaving the reasoning implicit.
                    debug_assert!(crate::cloud::session::is_terminal(&state));
                    // The sidecars have done their job; nothing else will ever read this
                    // session again once this document has acted on its outcome.
                    crate::cloud::session::clear_session(&watch.session_id);
                    match state {
                        crate::cloud::session::UploadState::Done { shared, url, .. } => {
                            self.preview_toast_icon(
                                id,
                                ToastKind::Success,
                                format!("Uploaded to {}", watch.label),
                                "upload-symbolic",
                            );
                            if shared {
                                // The child already holds the selection (the standalone
                                // route). Say so, and nothing more — unchanged behaviour.
                                self.preview_toast_icon(
                                    id,
                                    ToastKind::Success,
                                    "Copied to clipboard",
                                    "clipboard-check-symbolic",
                                );
                            } else if let Some(link) = url.as_deref() {
                                // DRAGON-553: a link exists but nobody has copied it, which
                                // on this route is not a failure, it is whose turn it is. The
                                // editor has the focused window the write needs; it posts its
                                // own toast so the claim still matches what happened.
                                link_copies.push(self.copy_upload_link_on_finish(id, link));
                            }
                        }
                        crate::cloud::session::UploadState::Failed => {
                            self.preview_toast_icon(
                                id,
                                ToastKind::Error,
                                format!("Upload to {} failed", watch.label),
                                "upload-symbolic",
                            );
                        }
                        crate::cloud::session::UploadState::Canceled => {
                            // Not an error: the user asked for this, from the tray or this
                            // very document's own titlebar Cancel.
                            self.preview_toast_icon(
                                id,
                                ToastKind::Success,
                                "Upload canceled",
                                "upload-symbolic",
                            );
                        }
                        // `is_terminal` filtered these out above; `retain_mut` never pushes
                        // either one into `finished`.
                        crate::cloud::session::UploadState::Percent(_)
                        | crate::cloud::session::UploadState::Indeterminate => unreachable!(),
                    }
                }
                // Empty on every standalone-route session, where `Task::batch([])` is a
                // `Task::none()` by another name, so nothing about those changes.
                Task::batch(link_copies)
            }
            PreviewMsg::UploadCancel(session_id) => {
                // Fire-and-forget: the child's own poll thread notices the marker and stops
                // the transfer between chunks.
                crate::cloud::session::request_cancel(&session_id);
                // DRAGON-507: the meter answers the CLICK, not the child. Red and X-less now,
                // gone in `UPLOAD_ENDING_HOLD`. Before this, the whole visible response to a
                // cancel was however long the child took to notice the marker and write
                // `Canceled` — a control that looked untouched while the user waited to find
                // out whether their press had registered.
                self.begin_upload_ending(id, &session_id, edit::MeterAction::Cancel);
                Task::none()
            }
            PreviewMsg::UploadUndo(session_id) => {
                self.undo_upload(id, &session_id);
                Task::none()
            }
            PreviewMsg::UploadCopyLink(session_id) => {
                // DRAGON-553: the write can be a task now (this session may serve the
                // clipboard from our own window), so it is returned rather than dropped.
                self.copy_upload_link(id, &session_id)
            }
            PreviewMsg::LoadFailed => {
                // DRAGON-415: the decode thread died. The capture is SAVED, so this is not a
                // lost capture, but the editor the user was waiting for is never going to
                // appear — say so, then close exactly as `Cancel` does (which ends the
                // process only if this was the last document).
                //
                // DRAGON-436 round 2 (Windows) / DRAGON-415 (macOS): "say so, THEN close" has
                // to be sequenced by hand on both platforms — neither may close inline. A
                // `MessageBox` does not block, and on Windows this close usually ends the
                // PROCESS — a single-document session is the common case there (since
                // DRAGON-651 a Windows preview can host handed-off siblings too, but a
                // load failure with no siblings is the shape this guards), so `Cancel`
                // reaches `finish_session` and its 1.5s hard exit within milliseconds,
                // killing the alert thread before anyone could read a word.
                // `NSAlert::runModal` DOES block, but only safely off the winit thread
                // (see `platform::mac::alert`'s module doc), so mac needs the exact same
                // deferral. Both wait for the dismissal (or, on Windows, its 120s bound)
                // before closing.
                #[cfg(windows)]
                if let Some(dismissal) = self.show_failure_alert() {
                    return Task::perform(super::failure::await_dismissal(dismissal), move |()| {
                        cosmic::Action::App(Msg::Preview(
                            id,
                            PreviewMsg::LoadFailedAlertDismissed,
                        ))
                    });
                }
                #[cfg(target_os = "macos")]
                if let Some(dismissal) = self.report_failure_deferred() {
                    return Task::perform(super::failure::await_mac_dismissal(dismissal), move |()| {
                        cosmic::Action::App(Msg::Preview(
                            id,
                            PreviewMsg::LoadFailedAlertDismissed,
                        ))
                    });
                }
                // Nothing was shown (suppressed, or no presenter), so there is nothing to
                // wait for and the close happens now, exactly as it always did.
                #[cfg(not(any(windows, target_os = "macos")))]
                self.report_failure();
                self.update_preview(id, PreviewMsg::Cancel)
            }
            #[cfg(any(windows, target_os = "macos"))]
            PreviewMsg::LoadFailedAlertDismissed => {
                // The alert has been read (or, on Windows, waited out). Run the close the
                // `LoadFailed` arm was holding — the SAME `Cancel` it would have run inline.
                self.update_preview(id, PreviewMsg::Cancel)
            }
            PreviewMsg::Cancel => {
                // THE EXIT PATH. Close without deleting — the file stays where it is (or, when
                // "Automatically save originals" is off, stays in the runtime directory).
                // Deleting is the explicit Delete (trash) action.
                //
                // Two gates, in this order, and the order is the point:
                //
                // 1. ASK. With unsaved edits and "Ask to save edited …" on, raise the
                //    unsaved-changes dialog instead of closing (DRAGON-353's card, now
                //    settings-gated). THE gate for every close path that can still show UI —
                //    the Esc / Close-button `Cancel`, the CSD ✕ and the WM close (both routed
                //    here through `WindowCloseRequested`). A `WindowClosed` cannot be gated:
                //    the surface is already destroyed by then, so there is nowhere to draw.
                // 2. COPY. With "Automatically copy changes on exit" on and something to
                //    carry, put the EDITED result on the clipboard and close once it lands
                //    (DRAGON-467). This runs AFTER the ask gate so the two compose the way
                //    the user reads them: you are asked about the disk first, and whatever
                //    you answer, the clipboard ends up holding what you were looking at.
                let a = self.preview_automation(id);
                if self.preview_for(id).is_some_and(|p| {
                    close_needs_confirmation(a.ask_to_save, p.unsaved(), p.edit.confirm_close)
                }) {
                    if let Some(p) = self.preview_for_mut(id) {
                        p.edit.confirm_close = true;
                    }
                    return Task::none();
                }
                let carry = self.preview_for(id).is_some_and(|p| {
                    exit_copies_changes(
                        a.copy_on_exit,
                        p.dirty(),
                        edit::clipboard_is_current(p.edit.copied_depth, p.edit.undo_stack.len()),
                    )
                });
                if carry {
                    // `share_then_close` is the SAME "act, then close" route the dialog's own
                    // buttons take: it arms `close_after_share` and dispatches the plain Copy,
                    // so the close waits for the bake and the clipboard write, and a FAILED
                    // copy leaves the editor up instead of closing over a lost edit.
                    return self.share_then_close(id, PreviewMsg::Copy);
                }
                self.stop_preview_playback(id);
                self.close_preview(id)
            }
            PreviewMsg::ToggleAppearance => self.toggle_preview_appearance(id),
            PreviewMsg::OpenSettings => self.open_settings_from_preview(id),
            PreviewMsg::OpenColorPicker => {
                // Detached, like every other tool launch. The editor is protected from a
                // later capture's sibling sweep by its own preview marker, and nothing
                // here ends this session.
                //
                // DRAGON-587: the child is TOLD which editor asked, by pid, so its pick
                // comes back here and can never land in an unrelated editor. The pid is an
                // explicit part of the launch rather than something the child infers from
                // whatever happens to be open. A child that finds this editor gone simply
                // shows its own result window, so nothing is lost either way.
                let pid = std::process::id().to_string();
                crate::recording_ui::spawn_capture_child_args(
                    &["--color-picker"],
                    &[(crate::app::color_picker::COLOR_TO_PID_ENV, pid.as_str())],
                );
                Task::none()
            }
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
            PreviewMsg::SaveAsResult(opt) => {
                let Some(dest) = opt else {
                    // Cancelled. A window is still open (only overlays close for the dialog),
                    // so stay on it; an overlay was torn down for the dialog, so bring it
                    // BACK — the capture and its edits are still loaded, and a cancelled
                    // dialog must return the user to where they were, not exit
                    // (DRAGON-157).
                    //
                    // DRAGON-467 review, major 5: DISARM the close first. A Save reached from
                    // the unsaved-changes card (or from the exit path) armed
                    // `close_after_share`, and cancelling the picker means the action did not
                    // happen — leaving it armed made the NEXT unrelated Copy close the editor
                    // out from under the user. Pre-existing, but the picker is the primary
                    // Save path now, so it went from a corner to the mainstream.
                    if let Some(p) = self.preview_for_mut(id) {
                        p.edit.close_after_share = false;
                    }
                    return if self.preview_for(id).is_some_and(|p| p.surface.is_window()) {
                        Task::none()
                    } else {
                        self.reopen_preview_surface(id)
                    };
                };
                // Gather everything the background worker needs, then release `self`.
                // (`external` used to matter here: a `--preview` file was COPIED while a
                // fresh capture was MOVED. DRAGON-353 made copy-not-move universal, so the
                // distinction is gone — see the export note below.)
                let processing_msg = random_processing_msg();
                let (src, covermark, annotations, annot_curve, dim, crop, video, is_video, dirty) = match self.preview_for(id) {
                    Some(p) => {
                        // The document's PRISTINE media, not `path` (DRAGON-467 review,
                        // blocker 1): after a still has been saved over its own capture, the
                        // untouched bytes live in a runtime-dir snapshot.
                        let Some(src) = p.bake_source().map(|s| s.to_path_buf()) else {
                            return self.close_preview(id);
                        };
                        let is_video = matches!(p.kind, PreviewKind::Video(_));
                        // A video bake needs the probed metadata; without it we can only
                        // copy it (share unedited). Images bake from their own pixels.
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
                        // Annotations + dim + crop are IMAGES only; a video never accumulates them.
                        // The curve radius is a POINT preset baked at SOURCE px (DRAGON-383);
                        // identity on an unscaled (1x) output.
                        (src, p.edit.covermark.clone(), p.edit.annotations.clone(), annotate::points_to_source_px(p.edit.curve_radius(), p.source_scale), p.edit.dim, p.edit.crop, video, is_video, p.dirty())
                    }
                    None => return self.close_preview(id),
                };
                // DRAGON-455: a STILL is written as PNG, so the destination NAMES png
                // whatever the user typed into the box. Forced HERE, in the shared tree, on
                // the path the dialog handed back — the three native panels each let a
                // foreign extension through in their own way (the Windows Common Item
                // Dialog through its all-files entry, `NSSavePanel` and the xdg portal by
                // simply allowing it), so one platform-local guard could never hold the
                // rule. `png_name` is idempotent, so a panel that already appended `.png`
                // is left exactly as it is. A RECORDING keeps the container the user chose;
                // `bake_video` really does honour it.
                let dest = if is_video { dest } else { naming::png_name(&dest) };
                // Only bake when there's something to apply AND we can (video needs meta).
                // The `video.is_some()` term is now unreachable defence: `PreviewMsg::SaveAs`
                // REFUSES a dirty document whose media can't bake before the picker even
                // opens (DRAGON-398), so a dirty video arriving here always has its meta.
                // `dirty()` is THE shared gate (the one Save/Copy's `begin_bake` reads):
                // covermark / annotations / dim / DELETED timeline content — razor cuts
                // alone never re-encode (DRAGON-352 unification; two parallel predicates
                // here had already begun to drift).
                let can_bake = dirty && (!is_video || video.is_some());
                // ── THE PRISTINE-SOURCE GUARD (DRAGON-467 review, blocker 1) ─────────────
                // Saving in place is the DEFAULT gesture now (the picker pre-fills the
                // overwrite target), so a bake whose destination IS its source is the common
                // case rather than an oddity. Left alone it would read its own output on the
                // NEXT bake — doubled annotations for a still, a re-applied cut for a
                // recording — and for video it would also have ffmpeg reading and writing one
                // file at once. `edit::bake_prep` is the rule; the two arms differ because a
                // still can be snapshotted for a few MB and a take cannot.
                let prep = edit::bake_prep(can_bake && same_file(&src, &dest), is_video);
                let src = match prep {
                    edit::BakePrep::SnapshotStill => {
                        // Copy the pristine bytes aside and repoint the document at them, so
                        // the scene stays fully editable across the save: undo, retouch, save
                        // again, all still compositing onto untouched pixels. Synchronous on
                        // purpose (one file copy of a decoded still) — the invariant must hold
                        // before the worker starts, not eventually.
                        match snapshot_bake_source(&src) {
                            Some(snap) => {
                                if let Some(p) = self.preview_for_mut(id) {
                                    p.bake_src = Some(snap.clone());
                                }
                                snap
                            }
                            None => {
                                // The snapshot is what makes the save safe, so a failed one
                                // REFUSES the save rather than baking over the source. The
                                // editor stays up with everything intact.
                                log::warn!("preview: refusing an in-place save we can't protect");
                                self.preview_toast_icon(
                                    id,
                                    ToastKind::Error,
                                    "Couldn't save over the original, so nothing was written",
                                    "save-off-symbolic",
                                );
                                self.fail_close_action(
                                    id,
                                    "This capture couldn't be saved over itself safely, so \
                                     nothing was written. Try saving it somewhere else.",
                                );
                                return Task::none();
                            }
                        }
                    }
                    // A recording bakes through a temp beside the destination and is renamed
                    // over it by the worker, then COMMITTED in `SaveAsBaked`. Nothing to do
                    // here: the source stays where it is until the rename replaces it.
                    edit::BakePrep::CommitVideo | edit::BakePrep::Direct => src,
                };
                let via_temp = matches!(prep, edit::BakePrep::CommitVideo);
                // Mark the export in flight (DRAGON-352): the SAME single-flight `baking`
                // guard the bake path uses, so a `WindowClosed` mid-export DEFERS
                // (`close_after_bake`) instead of exiting with the worker mid-write —
                // which could truncate the destination file. `SaveAsBaked` clears it. It
                // also raises the editor's processing overlay (DRAGON-353).
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.baking = true;
                    p.edit.processing_msg = processing_msg;
                }
                // Export in the BACKGROUND: bake straight to the destination, or deliver
                // the file as it stands when nothing needs baking. Await it via a task
                // only so the app stays alive until the file lands.
                //
                // DRAGON-353: never a MOVE. A save RETARGETS the document at `dest` (the next
                // Save's picker opens there) and leaves the source file exactly where it was.
                // Moving it would delete a fresh capture's auto-saved original behind the
                // user's back. Where the source lives makes no difference to this: with
                // "Automatically save originals" off it is a runtime-dir file the OS clears at
                // logout, and it still must not vanish mid-session while the editor renders
                // from it (DRAGON-467).
                let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
                let bake_dest = dest.clone();
                std::thread::spawn(move || {
                    let dest = bake_dest;
                    let ok = if can_bake {
                        // A recording saving over ITSELF renders to a temp beside the
                        // destination and renames over it (DRAGON-467 review, blocker 1):
                        // ffmpeg cannot read and write one file at once, and a rename is the
                        // only way to replace a take without a second full-size copy. Same
                        // directory, so the rename stays atomic rather than crossing devices.
                        let staged = via_temp.then(|| bake_temp_path(&dest));
                        let write_to = staged.as_ref().unwrap_or(&dest);
                        let result = match &video {
                            Some(v) => edit::bake_video(&src, write_to, covermark.as_ref(), v),
                            None => edit::bake_image(
                                &src,
                                write_to,
                                covermark.as_ref(),
                                &annotations,
                                annot_curve,
                                dim,
                                crop,
                            ),
                        };
                        // Log the real io::Error here — it's about to be discarded to a bool.
                        if let Err(e) = &result {
                            log::warn!("preview edit bake failed (Save As): {e}");
                        }
                        match (result.is_ok(), staged) {
                            // Swap the finished render over the original. A failed rename
                            // leaves BOTH files intact, so the take is never lost; the temp
                            // is cleaned up either way.
                            (true, Some(tmp)) => match std::fs::rename(&tmp, &dest) {
                                Ok(()) => true,
                                Err(e) => {
                                    log::warn!("preview: couldn't replace the recording: {e}");
                                    let _ = std::fs::remove_file(&tmp);
                                    false
                                }
                            },
                            (false, Some(tmp)) => {
                                let _ = std::fs::remove_file(&tmp);
                                false
                            }
                            (ok, None) => ok,
                        }
                    } else {
                        // Nothing to bake. Saving over ITSELF (dest == src) would truncate
                        // it, so that degenerate pick is a success with no work.
                        let same_file = std::fs::canonicalize(&src)
                            .ok()
                            .zip(std::fs::canonicalize(&dest).ok())
                            .is_some_and(|(a, b)| a == b);
                        if same_file {
                            true
                        } else if is_video {
                            // A recording is already in one of the containers `bake_video`
                            // writes, so its bytes go straight across.
                            std::fs::copy(&src, &dest).is_ok()
                        } else {
                            // DRAGON-455: a STILL never reached `bake_image` on this path —
                            // it was a bare `fs::copy` here, which is how an unedited Save As
                            // to `shot.jpg` produced PNG bytes under a `.jpg` name while an
                            // edited one produced a real JPEG. The destination is forced to
                            // `.png` above, so a byte copy is now the CORRECT answer for a
                            // PNG source (and keeps its `--inspect` chunk for free); a
                            // non-PNG source is re-written as a real PNG instead. That whole
                            // decision, and why the copy is deliberate rather than
                            // incidental, lives in `edit::save_unedited_still`.
                            match edit::save_unedited_still(&src, &dest) {
                                Ok(_) => true,
                                Err(e) => {
                                    log::warn!("preview save failed (Save As, unedited): {e}");
                                    false
                                }
                            }
                        }
                    };
                    // DRAGON-451: a system notification used to fire here on success. Save As
                    // is driven from an editor that stays on screen and reports the result in
                    // its own toast (`SaveAsBaked` below), so a desktop banner for it was a
                    // duplicate. The system channel is now the editor-LESS one.
                    let _ = tx.send(ok);
                });
                Task::perform(rx, move |res| {
                    // The reveal + write already happened on the worker; carry the dest so
                    // the editor can retarget onto it.
                    let done = matches!(res, Ok(true)).then(|| dest.clone());
                    cosmic::Action::App(Msg::Preview(id, PreviewMsg::SaveAsBaked(done)))
                })
            }
            PreviewMsg::SaveAsBaked(done) => {
                // DRAGON-353: a save RETARGETS. The chosen destination becomes the document's
                // working file (`saved_path`), so the NEXT Save opens its picker pre-filled
                // with that path rather than with the capture folder again — and the source
                // file is left untouched. The editor stays open on it.
                //
                // First, clear the in-flight export guard and take any close DEFERRED
                // while the worker ran (DRAGON-352): a `WindowClosed` mid-export means
                // the surface is already gone, so the document must close now (on EVERY
                // outcome) rather than reload/re-mint below.
                let deferred_close = match self.preview_for_mut(id) {
                    Some(p) => {
                        p.edit.baking = false;
                        std::mem::take(&mut p.edit.close_after_bake)
                    }
                    None => false,
                };
                if deferred_close || self.preview_for(id).is_none() {
                    return self.close_preview(id);
                }
                // The unsaved-changes dialog's "Save As" button asked for a close once the
                // export landed.
                let close_after = self
                    .preview_for_mut(id)
                    .map(|p| std::mem::take(&mut p.edit.close_after_share))
                    .unwrap_or(false);
                let Some(dest) = done else {
                    self.preview_toast_icon(id, ToastKind::Error, "Couldn't save to that location", "save-off-symbolic");
                    // A failed export still leaves the edits intact, so never close on it —
                    // even when the dialog asked to. When it DID ask, re-raise the dialog
                    // carrying the reason rather than leaving a window that looks like
                    // nothing happened (the shared failure seam).
                    if close_after {
                        // `close_after` was already taken above, so re-arm for the seam to
                        // consume — it is the "this came from the dialog" signal.
                        if let Some(p) = self.preview_for_mut(id) {
                            p.edit.close_after_share = true;
                        }
                        self.fail_close_action(
                            id,
                            "The file couldn't be written to that location. Check the \
                             folder still exists and that you can write to it.",
                        );
                    }
                    // Only the OVERLAY needs anything: its surface was torn down for the
                    // file chooser.
                    return if self.preview_for(id).is_some_and(|p| p.surface.is_window()) {
                        Task::none()
                    } else {
                        self.reopen_preview_surface(id)
                    };
                };
                let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                self.stop_preview_playback(id);
                self.preview_toast_icon(
                    id,
                    ToastKind::Success,
                    format!(
                        "Saved {}",
                        dest.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| dest.display().to_string())
                    ),
                    "save-check-symbolic",
                );
                // RETARGET, never reload — the same rule as before (DRAGON-353 follow-up).
                // The document keeps rendering its pristine media plus the live scene, so the
                // undo history survives the export; only the save-side bookkeeping moves. The
                // destination joins `written` because Delete removes every file this
                // document produced, wherever the user pointed it.
                let committed = self.preview_for(id).is_some_and(|p| {
                    matches!(p.kind, PreviewKind::Video(_)) && p.bake_source() == Some(dest.as_path())
                });
                if let Some(p) = self.preview_for_mut(id) {
                    p.saved_path = Some(dest.clone());
                    p.size = Some(size);
                    p.edit.mark_saved();
                    // DRAGON-467 review, major 4: the artifact for this exact state IS the
                    // file just written, so an exit copy right after a save reuses it rather
                    // than running the encoder a second time.
                    p.edit.mark_baked(&dest);
                }
                // ── THE VIDEO COMMIT (DRAGON-467 review, blocker 1) ──────────────────────
                // A recording that saved over its OWN source has no pristine media left: the
                // rename replaced it. Copying a take aside is not an option (they are
                // multi-GB), so the document commits instead — the file now IS the edit. The
                // scene and the whole history are cleared, and the recording is re-probed, so
                // the timeline comes back describing the file that actually exists. Saying so
                // is not optional: undo just stopped being able to reach the pre-cut take.
                let mut recommit = Task::none();
                if committed {
                    let was_dirty = self.preview_for(id).is_some_and(|p| p.dirty());
                    if let Some(PreviewState { kind: PreviewKind::Video(vid), edit, bake_src, .. }) =
                        self.preview_for_mut(id)
                    {
                        edit.reset_after_commit();
                        *bake_src = None;
                        vid.timeline = None;
                        vid.meta = None;
                        vid.extracted = false;
                    }
                    if was_dirty {
                        self.preview_toast_icon(
                            id,
                            ToastKind::Success,
                            "Saved over the recording, so the edits are now part of it",
                            "save-check-symbolic",
                        );
                    }
                    recommit = video::poster_task(id, dest.clone());
                }
                if close_after {
                    return Task::batch([recommit, self.close_preview(id)]);
                }
                if committed {
                    // Only the OVERLAY needs its surface back; either way the re-probe runs.
                    let back = if self.preview_for(id).is_some_and(|p| p.surface.is_window()) {
                        Task::none()
                    } else {
                        self.reopen_preview_surface(id)
                    };
                    return Task::batch([recommit, back]);
                }
                // Only the OVERLAY needs anything back: its surface was torn down for the
                // file chooser. A window kept its own.
                if self.preview_for(id).is_some_and(|p| p.surface.is_window()) {
                    Task::none()
                } else {
                    self.reopen_preview_surface(id)
                }
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

    /// TASK-8 UNIFICATION: the unsaved-changes card and the action bar resolve to the SAME
    /// behaviour, because they dispatch the same messages — `share_then_close` only layers
    /// "and then close" onto the plain toolbar message.
    ///
    /// DRAGON-467 is what makes this short. The card is down to THREE options (Save /
    /// Continue editing / Close without saving), and only one of them is an action at all, so
    /// what is left to pin is that it routes through the toolbar's own Save rather than a
    /// parallel implementation, and that the two ways OUT are the exit gates the `Cancel` path
    /// already uses (`the_exit_gates_read_the_settings_and_the_document`).
    #[test]
    fn the_card_and_the_toolbar_resolve_the_same_action() {
        // The card's ONE action is the toolbar's Save with a close armed around it, and
        // `share_then_close`'s dispatch is what guarantees that — a second Save
        // implementation is exactly what this pins against. The message it delegates to is
        // the same variant the toolbar button emits.
        let from_card = PreviewMsg::SaveAndClose;
        let from_toolbar = PreviewMsg::Save;
        // Neither counts as hands-on document interaction, so neither shortens the toast it
        // is about to produce (the classifier lives beside the enum).
        assert!(!from_card.is_document_interaction());
        assert!(!from_toolbar.is_document_interaction());
        // "Close without saving" is a DISCARD, not an action: it produces no toast of its own
        // and is classified with the card's other exits.
        assert!(!PreviewMsg::DiscardAndClose.is_document_interaction());
        assert!(!PreviewMsg::KeepEditing.is_document_interaction());
    }

    /// DRAGON-420: a document reads the triple for ITS media kind, and nothing else. The
    /// regression this exists to catch is a video path still reaching an image field (the
    /// state before that ticket, when both kinds shared one triple) — which no compile error
    /// and no visual check would report, because the settings window would look correct and
    /// only the behaviour would be wrong.
    #[test]
    fn video_and_image_preview_settings_never_bleed_into_each_other() {
        let image = PreviewAutomation {
            copy_on_exit: true,
            save_originals: true,
            ask_to_save: true,
        };
        let video = PreviewAutomation {
            copy_on_exit: false,
            save_originals: false,
            ask_to_save: false,
        };
        // Each kind gets its OWN triple, whole — never a field-by-field mix.
        assert_eq!(preview_automation(true, image, video), video);
        assert_eq!(preview_automation(false, image, video), image);
        // Exhaustive independence: over every combination of the two triples, the answer for
        // one kind is a function of THAT kind's triple alone. Changing the other cannot move
        // it, which is precisely "a user can have screenshots asked about while recordings
        // just close".
        let all = |bits: u8| PreviewAutomation {
            copy_on_exit: bits & 1 != 0,
            save_originals: bits & 2 != 0,
            ask_to_save: bits & 4 != 0,
        };
        for i in 0..8u8 {
            for j in 0..8u8 {
                let (img, vid) = (all(i), all(j));
                assert_eq!(preview_automation(false, img, vid), img, "image reads image");
                assert_eq!(preview_automation(true, img, vid), vid, "video reads video");
            }
        }
        // And the settings reach the DECISIONS unchanged. The fully-parted case: screenshots
        // ask and carry their edits out, recordings do neither.
        let i = preview_automation(false, image, video);
        let v = preview_automation(true, image, video);
        assert!(close_needs_confirmation(i.ask_to_save, true, false), "images ask");
        assert!(!close_needs_confirmation(v.ask_to_save, true, false), "videos do not");
        assert!(exit_copies_changes(i.copy_on_exit, true, false), "images carry the edits out");
        assert!(!exit_copies_changes(v.copy_on_exit, true, false), "videos do not");
    }

    /// THE exit gates (DRAGON-467), which together decide what closing the editor does.
    #[test]
    fn the_exit_gates_read_the_settings_and_the_document() {
        // ── Ask to save ──────────────────────────────────────────────────────────────
        // All three terms are required, and each one alone can veto the card.
        assert!(close_needs_confirmation(true, true, false), "unsaved edits must be confirmed");
        assert!(!close_needs_confirmation(false, true, false), "the setting can turn it off");
        assert!(!close_needs_confirmation(true, false, false), "a clean document just closes");
        assert!(!close_needs_confirmation(true, true, true), "the dialog must not re-raise itself");
        // A clean document closes straight away whatever the setting says — there is nothing
        // to lose, so asking would be a speed bump with no question in it.
        for ask in [true, false] {
            assert!(!close_needs_confirmation(ask, false, false));
        }
        // ── Copy changes on exit ─────────────────────────────────────────────────────
        assert!(exit_copies_changes(true, true, false), "an edited document carries its edits out");
        assert!(!exit_copies_changes(false, true, false), "the setting can turn it off");
        assert!(
            !exit_copies_changes(true, false, false),
            "a clean document must not re-copy the bytes the capture already put there"
        );
        assert!(!exit_copies_changes(false, false, false));
        // DRAGON-467 review, major 4: an explicit Copy followed by Escape must not copy the
        // same bytes twice, however dirty the scene is.
        assert!(
            !exit_copies_changes(true, true, true),
            "the clipboard already holds this exact state"
        );
    }

    // DRAGON-467: `doc_with` and `delete_covers_every_file_the_document_wrote_and_no_others`
    // lived here. They pinned the over-delete guard — Delete removed the capture plus every
    // path the document actually WROTE and nothing else, tracked rather than derived so a
    // neighbouring file from an earlier session could never be swept up. The editor does not
    // delete anything now, so there is no set to guard. Any future delete needs the guard and
    // the test back together.

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
            toasts: Toasts::default(),
            copied_on_open: false,
            auto_copy_waiting: false,
            demoted: false,
            bake_src: None,
            saved_path: None,
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
    /// `pub(super)` so the DRAGON-469 module beside this one can build one too.
    pub(super) fn doc(surface: PreviewSurface) -> PreviewState {
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
        hidden.mark_surface_torn_down();
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

    // ── The copy action's gates ─────────────────────────────────────────────────
    //
    // DRAGON-467: three tests here pinned `ShareIntent`'s table (which of save / copy /
    // delete each flavour performed), that no plain share closed the editor by itself, and
    // that a copy never persisted edits. The enum is gone with the last of those actions, so
    // what is left to pin is the copy itself — and the rules did not change, they just have
    // one subject now: a copy renders to a THROWAWAY temp, never closes on its own, and only
    // owes a render when the scene has something in it.
    //
    // `bake_need` / `clipboard_is_current` (edit.rs's `bake_need_tests`) carry the rest.

    /// A COPY renders to a throwaway temp and closes nothing by itself. The close it can
    /// appear to perform is `close_after_share`, armed AROUND it by the exit path or the
    /// unsaved-changes card, which is what keeps "act, then close" in one place.
    #[test]
    fn a_copy_renders_to_a_temp_and_closes_nothing_by_itself() {
        // The rule that matters: nothing a copy writes lands where the user's file is. The
        // temp's name is what enforces it (`clipboard_temp_name`'s `-copy` marker), and that
        // is tested in `share.rs`; here we pin the shape of the decision that reaches it.
        assert!(
            !edit::bake_need(false, 0, None).eq(&edit::BakeNeed::Fresh),
            "a clean document renders nothing at all"
        );
        assert_eq!(edit::bake_need(true, 1, None), edit::BakeNeed::Fresh);
        assert_eq!(edit::bake_need(true, 1, Some(1)), edit::BakeNeed::Reuse);
    }

    /// A VIDEO preview carrying `timeline` and nothing else — enough for the share gates.
    fn video_with(timeline: Option<timeline::Timeline>) -> PreviewState {
        let mut vid = video::VideoPreview::loading();
        vid.timeline = timeline;
        PreviewState {
            path: Some(PathBuf::from("/rec/take.mp4")),
            kind: PreviewKind::Video(vid),
            ..still_at((1920, 1080), 1.0)
        }
    }

    /// DRAGON-398 — the bake gate reaching the VIDEO kind. `owes_bake` is media-agnostic;
    /// what differs for a recording is where its `dirty` comes from, so this pins the whole
    /// chain from timeline state through `PreviewState::dirty()` into the decision.
    ///
    /// **The invariant that matters**: an uncut recording — including one the user razored to
    /// pieces without deleting any of them — is NOT dirty, so nothing owes it a bake and its
    /// ffmpeg invocations never change (see `edit::video_bake_args`'s uncut test for the other
    /// end of that chain). A recording only starts re-encoding once content is actually gone.
    #[test]
    fn a_video_owes_a_bake_only_once_content_is_deleted() {
        // No timeline at all (the probe never landed) — nothing to bake.
        assert!(!video_with(None).dirty());

        // A whole, uncut recording.
        let whole = timeline::Timeline::new(10.0);
        let p = video_with(Some(whole.clone()));
        assert!(!p.dirty(), "an untouched recording is clean");

        // RAZOR CUTS ALONE: three segments, none deleted. The concatenation is byte-for-byte
        // the original, so this must stay clean — cutting is not editing until something goes.
        let mut razored = whole.clone();
        assert!(razored.cut_at_source(3.0));
        assert!(razored.cut_at_source(7.0));
        assert_eq!(razored.spans.len(), 3, "the razor split the timeline");
        let p = video_with(Some(razored.clone()));
        assert!(!p.dirty(), "razor cuts alone must never re-encode");
        assert_eq!(
            edit::bake_need(p.dirty(), p.edit.undo_stack.len(), None),
            edit::BakeNeed::None,
            "an uncut recording renders nothing"
        );
        // And nothing is carried out on exit either: there is no change to carry.
        assert!(!exit_copies_changes(true, p.dirty(), false));

        // DELETE one segment: now the output differs from the source, so it is dirty and a
        // Copy owes a re-encode (Delete still never bakes — the file is being binned).
        let mut cut = razored;
        assert!(cut.delete(1));
        let p = video_with(Some(cut));
        assert!(p.dirty(), "deleted content must bake");
        assert!(p.unsaved(), "and it has never been saved");
        assert_eq!(
            edit::bake_need(p.dirty(), p.edit.undo_stack.len(), None),
            edit::BakeNeed::Fresh,
            "a copy must render the cut"
        );
        // The exit copy now has something to carry, and the ask gate something to ask about.
        assert!(exit_copies_changes(true, p.dirty(), false));
        assert!(close_needs_confirmation(true, p.unsaved(), false));
    }

    /// DRAGON-467 — a recording's Save prefills exactly like an image's, because
    /// `preview_save_target`'s rule is `naming::save_prefill` for both: the configured folder
    /// plus the capture's own name until the document has saved once, then the file it
    /// adopted. There is no `-edited` derivation any more (see `naming`'s module doc for the
    /// survey that retired it); the picker's own replace prompt is what guards an existing
    /// file.
    #[test]
    fn a_recordings_save_prefills_the_configured_folder_then_its_own_file() {
        let dir = std::path::Path::new("/home/me/Videos");
        // Never saved, and living in the runtime dir because originals are off: the picker
        // still opens on the configured folder.
        let rec = std::path::Path::new("/run/user/1000/take.mp4");
        assert_eq!(
            naming::save_prefill(None, rec, Some(dir)),
            PathBuf::from("/home/me/Videos/take.mp4"),
            "the save folder setting is what the picker opens on"
        );
        // Once saved, further saves prefill the file it adopted rather than deriving anything.
        let adopted = std::path::Path::new("/home/me/Videos/take.mp4");
        assert_eq!(naming::save_prefill(Some(adopted), rec, Some(dir)), PathBuf::from(adopted));
    }

    /// The unsaved-changes gate: a dirty close asks first, a clean one just goes — and a
    /// close attempted while the dialog is ALREADY up must not re-raise it, or the dialog's
    /// own buttons (which re-enter these paths) could never get past themselves. The `ask`
    /// term is DRAGON-467's setting, exercised in `the_exit_gates_read_the_settings_and_the_document`.
    #[test]
    fn only_a_dirty_close_asks_and_only_once() {
        assert!(close_needs_confirmation(true, true, false), "unsaved edits must be confirmed");
        assert!(!close_needs_confirmation(true, false, false), "a clean document just closes");
        assert!(
            !close_needs_confirmation(true, true, true),
            "the dialog must not re-raise itself"
        );
        assert!(!close_needs_confirmation(true, false, true));
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

/// DRAGON-469: what a preview surface's `Closed` event means, and which appearance the
/// toggle must mint. Both are the "leaving the overlay" machinery the Save As chooser and
/// the forced demotion exercise.
#[cfg(test)]
mod leaving_the_overlay_tests {
    use super::*;

    /// THE ticket's first symptom. Off Linux `window::close` echoes a `Closed` event back for
    /// OUR OWN teardown, and `save_as_dialog` tears the fullscreen overlay down while keeping
    /// the document loaded. Reading that echo as a lost surface closed the document, which was
    /// the last one, which ended the process — with the file chooser still on screen. A
    /// cleared `surface_open` is the "we did this on purpose" signal, and it must win.
    #[test]
    fn our_own_teardown_is_never_a_lost_surface() {
        assert_eq!(surface_closed(false, false), SurfaceClosed::Ours);
        // Even mid-bake: the document is coming back on a fresh surface, so arming
        // `close_after_bake` would close a document we are about to re-mint.
        assert_eq!(surface_closed(false, true), SurfaceClosed::Ours);
    }

    /// The two pre-existing meanings are unchanged, so nothing that worked before moves: a
    /// live surface taken away out of band still closes the document (and records the
    /// DRAGON-419 failure), and one taken away mid-bake still defers to the worker.
    #[test]
    fn a_live_surface_keeps_its_historical_meaning() {
        assert_eq!(surface_closed(true, false), SurfaceClosed::LostOutOfBand);
        assert_eq!(surface_closed(true, true), SurfaceClosed::DeferToBake);
    }

    /// Only a genuinely lost surface may end the session, so the classification is the whole
    /// gate: exactly one of the three outcomes closes the document.
    #[test]
    fn exactly_one_outcome_closes_the_document() {
        let closes = |a: SurfaceClosed| a == SurfaceClosed::LostOutOfBand;
        let all = [
            surface_closed(false, false),
            surface_closed(false, true),
            surface_closed(true, false),
            surface_closed(true, true),
        ];
        assert_eq!(all.iter().filter(|a| closes(**a)).count(), 1);
    }

    /// THE ticket's second symptom. The toggle asks for the OTHER surface than the one that
    /// is open, so the button always changes something.
    #[test]
    fn the_toggle_asks_for_the_other_surface() {
        assert!(
            toggled_preview_windowed(PreviewSurface::Overlay),
            "on the overlay, the button pops out into a window"
        );
        assert!(
            !toggled_preview_windowed(PreviewSurface::Window),
            "in a window, the button goes fullscreen"
        );
    }

    /// BYTE-IDENTITY PIN: whenever the persisted setting and the open surface AGREE — every
    /// ordinary state on all three platforms — the new expression equals the old
    /// `!self.preview_windowed`.
    #[test]
    fn it_matches_the_old_inversion_whenever_setting_and_surface_agree() {
        for (setting, open) in
            [(true, PreviewSurface::Window), (false, PreviewSurface::Overlay)]
        {
            assert_eq!(
                toggled_preview_windowed(open),
                !setting,
                "in agreement the toggle must not change behaviour (setting={setting})"
            );
        }
    }

    /// The OTHER disagreeing state, which the first version of this fix missed: a window
    /// pick's PRE-OPEN cover is an OVERLAY while the setting says window, on all three
    /// platforms (Linux `window_spinner_neutral`, macOS `mac_preview_preopen`, Windows
    /// `win_preview_preopen`). No toggle can be pressed there today — the loading view carries
    /// no appearance chrome — so this pins that the new answer is the SAFE one if that ever
    /// changes: it re-persists the setting's existing value, where the old inversion would
    /// have written "overlay" as the user's default off the back of a transient cover.
    #[test]
    fn a_window_pick_pre_open_cover_cannot_persist_a_wrong_appearance() {
        let setting_windowed = true; // what the pre-open cover runs under
        let open = PreviewSurface::Overlay; // ...while an overlay is what is up
        assert_eq!(
            toggled_preview_windowed(open),
            setting_windowed,
            "the toggle re-states the setting, so nothing is silently rewritten"
        );
        assert_ne!(
            toggled_preview_windowed(open),
            !setting_windowed,
            "the old inversion would have persisted OVERLAY off a transient loading cover"
        );
    }

    /// The DESYNCED state, which is the bug. A document DEMOTED out of the overlay is a WINDOW
    /// while the setting still says overlay — `demote_preview_to_window` deliberately never
    /// touches the appearance. Inverting the setting there asked for a window, so the toggle
    /// tore the window down and minted an identical one; reading the surface asks for the
    /// overlay. The LIVE trigger is the Settings-from-preview demotion; the DRAGON-336
    /// second-document one produces the same state but the toggle's sibling early-return
    /// blocks it until that sibling closes.
    #[test]
    fn a_forced_demotion_no_longer_makes_the_toggle_reload_the_window() {
        // The state after a demotion: the surface is a WINDOW, the setting still says
        // overlay (`false`), because `demote_preview_to_window` never touches it.
        let open = PreviewSurface::Window;
        let setting_windowed = false;
        assert!(
            !toggled_preview_windowed(open),
            "the toggle must ask for the overlay the user can see it is not on"
        );
        // The old inversion produced `!false` = a WINDOW, the kind already open: the two
        // answers differ here, and that difference is the bug.
        assert_ne!(toggled_preview_windowed(open), !setting_windowed);
    }

    /// The teardown ORDERING the whole fix rests on, pinned at the model level: after the one
    /// named mutator, the classification is `Ours`. If a later edit issues a destroy without
    /// going through it, the flag is still set when the echo lands and the document is closed
    /// (which, on the last document, ends the process mid-dialog).
    #[test]
    fn a_marked_teardown_classifies_as_ours_and_an_unmarked_one_does_not() {
        let mut p = tests::doc(PreviewSurface::Overlay);
        assert_eq!(
            surface_closed(p.surface_open, p.edit.baking),
            SurfaceClosed::LostOutOfBand,
            "an un-marked destroy is a lost surface, which is what ends the session"
        );
        p.mark_surface_torn_down();
        assert_eq!(
            surface_closed(p.surface_open, p.edit.baking),
            SurfaceClosed::Ours,
            "marking first is what makes our own destroy survivable"
        );
        // Idempotent: the `WindowClosed` handler marks again on arrival.
        p.mark_surface_torn_down();
        assert_eq!(surface_closed(p.surface_open, p.edit.baking), SurfaceClosed::Ours);
    }

    /// DRAGON-488: the appearance toggle's direction is decided by the OPEN SURFACE and
    /// nothing else. DRAGON-469 briefly ANDed a second input into it, "is a settings pane
    /// open", and refused the overlay direction when it was; that refusal is gone, so the
    /// press on a window always heads for the overlay whatever else is on screen. This is the
    /// model-level statement of "settings never blocks fullscreen".
    #[test]
    fn the_direction_depends_on_the_open_surface_alone() {
        assert!(!toggled_preview_windowed(PreviewSurface::Window), "window ⇒ heading to overlay");
        assert!(toggled_preview_windowed(PreviewSurface::Overlay), "overlay ⇒ heading to window");
        // The state the ticket was filed from: demoted for settings (a WINDOW is open, the
        // setting still says overlay), pane still up. The answer is the overlay, and there is
        // no second input that could turn it into a refusal.
        let after_settings_demotion = PreviewSurface::Window;
        assert!(
            !toggled_preview_windowed(after_settings_demotion),
            "pressing fullscreen with settings open must still ask for the overlay"
        );
    }
}

/// DRAGON-488: the Settings press from the preview editor. The demotion is unconditional and
/// the pane decision is a closed three-case table, so no branch can quietly skip the way back
/// out of fullscreen.
#[cfg(test)]
mod settings_activation_tests {
    use super::*;

    /// The three cases, plus the domination rule: a pane we OWN wins over the cross-process
    /// probe, which would answer `true` for our own lock anyway.
    #[rstest::rstest]
    #[case::own_pane_only(true, false, SettingsActivation::FocusOwn)]
    #[case::own_pane_beats_the_probe(true, true, SettingsActivation::FocusOwn)]
    #[case::pane_in_another_instance(false, true, SettingsActivation::FocusOther)]
    #[case::no_pane_anywhere(false, false, SettingsActivation::Spawn)]
    fn the_three_cases(
        #[case] own_pane: bool,
        #[case] pane_elsewhere: bool,
        #[case] want: SettingsActivation,
    ) {
        assert_eq!(settings_activation(own_pane, pane_elsewhere), want);
    }

    /// Exactly one case focuses a window WE own; the other two spawn a child. The distinction
    /// is what the caller matches on, and folding two of them together would either spawn a
    /// second pane or try to focus a window id we do not have.
    #[test]
    fn only_the_own_pane_case_focuses_in_process() {
        let all = [
            settings_activation(true, true),
            settings_activation(true, false),
            settings_activation(false, true),
            settings_activation(false, false),
        ];
        assert_eq!(all.iter().filter(|a| **a == SettingsActivation::FocusOwn).count(), 2);
        // ...and every case is reachable, so none of them is dead code.
        for want in [
            SettingsActivation::FocusOwn,
            SettingsActivation::FocusOther,
            SettingsActivation::Spawn,
        ] {
            assert!(all.contains(&want), "{want:?} is unreachable");
        }
    }

    /// THE ticket's invariant, stated where it can be read: NO case of the table carries a
    /// "skip the demotion" answer, because the demotion is not in the table at all. The
    /// `FocusOwn` branch is the one that used to return early and leave a same-process pane
    /// focused under a keyboard-owning overlay. If someone ever adds a `demote: bool` here,
    /// this test is the note explaining why they should not.
    #[test]
    fn the_demotion_is_not_one_of_the_cases() {
        // A document ON the overlay needs the demotion, whatever the pane situation is...
        for (own, elsewhere) in [(true, true), (true, false), (false, true), (false, false)] {
            let _ = settings_activation(own, elsewhere);
            assert!(
                overlay_demotion_needed(PreviewSurface::Overlay, true),
                "every settings case demotes an open overlay document"
            );
        }
        // ...and for a document already in a window the demotion is a no-op, which is why the
        // caller can issue it unconditionally instead of branching on the surface.
        assert!(!overlay_demotion_needed(PreviewSurface::Window, true));
    }
}

/// DRAGON-488: the one "is this document actually on the fullscreen overlay" expression,
/// shared by the sibling sweep and the demotion.
#[cfg(test)]
mod overlay_demotion_tests {
    use super::*;

    /// Only a LIVE overlay surface can be demoted. A document already in a window has nothing
    /// to bring down, and one whose surface is torn down while it stays loaded (a background
    /// bake, the overlay's Save-As chooser) must not have a window minted for it — that would
    /// resurrect a surface closed on purpose.
    #[rstest::rstest]
    #[case::live_overlay(PreviewSurface::Overlay, true, true)]
    #[case::overlay_with_no_surface(PreviewSurface::Overlay, false, false)]
    #[case::live_window(PreviewSurface::Window, true, false)]
    #[case::window_with_no_surface(PreviewSurface::Window, false, false)]
    fn only_a_live_overlay_is_demotable(
        #[case] surface: PreviewSurface,
        #[case] surface_open: bool,
        #[case] want: bool,
    ) {
        assert_eq!(overlay_demotion_needed(surface, surface_open), want);
    }

    /// BYTE-IDENTITY PIN for the two callers that used to spell the rule out inline (the
    /// DRAGON-336 sibling sweep and `demote_preview_to_window`'s guard): the shared predicate
    /// is the same expression they each carried.
    #[test]
    fn it_is_the_historical_expression_both_callers_carried() {
        for surface in [PreviewSurface::Overlay, PreviewSurface::Window] {
            for surface_open in [true, false] {
                assert_eq!(
                    overlay_demotion_needed(surface, surface_open),
                    !surface.is_window() && surface_open,
                );
            }
        }
    }

    /// The sibling sweep reads it through `overlay_siblings`, so a document with no live
    /// surface is never selected there either — the selection and the guard agree by
    /// construction now, rather than by two authors writing the same `&&`.
    #[test]
    fn the_sibling_sweep_selects_exactly_the_demotable_documents() {
        // `doc` mints a unique surface id per document.
        let live = tests::doc(PreviewSurface::Overlay);
        let mut torn_down = tests::doc(PreviewSurface::Overlay);
        torn_down.mark_surface_torn_down();
        let windowed = tests::doc(PreviewSurface::Window);
        let previews = vec![live, torn_down, windowed];
        assert_eq!(overlay_siblings(&previews, None), vec![previews[0].window]);
        // ...and a document never sweeps itself, however demotable it looks.
        assert!(overlay_siblings(&previews, Some(previews[0].window)).is_empty());
    }
}
