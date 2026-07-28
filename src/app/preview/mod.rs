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
pub use layers::PixelFrame;
pub use edit::covermark_dir;
pub(crate) use annotate::AnnotId;

use annotate::Reorder;
use edit::{Covermark, CovermarkKind, EditKind, EditState, Picker, ShareIntent};
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
    /// Where this document last SAVED, when it has (DRAGON-353 follow-up) — the `-edited`
    /// sibling a dirty Save minted, or a Save As destination. `None` = never saved.
    ///
    /// It is the document's IDENTITY on disk (what a further Save overwrites, what a Copy
    /// puts on the clipboard, what the toasts name), while [`Self::path`] stays the media
    /// it renders from. [`App::preview_save_target`] resolves the next save target from
    /// this plus [`Self::save_in_place`].
    pub saved_path: Option<PathBuf>,
    /// Every path this document has WRITTEN, in write order — the `-edited` variants and
    /// any Save As destinations (deduped; [`Self::path`] is not in here, it was written by
    /// the capture, not by us).
    ///
    /// Delete removes exactly this set plus the original (see [`Self::delete_paths`]). It
    /// is TRACKED rather than derived because deriving it would mean globbing for
    /// `<stem>-edited*`, which would sweep up a `-edited` file the user made last week.
    /// Only paths we actually wrote are ours to remove.
    pub written: Vec<PathBuf>,
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
    /// This document's transient success / error notices (DRAGON-353) — always
    /// PER-DOCUMENT, so a toast renders in the surface whose button produced it. See
    /// [`toast`]'s module doc.
    pub toasts: Toasts,
    /// Whether `path` is a save target the USER chose, so a Save writes straight back to
    /// it instead of deriving a fresh `-edited` sibling (DRAGON-353). True after a Save As
    /// (the destination is explicit) and after the first `-edited` save (the document
    /// ADOPTS that file). False for a fresh capture's auto-saved original and for a
    /// `--preview` file — both are protected. The rule itself is
    /// [`naming::save_target`].
    pub save_in_place: bool,
    /// The open-time automatic clipboard copy already ran for this document (DRAGON-353).
    /// The path can arrive later than the surface (a pre-opened spinner), so the copy is
    /// attempted at several seams; this makes it happen exactly once.
    pub copied_on_open: bool,
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

    /// Record a path this document just WROTE, for [`Self::delete_paths`]. Idempotent: a
    /// repeated save to the same target does not stack duplicates.
    pub fn note_written(&mut self, path: &std::path::Path) {
        if !self.written.iter().any(|p| p == path) {
            self.written.push(path.to_path_buf());
        }
    }

    /// EVERY file Delete removes for this document (DRAGON-353 follow-up): the capture it
    /// opened with, plus every path it has written — `-edited` variants and Save As
    /// destinations alike, wherever on disk they landed.
    ///
    /// # The two rulings this encodes
    ///
    /// * **A file we WROTE is fair game; a file we merely OPENED is not.** An external
    ///   `--preview` document returns an EMPTY set even if it saved an `-edited` sibling —
    ///   the Delete button is hidden there anyway, and the user's own file must never be
    ///   removed by an editor they pointed at it. (The saved sibling is spared with it:
    ///   Delete is not offered, so there is no action to attach that cleanup to.)
    /// * **Save As destinations ARE included** (owner's ruling): Delete is a deliberate,
    ///   explicitly-chosen action, and a file this session produced is part of the set the
    ///   document made — including one the user pointed somewhere else. This is why the
    ///   delete toast reports the COUNT: a multi-directory delete must not be silent.
    ///
    /// Tracked, never derived: nothing here is reconstructed from name patterns, so a
    /// pre-existing `shot-edited.png` from an earlier session can never be swept up.
    pub fn delete_paths(&self) -> Vec<PathBuf> {
        if self.external {
            return Vec::new();
        }
        let mut out: Vec<PathBuf> = Vec::new();
        let mut push = |p: &std::path::Path| {
            if !out.iter().any(|q| q == p) {
                out.push(p.to_path_buf());
            }
        };
        if let Some(p) = &self.path {
            push(p);
        }
        for p in &self.written {
            push(p);
        }
        out
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


/// WHAT A COPY MEANS, from the two INDEPENDENT "Automatically save on copy" and
/// "Automatically close on copy" settings (DRAGON-355 split the old combined toggle) — the
/// ONE place the rule lives.
///
/// The four combinations map onto the four copy-family intents: both on is the historical
/// save-and-close (`SaveCopyClose`); save-only writes the document but stays open
/// (`SaveCopy`); close-only copies then closes without touching the saved file
/// (`CopyClose`); both off is a plain `Copy`.
///
/// The toolbar's Copy and the unsaved-changes dialog's Copy both arrive at
/// `PreviewMsg::Copy` and therefore both read this: the dialog's button delegates to the
/// plain toolbar message through [`App::share_then_close`], which only layers "and then
/// close" on top. There is no second implementation to drift.
pub(super) fn copy_intent(save_on_copy: bool, close_on_copy: bool) -> ShareIntent {
    match (save_on_copy, close_on_copy) {
        (true, true) => ShareIntent::SaveCopyClose,
        (true, false) => ShareIntent::SaveCopy,
        (false, true) => ShareIntent::CopyClose,
        (false, false) => ShareIntent::Copy,
    }
}

/// WHAT A DELETE MEANS, from the "Automatically copy to clipboard on delete" setting — the mirror of
/// [`copy_intent`], and read by the toolbar's Delete and the dialog's alike (both land on
/// `PreviewMsg::Delete`).
pub(super) fn delete_intent(copy_on_delete: bool) -> ShareIntent {
    if copy_on_delete { ShareIntent::CopyThenDelete } else { ShareIntent::Delete }
}

/// THE dirty-close gate (DRAGON-353): should a close attempt raise the unsaved-changes
/// dialog instead of closing?
///
/// Yes exactly when the document has unbaked edits AND the dialog is not already up. The
/// second term is what makes the dialog's own buttons work: they re-enter the very close
/// paths this guards (Delete closes, and `share_then_close` clears the flag before acting),
/// so without it a close could bounce off its own dialog forever.
///
/// A clean document always closes straight away — the file on disk is already what the
/// editor is showing, so there is nothing to lose and nothing to ask about.
pub(super) fn close_needs_confirmation(dirty: bool, already_confirming: bool) -> bool {
    dirty && !already_confirming
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
                Task::batch([self.refresh_covermark_for_zoom(id), self.refresh_text_for_zoom(id)])
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
                Task::batch([self.refresh_covermark_for_zoom(id), self.refresh_text_for_zoom(id)])
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
                Task::batch([self.refresh_covermark_for_zoom(id), self.refresh_text_for_zoom(id)])
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
                // A recolored highlight re-renders through the GPU shader (DRAGON-330); a
                // recolored text box re-renders its raster layer (DRAGON-354).
                self.refresh_text_display(id)
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
                // gone, so the document must close once the bake has landed, whatever the
                // intent says — a surfaceless document would otherwise linger as a zombie.
                let deferred_close = std::mem::take(&mut p.edit.close_after_bake);
                let intent = p.edit.pending.take();
                let output = p.edit.pending_output.take();
                if deferred_close
                    && let Some(p) = self.preview_for_mut(id)
                {
                    p.edit.close_after_share = true;
                }
                let Some(intent) = intent else {
                    // No pending intent (shouldn't happen — `begin_bake` always sets one):
                    // still honor a deferred close so the surfaceless document can't linger.
                    return if deferred_close { self.close_preview(id) } else { Task::none() };
                };
                match baked {
                    Some(_) => {
                        // `pending_output` is where the bake wrote; the size it reported is
                        // re-read from disk by the completion seam.
                        self.finish_share_intent(id, intent, output)
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
                // exit before one. Only the PENDING edits are abandoned; every file on disk
                // is untouched.
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.dismiss_close_dialog();
                }
                self.stop_preview_playback(id);
                self.close_preview(id)
            }
            // The dialog's action buttons ACT and THEN close: arm `close_after_share` and
            // delegate to the plain TOOLBAR message, so the share flow never learns about
            // closing and the two entry points can never drift.
            PreviewMsg::SaveAndClose => self.share_then_close(id, PreviewMsg::Save),
            PreviewMsg::SaveAsAndClose => self.share_then_close(id, PreviewMsg::SaveAs),
            PreviewMsg::CopyAndClose => self.share_then_close(id, PreviewMsg::Copy),
            PreviewMsg::DeleteAndClose => self.share_then_close(id, PreviewMsg::Delete),
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
                // DRAGON-353: Save never overwrites an ORIGINAL. A dirty document bakes to
                // its save target — the `-edited` sibling for a capture whose path the user
                // never chose, or the chosen path itself once Save As (or a previous
                // `-edited` save) made it explicit. There is nothing left to confirm, so
                // the old "Overwrite original file?" modal is gone. A CLEAN document is a
                // no-op with a toast; both cases stay in the editor.
                self.run_share(id, ShareIntent::Save)
            }
            PreviewMsg::Copy => {
                // The clipboard gets the edited capture: pending edits bake first (to a
                // throwaway temp, so the saved file stays clean). The two independent
                // settings then layer on save-first ("Automatically save on copy", a real
                // SAVE whose result is copied) and/or close-after ("Automatically close on
                // copy", held so the toast reads and aborted if the copy fails) — DRAGON-355.
                self.run_share(id, copy_intent(self.preview_save_on_copy, self.preview_close_on_copy))
            }
            PreviewMsg::Cancel => {
                // Close without deleting — the file stays where it is. Deleting is the
                // explicit Delete (trash) action.
                //
                // DRAGON-353: with UNSAVED edits this raises the unsaved-changes dialog
                // instead. THE gate for every close path that can still show UI — the
                // Esc / Close-button `Cancel`, the CSD ✕ and the WM close (both routed here
                // through `WindowCloseRequested`). A `WindowClosed` cannot be gated: the
                // surface is already destroyed by then, so there is nowhere to draw.
                if self
                    .preview_for(id)
                    .is_some_and(|p| close_needs_confirmation(p.unsaved(), p.edit.confirm_close))
                {
                    if let Some(p) = self.preview_for_mut(id) {
                        p.edit.confirm_close = true;
                    }
                    return Task::none();
                }
                self.stop_preview_playback(id);
                self.close_preview(id)
            }
            PreviewMsg::ToggleAppearance => self.toggle_preview_appearance(id),
            PreviewMsg::OpenSettings => self.open_settings_from_preview(id),
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
                // DRAGON-353: Delete is the ONE action that still closes its document —
                // "no auto-close" is about the SHARE actions; an editor sitting over a file
                // that no longer exists is nonsense. It closes THIS document only
                // (`close_preview`), so siblings survive and the process ends solely when
                // it was the last one.
                //
                // "Automatically copy to clipboard on delete" (setting, default on) puts the
                // media on the clipboard first, from a STAGED temp so the unlink can never
                // strand the clipboard worker. DRAGON-355: a FAILED copy now ABORTS the delete
                // (the file survives the clipboard miss and the editor stays open) rather than
                // deleting anyway; on success the document closes IMMEDIATELY (DRAGON-371 —
                // the 1s hold that existed so the copy toast could be read is gone).
                //
                // DRAGON-352: the courtesy copy carries the EDITED picture, so this routes
                // through `run_share` (not straight to `finish_share_intent`). A dirty
                // `CopyThenDelete` bakes the scene to a throwaway temp FIRST, and only its
                // `BakeDone` runs the copy-then-delete — so the clipboard gets the composited
                // image, never the untouched base. The ordering falls out of that: the delete
                // lives past the bake by construction, and a bake FAILURE lands in `BakeDone`'s
                // error arm, which aborts the delete (the original survives) and re-raises the
                // failure notice rather than copying the base and destroying the file. A plain
                // Delete (copy setting off) bakes nothing — `begin_bake` returns `None` for it
                // — and completes synchronously, then toasts "Capture deleted" and closes at
                // once (DRAGON-371). That confirmation is therefore not readable in practice:
                // deliberate, since the files are already gone and the surface vanishing says
                // so. The toast still posts because the FAILURE branch beside it keeps the
                // editor open, and there the text is the whole point.
                //
                // Delete closes the document by definition, so an open unsaved-changes
                // dialog is moot the moment it is pressed — dismiss it rather than leaving
                // a modal card floating over the delete's own feedback. (The dialog's own
                // Delete button routes here through `share_then_close`, which already
                // cleared it; this covers the toolbar/hotkey press while the dialog is up.)
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.dismiss_close_dialog();
                }
                self.stop_preview_playback(id);
                self.run_share(id, delete_intent(self.preview_copy_on_delete))
            }
            PreviewMsg::SaveAs => {
                // Ask WHERE to save first — no bake up front. The bake (if any) runs in the
                // background against the chosen destination in `SaveAsResult`, behind the
                // editor's own processing overlay, so the user isn't blocked before the
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
                // (`external` used to matter here: a `--preview` file was COPIED while a
                // fresh capture was MOVED. DRAGON-353 made copy-not-move universal, so the
                // distinction is gone — see the export note below.)
                let processing_msg = random_processing_msg();
                let (src, covermark, annotations, annot_curve, dim, video, is_video, dirty) = match self.preview_for(id) {
                    Some(p) => {
                        let Some(src) = p.path.clone() else {
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
                        // Annotations + dim are IMAGES only; a video never accumulates them.
                        (src, p.edit.covermark.clone(), p.edit.annotations.clone(), p.edit.curve_radius(), p.edit.dim, video, is_video, p.dirty())
                    }
                    None => return self.close_preview(id),
                };
                // Only bake when there's something to apply AND we can (video needs meta).
                // `dirty()` is THE shared gate (the one Save/Copy's `begin_bake` reads):
                // covermark / annotations / dim / DELETED timeline content — razor cuts
                // alone never re-encode (DRAGON-352 unification; two parallel predicates
                // here had already begun to drift).
                let can_bake = dirty && (!is_video || video.is_some());
                // Mark the export in flight (DRAGON-352): the SAME single-flight `baking`
                // guard the bake path uses, so a `WindowClosed` mid-export DEFERS
                // (`close_after_bake`) instead of exiting with the worker mid-write —
                // which could truncate the destination file. `SaveAsBaked` clears it. It
                // also raises the editor's processing overlay (DRAGON-353).
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.baking = true;
                    p.edit.processing_msg = processing_msg;
                }
                // Export in the BACKGROUND: bake straight to the destination, or plainly
                // COPY when nothing needs baking. Await it via a task only so the app
                // stays alive until the file lands.
                //
                // DRAGON-353: never a MOVE. Save As RETARGETS the document at `dest` (a
                // later Save writes there, with no `-edited` derivation — the user chose
                // that path), and the source file is left exactly where it was. Moving it
                // would delete a fresh capture's auto-saved original behind the user's
                // back, which is the very thing the `-edited` rule exists to prevent.
                let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
                let bake_dest = dest.clone();
                std::thread::spawn(move || {
                    let dest = bake_dest;
                    let ok = if can_bake {
                        let result = match &video {
                            Some(v) => edit::bake_video(&src, &dest, covermark.as_ref(), v),
                            None => edit::bake_image(
                                &src,
                                &dest,
                                covermark.as_ref(),
                                &annotations,
                                annot_curve,
                                dim,
                            ),
                        };
                        // Log the real io::Error here — it's about to be discarded to a bool.
                        if let Err(e) = &result {
                            log::warn!("preview edit bake failed (Save As): {e}");
                        }
                        result.is_ok()
                    } else {
                        // Nothing to bake: copy the file as it stands. Saving over ITSELF
                        // (dest == src) would truncate it, so that degenerate pick is a
                        // success with no work.
                        let same_file = std::fs::canonicalize(&src)
                            .ok()
                            .zip(std::fs::canonicalize(&dest).ok())
                            .is_some_and(|(a, b)| a == b);
                        same_file || std::fs::copy(&src, &dest).is_ok()
                    };
                    if ok {
                        crate::platform::services::notify(&dest, false);
                    }
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
                // DRAGON-353: Save As RETARGETS. The chosen destination becomes the
                // document's working file (`save_in_place`), so a later Save writes THERE
                // — with no `-edited` derivation, because the user picked that path — and
                // the source file is left untouched. The editor stays open on the
                // destination, reloaded so the committed pixels are the new baseline
                // (carrying the old edit state forward would apply them twice).
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
                // RETARGET, never reload — the same rule as the in-place Save
                // (`finish_share_intent` step 1, DRAGON-353 follow-up). The document keeps
                // rendering its untouched media plus the live scene, so the undo history
                // survives the export; only the save-side bookkeeping moves. The
                // destination joins `written` because Delete removes every file this
                // document produced, wherever the user pointed it.
                if let Some(p) = self.preview_for_mut(id) {
                    p.saved_path = Some(dest.clone());
                    p.size = Some(size);
                    p.save_in_place = true;
                    p.note_written(&dest);
                    p.edit.mark_saved();
                }
                if close_after {
                    return self.close_preview(id);
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

    /// TASK-8 UNIFICATION: the unsaved-changes dialog's buttons and the action bar's
    /// buttons resolve to the SAME behaviour for the same settings, because they dispatch
    /// the same messages — `share_then_close` only layers "and then close" onto the plain
    /// toolbar message. This pins the settings-driven halves of that: a dialog Delete
    /// honours "Automatically copy to clipboard on delete" and a dialog Copy honours the
    /// two independent save-on-copy / close-on-copy settings (DRAGON-355), exactly as the
    /// toolbar does.
    #[test]
    fn the_dialog_and_the_toolbar_resolve_the_same_intents() {
        // Delete, both entry points → one rule.
        assert_eq!(delete_intent(true), ShareIntent::CopyThenDelete);
        assert_eq!(delete_intent(false), ShareIntent::Delete);
        assert!(delete_intent(true).copies(), "copy-on-delete must actually copy");
        assert!(delete_intent(true).deletes() && delete_intent(false).deletes());
        assert!(!delete_intent(false).copies(), "with the setting off, no copy is attempted");
        // Copy, the four (save, close) combinations → four distinct intents (DRAGON-355).
        assert_eq!(copy_intent(true, true), ShareIntent::SaveCopyClose);
        assert_eq!(copy_intent(true, false), ShareIntent::SaveCopy);
        assert_eq!(copy_intent(false, true), ShareIntent::CopyClose);
        assert_eq!(copy_intent(false, false), ShareIntent::Copy);
        // save-on-copy drives `saves()`, independent of close.
        for close in [true, false] {
            assert!(copy_intent(true, close).saves(), "save-on-copy must save");
            assert!(!copy_intent(false, close).saves(), "no save-on-copy, no save");
            assert!(copy_intent(false, close).copies(), "a Copy always copies");
        }
        // close-on-copy drives `closes_document()`, independent of save.
        for save in [true, false] {
            assert!(copy_intent(save, true).closes_document(), "close-on-copy must close");
            assert!(!copy_intent(save, false).closes_document(), "no close-on-copy, no close");
        }
        // A save WITHOUT close leaves the saved file written but the editor open, and does
        // NOT bake to a temp (it is a real save); a close WITHOUT save copies from a temp so
        // the saved file is left alone.
        assert!(copy_intent(true, false).saves() && !copy_intent(true, false).bakes_to_temp());
        assert!(!copy_intent(false, true).saves() && copy_intent(false, true).bakes_to_temp());
        // Every intent a DELETE resolves to closes the document by itself, which is why the
        // dialog's Delete needs no close-after of its own (and can never close twice).
        for on in [true, false] {
            assert!(delete_intent(on).closes_document());
        }
        // A plain Copy never closes. A toolbar Copy therefore never closes a dirty document
        // WITHOUT the settings saying so; and a close-without-save is protected by the
        // unsaved-changes guard in `finish_share_intent` (verified by the share-flow logic).
        assert!(!copy_intent(false, false).closes_document());
        assert!(copy_intent(true, true).closes_document() && copy_intent(true, true).saves());
    }

    /// A document that opened on `capture` and has since WRITTEN `written`, in order.
    fn doc_with(capture: Option<&str>, written: &[&str], external: bool) -> PreviewState {
        let mut p = still_at((100, 100), 1.0);
        p.path = capture.map(PathBuf::from);
        p.external = external;
        for w in written {
            p.note_written(std::path::Path::new(w));
        }
        p
    }

    /// THE over-delete guard (DRAGON-353 follow-up): Delete removes the capture plus every
    /// path the document actually WROTE — and nothing else, ever. Nothing here is derived
    /// from a name pattern, so a `-edited` file from an earlier session cannot be caught.
    #[test]
    fn delete_covers_every_file_the_document_wrote_and_no_others() {
        // (a) Never saved: just the capture.
        assert_eq!(
            doc_with(Some("/shots/a.png"), &[], false).delete_paths(),
            vec![PathBuf::from("/shots/a.png")]
        );
        // (b) Saved once to the `-edited` sibling: both.
        assert_eq!(
            doc_with(Some("/shots/a.png"), &["/shots/a-edited.png"], false).delete_paths(),
            vec![PathBuf::from("/shots/a.png"), PathBuf::from("/shots/a-edited.png")]
        );
        // (c) Saved TWICE: the second save writes the same adopted file, so the set does
        // not grow (the tracking is deduped).
        assert_eq!(
            doc_with(
                Some("/shots/a.png"),
                &["/shots/a-edited.png", "/shots/a-edited.png"],
                false
            )
            .delete_paths(),
            vec![PathBuf::from("/shots/a.png"), PathBuf::from("/shots/a-edited.png")]
        );
        // (d) Save As elsewhere: the chosen destination IS ours to remove (owner's ruling —
        // Delete is a deliberate action and this file is one this session produced), even
        // though it lives in another directory entirely.
        assert_eq!(
            doc_with(
                Some("/shots/a.png"),
                &["/shots/a-edited.png", "/home/me/final.png"],
                false
            )
            .delete_paths(),
            vec![
                PathBuf::from("/shots/a.png"),
                PathBuf::from("/shots/a-edited.png"),
                PathBuf::from("/home/me/final.png"),
            ]
        );
        // (e) An EXTERNAL `--preview` document deletes NOTHING — not the file it merely
        // opened, and not a sibling it saved beside it. Delete isn't even offered there.
        assert!(
            doc_with(Some("/home/me/theirs.png"), &["/home/me/theirs-edited.png"], true)
                .delete_paths()
                .is_empty()
        );
        // A pre-existing `-edited` file that this document never wrote is untouched — it
        // simply never enters the set, because the set is tracked and not derived.
        let d = doc_with(Some("/shots/a.png"), &[], false);
        assert!(!d.delete_paths().contains(&PathBuf::from("/shots/a-edited.png")));
        // A document with no file at all has nothing to delete.
        assert!(doc_with(None, &[], false).delete_paths().is_empty());
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
            toasts: Toasts::default(),
            save_in_place: false,
            copied_on_open: false,
            demoted: false,
            saved_path: None,
            written: Vec::new(),
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

    // ── DRAGON-353: the share-action model ──────────────────────────────────────

    /// THE intent table. Each action is defined by WHICH of the four steps it performs,
    /// and `finish_share_intent` runs exactly those, in this order. Pinning the table here
    /// is what stops a fifth flavour from quietly acquiring (or losing) a step.
    #[test]
    fn every_share_intent_declares_exactly_what_it_does() {
        use ShareIntent::*;
        // (intent, saves, copies, deletes, closes, bakes to a temp)
        let table = [
            (Save, true, false, false, false, false),
            (Copy, false, true, false, false, true),
            (SaveCopyClose, true, true, false, true, false),
            // DRAGON-355 split the old save-&-close into these two independent halves:
            (SaveCopy, true, true, false, false, false),
            (CopyClose, false, true, false, true, true),
            (CopyThenDelete, false, true, true, true, true),
            (Delete, false, false, true, true, false),
        ];
        for (i, saves, copies, deletes, closes, temp) in table {
            assert_eq!(i.saves(), saves, "{i:?}.saves()");
            assert_eq!(i.copies(), copies, "{i:?}.copies()");
            assert_eq!(i.deletes(), deletes, "{i:?}.deletes()");
            assert_eq!(i.closes_document(), closes, "{i:?}.closes_document()");
            assert_eq!(i.bakes_to_temp(), temp, "{i:?}.bakes_to_temp()");
        }
    }

    /// The headline rule of the ticket: **no share action closes the editor by default**.
    /// Only the SETTINGS-driven flavours (close-on-copy in any of its combinations, and
    /// copy-on-delete) and the plain Delete do — and Delete only because there is no file
    /// left to edit. A save-on-copy WITHOUT close-on-copy (`SaveCopy`, DRAGON-355) stays open.
    #[test]
    fn a_plain_share_never_closes_the_document() {
        assert!(!ShareIntent::Save.closes_document());
        assert!(!ShareIntent::Copy.closes_document());
        assert!(!ShareIntent::SaveCopy.closes_document(), "save-on-copy alone must not close");
        // ...while every closing flavour is one the user opted into, or a delete.
        for i in [
            ShareIntent::SaveCopyClose,
            ShareIntent::CopyClose,
            ShareIntent::CopyThenDelete,
            ShareIntent::Delete,
        ] {
            assert!(i.closes_document(), "{i:?}");
        }
    }

    /// A COPY must never persist edits into the saved file UNLESS it is a save: the
    /// non-saving copy flavours bake to a throwaway temp. The saving copy flavours
    /// (`SaveCopyClose`, `SaveCopy`) are the deliberate exception — they ARE saves, and the
    /// clipboard then gets the very bytes that landed on disk (one bake, not two).
    #[test]
    fn copying_leaves_the_saved_file_alone_unless_it_is_a_save() {
        // Non-saving copies bake to a temp and do not save.
        for i in [ShareIntent::Copy, ShareIntent::CopyThenDelete, ShareIntent::CopyClose] {
            assert!(i.bakes_to_temp() && !i.saves(), "{i:?} must copy from a temp");
        }
        // Saving copies write the save target instead (no temp).
        for i in [ShareIntent::SaveCopyClose, ShareIntent::SaveCopy] {
            assert!(i.saves() && !i.bakes_to_temp(), "{i:?} copies the saved bytes");
        }
    }

    /// DRAGON-352: the bug this branch fixes. A dirty `CopyThenDelete` MUST owe a bake so the
    /// courtesy copy carries the composited picture, not the untouched base — the whole point
    /// of routing Delete through `run_share`/`begin_bake` instead of straight to
    /// `finish_share_intent`. The rest of the table pins the rule so no flavour drifts:
    ///
    /// * copy-to-temp flavours (`Copy`, `CopyThenDelete`) bake on `dirty` ALONE — the temp is
    ///   new, so any scene needs rendering onto it, saved-or-not;
    /// * saving flavours (`Save`, `SaveCopyClose`) bake only when `dirty && unsaved` (a save
    ///   standing on its own save point is the clean-save no-op);
    /// * a plain `Delete` never bakes, however dirty — the file is being discarded.
    #[test]
    fn copy_on_delete_bakes_the_edits_and_plain_delete_never_does() {
        use ShareIntent::*;
        // The headline: a dirty copy-on-delete owes a bake (so the clipboard gets the edits);
        // a clean one does not (the base IS what's displayed).
        assert!(CopyThenDelete.owes_bake(true, true), "dirty copy-on-delete must bake the edits");
        assert!(CopyThenDelete.owes_bake(true, false), "saved-but-dirty still bakes to the temp");
        assert!(!CopyThenDelete.owes_bake(false, true), "a clean copy-on-delete copies the base");

        // A plain Delete never bakes — the source is about to be unlinked and nothing reads
        // the output.
        for (dirty, unsaved) in [(true, true), (true, false), (false, true), (false, false)] {
            assert!(!Delete.owes_bake(dirty, unsaved), "plain Delete must never bake");
        }

        // Copy (to temp) tracks `dirty` alone, exactly like CopyThenDelete.
        assert!(Copy.owes_bake(true, false) && !Copy.owes_bake(false, false));

        // Saving flavours need BOTH dirty and unsaved (the clean-save no-op otherwise).
        for i in [Save, SaveCopyClose] {
            assert!(i.owes_bake(true, true), "{i:?} bakes when there is uncommitted work");
            assert!(!i.owes_bake(true, false), "{i:?} on its own save point is a no-op");
            assert!(!i.owes_bake(false, true), "{i:?} with no edits is a no-op");
        }
    }

    /// The unsaved-changes gate: a dirty close asks first, a clean one just goes — and a
    /// close attempted while the dialog is ALREADY up must not re-raise it, or the dialog's
    /// own buttons (which re-enter these paths) could never get past themselves.
    #[test]
    fn only_a_dirty_close_asks_and_only_once() {
        assert!(close_needs_confirmation(true, false), "unsaved edits must be confirmed");
        assert!(!close_needs_confirmation(false, false), "a clean document just closes");
        assert!(!close_needs_confirmation(true, true), "the dialog must not re-raise itself");
        assert!(!close_needs_confirmation(false, true));
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
