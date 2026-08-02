//! Preview-overlay editing: a single covermark overlay (with zoom + undo/redo).
//!
//! The covermark is NON-destructive until a share action (Save / Save As / Copy)
//! bakes it into the file: an image is re-encoded in place from its decoded pixels;
//! a video is re-encoded through an `ffmpeg` `overlay` filter graph. Undo/redo moves
//! the covermark between history stacks — the display recomposites from the untouched
//! original (image) or stacks the covermark over the frame (video), so nothing is
//! lost until the user commits by sharing.

use super::annotate::{AnnotGesture, AnnotColor, AnnotId, AnnotationItem, ToolClicks};
use super::crop::CropRect;
use super::layers::RasterSlot;
use super::timeline::{Span, Timeline};
use crate::widgets::annotation_canvas::Tool;
use crate::widgets::crop_canvas::CropHandle;
use ::image::RgbaImage;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The built-in "CONFIDENTIAL" covermark (also installed for packaging; embedded
/// so the default choice exists on every install).
const CONFIDENTIAL_SVG: &[u8] = include_bytes!("../../../res/covermarks/confidential.svg");

/// What a covermark draws.
#[derive(Clone, Debug, PartialEq)]
pub enum CovermarkKind {
    /// The built-in tiled red/white "CONFIDENTIAL" mark.
    Confidential,
    /// A custom tiled gray text mark (text snapshotted from settings when applied).
    Text(String),
    /// A user-supplied SVG from the covermarks folder.
    File(PathBuf),
}

impl CovermarkKind {
    /// Display name for the picker. The custom-text mark shows its configured text
    /// (unless blank once trimmed, then a generic label).
    pub fn name(&self) -> String {
        match self {
            CovermarkKind::Confidential => "Confidential".into(),
            CovermarkKind::Text(t) if !t.trim().is_empty() => t.trim().to_string(),
            CovermarkKind::Text(_) => "Custom text".into(),
            CovermarkKind::File(p) => p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "covermark".into()),
        }
    }

    /// A stable key for remembering this option's zoom/opacity independently of the
    /// others. The "Custom text" option shares one slot (its text can change); files key
    /// by path; the built-in mark is its own slot.
    pub fn pref_key(&self) -> String {
        match self {
            CovermarkKind::Confidential => "confidential".to_string(),
            CovermarkKind::Text(_) => "text".to_string(),
            CovermarkKind::File(p) => format!("file:{}", p.display()),
        }
    }

    /// The SVG bytes this kind renders (generated for `Text`, read for `File`).
    fn svg(&self) -> Option<std::borrow::Cow<'static, [u8]>> {
        match self {
            CovermarkKind::Confidential => Some(std::borrow::Cow::Borrowed(CONFIDENTIAL_SVG)),
            CovermarkKind::Text(text) => Some(std::borrow::Cow::Owned(text_svg(text).into_bytes())),
            CovermarkKind::File(p) => std::fs::read(p).ok().map(std::borrow::Cow::Owned),
        }
    }
}

/// A covermark applied to the capture: what it draws, a zoom factor (0 = the
/// default cover fit; higher enlarges the pattern while still filling the frame),
/// and an opacity (0..1) applied to the whole mark at composite time.
#[derive(Clone, Debug, PartialEq)]
pub struct Covermark {
    pub kind: CovermarkKind,
    pub zoom: f32,
    pub opacity: f32,
}

// DRAGON-467 removed `ShareIntent` from here. It named which of save / copy / delete a
// share was running, and once carried seven flavours, one per combination of the
// "Automatically save on copy" / "close on copy" / "copy to clipboard on delete" settings.
// Those settings went first, then Save moved to the destination picker
// (`PreviewMsg::SaveAsResult`), then Delete left the editor entirely (user decision: "not
// needed anymore" — with "Automatically save originals" off an unwanted capture never
// reaches the user's folder, so closing IS the discard). One action was left, so the enum
// said nothing and its callers now name the copy directly.

/// What a copy-flavoured share owes the baker, given what the last bake already produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BakeNeed {
    /// Nothing to render: the document is clean, so the file as it stands IS the answer.
    None,
    /// The last bake's artifact is EXACTLY this state — copy that instead of producing it
    /// again.
    Reuse,
    /// Render it.
    Fresh,
}

/// THE re-bake gate (DRAGON-467 review, major 4). Pure; unit-tested in `bake_need_tests`.
///
/// Without it, "Save, then Escape" ran the whole scene through the encoder a SECOND time
/// (the exit copy), because `dirty()` stays true after a save by design — it asks "does an
/// export have to re-encode?", and the answer is still yes for a scene that has annotations
/// in it. For a still that is a wasted composite; for a recording it is a second full ffmpeg
/// pass over the whole take, behind a spinner, on the way out of the editor.
///
/// `baked_at` is [`EditState::baked`]'s history position. Equal to the current `depth` means
/// the artifact on disk was rendered from exactly the scene now on screen, so it can be
/// served directly. Any edit moves `depth`, and [`EditState::push_op`] drops a marker that
/// ends up on an abandoned redo branch, so a stale artifact can never be reused.
pub fn bake_need(dirty: bool, depth: usize, baked_at: Option<usize>) -> BakeNeed {
    if !dirty {
        BakeNeed::None
    } else if baked_at == Some(depth) {
        BakeNeed::Reuse
    } else {
        BakeNeed::Fresh
    }
}

/// Whether the clipboard ALREADY holds this exact state of the document (DRAGON-467 review,
/// major 4). Pure; unit-tested in `bake_need_tests`.
///
/// The second half of the same economy: an explicit toolbar Copy followed by Escape must not
/// copy twice. `copied_depth` is [`EditState::copied_depth`], set by every successful copy
/// (including the automatic one the editor performs as it opens, which lands at depth 0).
pub fn clipboard_is_current(copied_depth: Option<usize>, depth: usize) -> bool {
    copied_depth == Some(depth)
}

/// What must happen BEFORE a bake whose destination is the document's own bake source
/// (DRAGON-467 review, blocker 1). Pure; unit-tested in `bake_prep_tests`.
///
/// # The invariant this defends
///
/// Every bake reads PRISTINE media and composites the whole live scene onto it. That is what
/// makes the editor non-destructive: `path` stays pinned to the untouched capture, the scene
/// stays live on top, and the undo history survives a save. It also means a bake that reads
/// ALREADY-BAKED bytes doubles every annotation, and for a recording re-applies the kept
/// spans to a file the cut has already been taken out of.
///
/// Before DRAGON-467 that could not happen: a dirty Save wrote the `-edited` sibling, so the
/// destination was never the source. With Save asking for a destination and pre-filling the
/// overwrite target, saving in place is now the DEFAULT gesture, and the very next bake (an
/// exit copy, a second save, the ask-card's Copy) would have read its own output.
///
/// The two media kinds get different answers because the cost is different:
///
/// * A STILL snapshots its pristine bytes into the runtime directory and permanently
///   repoints the bake source at the snapshot. Cheap (one file copy of a few MB), and the
///   scene stays fully editable after the save: undo, retouch, save again.
/// * A RECORDING must not be copied, since a take can be multi-GB. It COMMITS instead: bake
///   through a temp, rename over the destination, then repoint the document at the result
///   and RESET the scene, because the file now IS the edit. The undo history goes with it,
///   which is why the user is told.
pub fn bake_prep(dest_is_source: bool, is_video: bool) -> BakePrep {
    if !dest_is_source {
        BakePrep::Direct
    } else if is_video {
        BakePrep::CommitVideo
    } else {
        BakePrep::SnapshotStill
    }
}

/// The answers [`bake_prep`] gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BakePrep {
    /// The destination is a different file: bake straight to it, nothing to protect.
    Direct,
    /// A STILL saving over its own source: snapshot the pristine bytes aside first and
    /// repoint the bake source at the snapshot.
    SnapshotStill,
    /// A RECORDING saving over its own source: bake through a temp, rename over the
    /// destination, then commit-and-reset.
    CommitVideo,
}

/// The covermark picker's entries while open (a dropdown under the covermark button). The
/// keyboard highlight/nav lives in the SHARED [`FlyoutNav`] (see [`EditState::flyout`]).
pub struct Picker {
    /// The choices, in display order. `None` is the "None" (disable) card, always
    /// first; `Some(kind)` are the real covermarks.
    pub entries: Vec<Option<CovermarkKind>>,
}

/// Which keyboard-navigable toolbar flyout is open. Both flyouts drive the SAME small state
/// machine ([`FlyoutNav`]) + the same `preview_modal_key` dispatch; only the entry LIST and
/// the apply ACTION differ per kind. A future third flyout adds a variant + its panel +
/// entry list + a hotkey, reusing all the open/nav/select/display plumbing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlyoutKind {
    /// The covermark picker (bottom bar).
    Covermark,
    /// The annotation color palette (top bar).
    Color,
    /// The TEXT-size dropdown (top bar, DRAGON-354).
    TextSize,
    /// The TEXT-font dropdown (top bar, DRAGON-357 item 16): Hand / Clean.
    TextFont,
}

/// The shared open/nav state of a toolbar flyout: which one is open, the highlighted entry
/// index (`None` = nothing highlighted yet), and the entry count (for wrap-around).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FlyoutNav {
    pub kind: FlyoutKind,
    pub selected: Option<usize>,
    pub len: usize,
}

impl FlyoutNav {
    /// Move the highlight by `delta`, wrapping. From "no highlight", +1 → first, −1 → last.
    pub fn nav(&mut self, delta: i32) {
        if self.len == 0 {
            return;
        }
        let n = self.len as i32;
        let base = self.selected.map(|s| s as i32).unwrap_or(if delta >= 0 { -1 } else { 0 });
        self.selected = Some(((base + delta).rem_euclid(n)) as usize);
    }
}

/// The annotation SELECTION (DRAGON-341): an ordered set of ids whose LAST member is the
/// PRIMARY — the one wearing resize handles and the one single-item operations (resize,
/// duplicate, reorder) act on. Everything that acts on "the selection" as a whole (move,
/// delete, recolor, re-stroke) walks [`Self::ids`] instead, as ONE undo entry.
///
/// Order is selection order, so the newest pick is always primary: a plain click makes its item
/// the only (and primary) member; a Ctrl/Shift-click appends (or removes) one; a rubber band
/// appends everything it touched. Duplicates are impossible by construction.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Selection {
    ids: Vec<AnnotId>,
}

impl Selection {
    /// The PRIMARY selected id — the most recently added. `None` when nothing is selected.
    pub fn primary(&self) -> Option<AnnotId> {
        self.ids.last().copied()
    }

    /// Every selected id, in selection order (primary last).
    pub fn ids(&self) -> &[AnnotId] {
        &self.ids
    }

    pub fn contains(&self, id: AnnotId) -> bool {
        self.ids.contains(&id)
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Drop the whole selection.
    pub fn clear(&mut self) {
        self.ids.clear();
    }

    /// Replace the selection with exactly `id` (a plain click).
    pub fn set_one(&mut self, id: AnnotId) {
        self.ids.clear();
        self.ids.push(id);
    }

    /// Replace the selection with `ids`, de-duplicated in first-seen order (Ctrl+A, a
    /// non-additive rubber band).
    pub fn set_all(&mut self, ids: impl IntoIterator<Item = AnnotId>) {
        self.ids.clear();
        self.add_all(ids);
    }

    /// Add `ids` to the selection, skipping ones already in it (an additive rubber band).
    pub fn add_all(&mut self, ids: impl IntoIterator<Item = AnnotId>) {
        for id in ids {
            if !self.ids.contains(&id) {
                self.ids.push(id);
            }
        }
    }

    /// Ctrl/Shift-click: remove `id` when already selected, else append it — so a freshly
    /// added item becomes the PRIMARY and immediately shows its handles.
    pub fn toggle(&mut self, id: AnnotId) {
        match self.ids.iter().position(|x| *x == id) {
            Some(i) => {
                self.ids.remove(i);
            }
            None => self.ids.push(id),
        }
    }

    /// Drop any id that is no longer in `items` — called after a mutation that can remove
    /// items, so the selection can never point at a deleted annotation.
    pub fn retain_existing(&mut self, items: &[AnnotationItem]) {
        self.ids.retain(|id| items.iter().any(|it| it.id == *id));
    }

    /// Drop every id that fails `keep`, preserving the order of the rest — so the primary
    /// simply falls back to the last SURVIVOR. Used to let pen groups go when pointer mode ends.
    pub fn retain(&mut self, keep: impl Fn(AnnotId) -> bool) {
        self.ids.retain(|id| keep(*id));
    }
}

/// One undoable preview edit — the SHARED history holds both kinds in order,
/// so Ctrl+Z walks covermark changes and timeline cuts/deletes interleaved,
/// newest first, exactly as they were made.
#[derive(Clone, Debug, PartialEq)]
pub enum EditOp {
    /// A covermark change: the covermark state BEFORE the change.
    Covermark(Option<Covermark>),
    /// A timeline cut/delete: the kept spans BEFORE the change.
    Timeline(Vec<Span>),
    /// An annotation-scene change (add/move/resize/delete/reorder): the whole scene
    /// BEFORE the change. A continuous drag pushes ONE entry on gesture-commit.
    Annotations(Vec<AnnotationItem>),
    /// A global dim change (DRAGON-329): the dim value (0..1) BEFORE the change. One entry per
    /// slider DRAG (coalesced via [`EditState::dim_drag_start`]), like an annotation gesture.
    Dim(f32),
    /// A crop change (DRAGON-382): the committed crop BEFORE the change (`None` = un-cropped).
    /// One entry per crop-session Accept.
    Crop(Option<CropRect>),
}

/// Which display artifact an undo/redo step touched, so the caller refreshes the right
/// raster: a covermark or annotation re-raster is owed; a timeline change redraws for free.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    Covermark,
    Timeline,
    Annotations,
    /// A global dim change — redraws for free through the GPU dim pass on the next view build.
    Dim,
    /// A crop change (DRAGON-382) — applied only at bake; the main view redraws for free.
    Crop,
}

/// The transient crop-tool SESSION (DRAGON-382): a WORKING COPY of the crop rect being edited,
/// the viewport saved at session start (restored on Accept/Cancel), and the active drag. `Some`
/// on [`EditState::crop_session`] is what makes the crop overlay draw and swallows the tool
/// hotkeys (the crop UI owns the surface, like a live text edit).
#[derive(Clone, Debug)]
pub struct CropSession {
    /// The live crop rect (SOURCE px) — Accept commits it, Cancel discards it.
    pub rect: CropRect,
    /// The viewport at session start; restored when the session ends.
    pub saved_view: super::viewport::Viewport,
    /// The ARMED TOOL at session start, restored when the session ends (DRAGON-392 correction).
    /// The session disarms the tray — nothing may look armed while the crop owns the canvas — and
    /// hands the tool back on BOTH exits, accept and cancel alike: which tool you were holding has
    /// nothing to do with the crop's outcome. `None` (the neutral state) round-trips as `None`,
    /// which is why this is the whole `Option` rather than "a tool if there was one".
    ///
    /// SESSION state, like [`Self::saved_view`] beside it: it lives and dies with the session and
    /// never reaches the settings store.
    pub saved_tool: Option<Tool>,
    /// The in-flight drag, `None` between drags.
    pub drag: Option<CropDrag>,
}

/// One in-flight crop drag: the grabbed handle plus the rect and pointer image point at press,
/// so each motion resolves against the drag START (see [`super::crop::resolve_drag`]).
#[derive(Clone, Copy, Debug)]
pub struct CropDrag {
    pub handle: CropHandle,
    pub orig: CropRect,
    pub press: (f32, f32),
}

/// A LIVE, debounced preview edit driven by a DRAGGING slider — the shared live-slider path
/// (covermark zoom/opacity today; any future RASTER-backed slider after). The dim/spotlight
/// slider (DRAGON-329) is NOT here: the dim renders straight from the model through the GPU
/// dim pass every view build (like the annotation effects), so it needs no off-thread raster.
/// Every raster-backed slider wires up identically, so no debounce is hand-rolled per control:
///   * each drag TICK updates the model value, then calls [`super::App::refresh_live_edit`],
///     which kicks a COALESCED off-thread re-raster of that edit's [`RasterSlot`]. The slot's
///     begin/finish coalescing IS the debounce — at most one raster is ever in flight, and
///     however many ticks arrive while it runs collapse into exactly ONE pending re-run — so a
///     fast drag re-renders CONTINUOUSLY without thrashing the GPU, and blink-free (the
///     persistent-texture shader updates in place).
///   * RELEASE fires the same refresh once more, so the final settled value is always rendered.
///
/// Add a live edit: add a variant here plus its raster-slot arm in
/// [`super::App::refresh_live_edit`]; the slider + handler wiring is then identical.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LiveEdit {
    /// The covermark overlay — its zoom + opacity sliders share the one `cm_raster` slot.
    Covermark,
}

/// The in-flight text-annotation editing session (DRAGON-354): which item is open, the caret
/// position (CHAR index into the item's text), the pre-edit scene for the ONE undo entry the
/// settle pushes, whether the box was just created (so an empty settle DISCARDS it), and the
/// caret blink phase. Held on [`EditState::text_edit`]; `Some` is what routes keystrokes to
/// the editor and suspends the tool hotkeys.
#[derive(Clone, Debug)]
pub struct TextEdit {
    pub id: AnnotId,
    /// Caret position as a CHAR index into the item's text (0..=char_len).
    pub caret: usize,
    /// The OTHER end of an active text selection (a CHAR index), or `None` for a bare caret
    /// (DRAGON-354 item 12). The selected range is `[min(anchor, caret), max(anchor, caret))`;
    /// a drag from a press point, or Shift+arrows, extends it. Typing / Backspace / paste act
    /// on the whole range when it is non-empty.
    pub anchor: Option<usize>,
    /// The scene BEFORE this edit began — pushed as one `EditOp::Annotations` on a changed
    /// settle (and never pushed when nothing changed / the box is discarded).
    pub snapshot: Vec<AnnotationItem>,
    /// The box was created by THIS session (a fresh click/drag): an empty settle removes it
    /// with no undo entry, exactly like a degenerate shape.
    pub is_new: bool,
    /// Whether the caret is on this blink tick (toggled by the blink subscription).
    pub blink_on: bool,
    /// The IN-SESSION text undo/redo history (DRAGON-354 item 13): Cmd/Ctrl+Z steps back through
    /// this session's typing/paste/cut/replace, Shift+Cmd/Ctrl+Z forward, WITHOUT touching the
    /// shared `EditOp` stack. The session still settles into exactly ONE `EditOp::Annotations`
    /// entry (see `settle_text_edit`), so the GLOBAL history stays one-entry-per-text-edit; this
    /// stack is scoped to the open session and is dropped when it settles. Exhausting it makes a
    /// further Cmd+Z a NO-OP (never a settle-and-pop of the global history mid-edit).
    pub history: TextEditHistory,
}

/// One reversible state of a text-edit session (DRAGON-354 item 13): the box's whole buffer plus
/// the caret and selection anchor at that moment. Small by construction (the buffer is capped at
/// the 32KB paste limit), so snapshotting per input event is cheap.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TextSnapshot {
    pub text: String,
    pub caret: usize,
    pub anchor: Option<usize>,
}

/// The per-session text undo/redo stacks (DRAGON-354 item 13) — a small state machine, kept PURE
/// (no window/annotation coupling) so its push/undo/redo/coalesce/clear-on-input behaviour is
/// unit-testable. Consecutive single-character typing COALESCES into one step (a burst); a
/// word-break (space/newline), a caret move, or any non-typing edit ends the burst so the next
/// char starts a fresh step.
#[derive(Clone, Debug, Default)]
pub struct TextEditHistory {
    undo: Vec<TextSnapshot>,
    redo: Vec<TextSnapshot>,
    /// A single-character typing burst is in progress: further coalescing typing extends the
    /// current step instead of pushing a new one.
    typing_burst: bool,
}

impl TextEditHistory {
    /// Record the PRE-edit `snapshot` before a mutation. `coalesce` = this mutation is
    /// single-character typing that should merge with an ongoing burst. Any recorded input clears
    /// the redo stack (standard redo-dies-on-new-input semantics).
    pub fn record(&mut self, snapshot: TextSnapshot, coalesce: bool) {
        if !(coalesce && self.typing_burst) {
            self.undo.push(snapshot);
        }
        self.redo.clear();
        self.typing_burst = coalesce;
    }

    /// End any ongoing typing burst (a caret move / non-typing op) so the next typing character
    /// begins a fresh undo step.
    pub fn break_burst(&mut self) {
        self.typing_burst = false;
    }

    /// Undo: given the session's `current` state, pop the prior snapshot (pushing `current` onto
    /// the redo stack) — or `None` when the session stack is exhausted (a NO-OP; the global
    /// history is never touched mid-edit).
    pub fn undo(&mut self, current: TextSnapshot) -> Option<TextSnapshot> {
        let prev = self.undo.pop()?;
        self.redo.push(current);
        self.typing_burst = false;
        Some(prev)
    }

    /// Redo: given the session's `current` state, pop the next snapshot (pushing `current` back
    /// onto the undo stack) — or `None` when there is nothing to redo.
    pub fn redo(&mut self, current: TextSnapshot) -> Option<TextSnapshot> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        self.typing_burst = false;
        Some(next)
    }
}

impl TextEdit {
    /// The active selection as a CHAR range `[start, end)`, or `None` for a bare caret (no
    /// anchor, or the anchor collapsed onto the caret). Order-normalized.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.caret).then(|| (a.min(self.caret), a.max(self.caret)))
    }
}

/// The preview's edit state — shared by image and video previews.
#[derive(Default)]
pub struct EditState {
    /// The active covermark (kind + zoom), or `None`.
    pub covermark: Option<Covermark>,
    /// Prior edit states (undo pops from here), covermark + timeline interleaved.
    pub undo_stack: Vec<EditOp>,
    /// Redone-from states (redo pops from here). Cleared by any new edit.
    pub redo_stack: Vec<EditOp>,
    /// The covermark picker dropdown's entries, when open (paired with a `flyout` of kind
    /// `Covermark`).
    pub picker: Option<Picker>,
    /// The SHARED keyboard-nav state of whichever toolbar flyout is open (covermark picker
    /// or color palette). Drives the highlight + arrow/Enter/Esc dispatch for both.
    pub flyout: Option<FlyoutNav>,
    /// A bake (export re-encode) is in flight; share/delete inputs are held off.
    pub baking: bool,
    // DRAGON-467: `pending: Option<ShareIntent>` lived here, naming which action the
    // in-flight bake was for. Copy was the one action left then; the share sheet
    // (DRAGON-474) made it two again, and `bake_for_share` below is the difference that
    // remains — a full intent enum is still not warranted for one bit.
    /// The file the in-flight bake writes (the save destination for a Save; a throwaway temp
    /// for a Copy, so copying never persists edits to a file on disk).
    pub pending_output: Option<PathBuf>,
    /// The in-flight bake's artifact is for the SHARE SHEET, not the clipboard
    /// (DRAGON-474). Set by `run_share_sheet` when its bake starts, taken by `BakeDone` to
    /// pick the completion seam; meaningless while `baking` is false. Defaults to `false`
    /// so every historical bake path keeps meaning "copy".
    pub bake_for_share: bool,
    /// The LAST completed bake: where it sits in the history, and the file it wrote
    /// (DRAGON-467 review, major 4). `None` = nothing baked yet, or the artifact ended up on
    /// an abandoned redo branch.
    ///
    /// Read by [`bake_need`] so a share standing on exactly this state serves the artifact
    /// instead of rendering it again — which is what stops "Save, then Escape" from running
    /// a second full encode. Maintained beside `saved_depth` in [`Self::push_op`].
    pub baked: Option<(usize, PathBuf)>,
    /// Where the bytes currently ON THE CLIPBOARD sit in the history (DRAGON-467 review,
    /// major 4), or `None` when nothing of this document has been copied.
    ///
    /// Set by every successful copy, including the automatic one the editor performs as it
    /// opens (which lands at depth 0). Read by [`clipboard_is_current`], so an explicit Copy
    /// followed by Escape does not copy the same bytes twice.
    pub copied_depth: Option<usize>,
    /// The document was asked to CLOSE with unsaved edits (DRAGON-353): show the modal
    /// "you have unsaved changes" card instead of closing. Cleared by every button on it.
    /// Replaces the old `confirm_overwrite` flag — Save no longer overwrites ANY original
    /// (it writes the `-edited` sibling), so there is nothing left to confirm there.
    pub confirm_close: bool,
    /// Close this document as soon as the in-flight share action completes (DRAGON-353).
    /// Set by the unsaved-changes dialog's Save / Save As / Copy / Delete buttons, which
    /// ACT and THEN close — the share itself never closes anything. It doubles as the
    /// "this action came from the dialog" signal: only a dialog press ever sets it, so a
    /// failure can tell whether it owes the user a dialog (see [`Self::close_error`]).
    pub close_after_share: bool,
    /// Why the last dialog-initiated action FAILED (DRAGON-353 follow-up), when one did.
    ///
    /// `Some` re-raises the unsaved-changes dialog carrying the real reason — the
    /// `io::Error`, the encoder's complaint, the unlink that didn't — instead of letting a
    /// toast expire over a window that looks like nothing happened. From there the user has
    /// three honest ways forward, all already on the card: retry the action, **Exit anyway**
    /// (the same discard route as "Close without saving", now an informed choice), or
    /// **Continue editing**. Cleared by every button on the dialog and by the start of the
    /// next attempt, so a retry is never wedged by a stale notice.
    pub close_error: Option<String>,
    /// Index into [`super::PREVIEW_PROCESSING_MESSAGES`] for the in-editor processing
    /// overlay, picked when a bake/export starts (DRAGON-353 replaced the desktop
    /// "Processing capture" notification with the editor's own spinner).
    pub processing_msg: usize,
    /// The surface was destroyed (a `WindowClosed` event) while the bake was in flight
    /// (DRAGON-352): closing the document right then would `finish_session` and exit
    /// with the bake thread mid-write, so the close is DEFERRED — `BakeDone` reads this
    /// and completes it (forcing the close even in keep-open mode, since the surface is
    /// already gone and the document would otherwise linger surfaceless).
    pub close_after_bake: bool,
    /// Cached covermark-overlay raster (raw RGBA), stacked over the base image/video via a
    /// persistent-texture shader so re-rasters don't churn iced's atlas (no blink). Built
    /// off-thread, coalesced/staleness-tracked by the slot itself. Shared by image + video
    /// previews.
    pub cm_raster: RasterSlot,
    /// The pixel dimensions the covermark raster was last produced at (DRAGON-324). The
    /// covermark raster resolution scales with the view zoom so a magnified mark stays crisp;
    /// this lets a zoom step SKIP a re-raster when the wanted resolution is unchanged.
    pub cm_raster_px: (u32, u32),
    /// The capture's pixel dimensions (set once probed/decoded) so preview rasters
    /// match the bake's aspect.
    pub frame: (u32, u32),

    // ── Crop tool (DRAGON-382; IMAGES only) ──────────────────────────────────────────
    /// The committed crop rectangle over the SOURCE image (`None` = un-cropped / the whole
    /// frame). Non-destructive: applied only at bake ([`bake_image`]). A full-frame crop is
    /// stored as `None`, so it is never dirty. See [`super::crop`].
    pub crop: Option<CropRect>,
    /// The transient crop-tool session, `Some` while the tool is active (its overlay draws and
    /// the tool hotkeys are suspended). See [`CropSession`].
    pub crop_session: Option<CropSession>,

    // ── Annotation editor (DRAGON-321; IMAGES only) ──────────────────────────────────
    /// The annotation scene, in SOURCE-pixel coords. Z-order IS the vector order (later
    /// = on top).
    pub annotations: Vec<AnnotationItem>,
    /// The active annotation DRAW tool: `Some(Arrow | Box)` draws on an empty-canvas drag;
    /// `None` (the default) is NEUTRAL — existing items are still fully selectable /
    /// movable / resizable and an empty click deselects, but an empty drag draws nothing.
    pub tool: Option<Tool>,
    /// The action tray's double-click detector (DRAGON-339): two presses of the SAME tool button
    /// in quick succession spawn a ready-made item in the middle of the picture.
    pub tool_clicks: ToolClicks,
    /// The per-slot tool-cycle CURSOR (DRAGON-369): for each cycling tray slot (keyed by its
    /// cycle [`crate::shortcuts::Action`]) the member that was last armed — by its cycle key,
    /// by a direct tray click, or by a per-tool hotkey alike, since every route funnels through
    /// `App::select_annot_tool`. That is what makes the keyboard and the mouse agree: clicking
    /// Border in the tray makes the next `U` advance to Border Highlight, exactly like picking
    /// from a Photoshop flyout.
    ///
    /// RUNTIME state, reset per document on purpose — the one-shot process model makes
    /// cross-session memory near-meaningless, and it would make a fresh capture behave unlike
    /// the last one. (The ARMED TOOL itself is persisted, `App::annot_tool`; that is a separate,
    /// pre-existing preference.)
    pub slot_cursor: std::collections::HashMap<crate::shortcuts::Action, Tool>,
    /// The current annotation color (`None` = the accent default, resolved when a shape
    /// is created so the off-thread raster never reads the theme).
    pub annot_color: Option<AnnotColor>,
    /// The remembered "swap back" color for the companion-swap hotkey (`X`; DRAGON-386): the
    /// color the active annotation color was last swapped away FROM, so a second `X` returns to
    /// it EXACTLY ([`super::annotate::companion_color`] is involutive only up to u8 rounding).
    /// `None` outside a swap pair; any NON-swap color pick (the flyout or the custom wheel)
    /// clears it, so a stale partner never hijacks the next swap. See
    /// [`super::annotate::companion_swap`].
    pub color_swap_back: Option<AnnotColor>,
    /// The SHARED stroke width in logical POINTS (DRAGON-383) seeded onto every new box AND
    /// arrow — the single source of truth a future width control drives. Kept in POINTS so it
    /// matches the preset ladder + the chrome flyout; scaled to SOURCE px at the seed site.
    /// `0.0` means [`super::annotate::DEFAULT_ANNOT_STROKE`].
    pub annot_stroke_w: f32,
    /// The SHARED ABSOLUTE corner radius in logical POINTS (DRAGON-383) both the box (corner
    /// radius) and arrow (round caps when > 0) read, scaled to SOURCE px at each render/bake
    /// site. `0.0` means [`super::annotate::DEFAULT_ANNOT_CURVE_RADIUS`] (there is no way to set
    /// a deliberate sharp `0.0` yet, so the fallback is safe).
    pub annot_curve_radius: f32,
    /// The side in logical POINTS (DRAGON-383) the NEXT sequence badge spawns at (click-placed OR
    /// double-click pre-placed): whatever the last badge in THIS editor was placed or resized to,
    /// brought back to points from its settled source-px side. `0.0`
    /// means [`super::annotate::DEFAULT_BADGE_SIZE`] — read it through [`Self::badge_size`].
    ///
    /// This is the per-DOCUMENT WORKING copy of a PERSISTED preference: it is seeded at
    /// document open from `App::annot_badge_size` (`preview::open::new_edit_state`), and every
    /// placement/resize writes back through `App::remember_badge_size`, so the size survives
    /// new editors, new capture processes and app restarts. With two documents open in the
    /// multi-doc host the two working copies may briefly disagree — deliberately: last write
    /// wins on disk, and the next document opened picks that up. No cross-document sync.
    pub annot_badge_size: f32,
    /// The selected annotation(s) — an ordered SET since DRAGON-341 (primary last). Drives the
    /// chrome + Delete/reorder/Esc handling. Read the primary through [`Self::selected`].
    pub sel: Selection,
    /// The in-flight pointer gesture (draw / move / resize), if any.
    pub gesture: Option<AnnotGesture>,
    /// The pre-gesture scene snapshot, pushed as ONE undo entry on gesture-commit.
    pub annot_snapshot: Option<Vec<AnnotationItem>>,
    /// The pen groups the IN-FLIGHT eraser sweep has marked (DRAGON-338). They draw at
    /// [`super::annotate::ERASE_PREVIEW_ALPHA`] — the preview of what releasing deletes — and
    /// the model itself is untouched until [`super::App::annot_gesture_end`] commits the sweep
    /// as ONE undo entry. Always empty outside an eraser gesture.
    pub erase_marks: Vec<AnnotId>,
    /// The IN-FLIGHT freehand stroke's RAW pointer trail (DRAGON-342), thinned only by
    /// [`super::annotate::PEN_MIN_STEP`]. The MODEL always holds the beautified (smoothed,
    /// pressure-profiled) curve — that is what the canvas draws, what the bake bakes and what
    /// every hit test reads — so the un-smoothed samples the fit is derived from live here and
    /// nowhere else, for the length of the gesture. Also the sole source of the SPEED proxy:
    /// the gaps between these samples are what the pseudo-pressure reads as "how hard was the
    /// hand pressing". Always empty outside a pen draw.
    pub pen_raw: Vec<super::annotate::AnnotPoint>,
    /// The custom-color WHEEL picker's live model. `Some` = the picker is open — it owns the
    /// interactive hue/saturation-value spectrum + hex/rgb input (libcosmic's own
    /// cross-platform color picker). `None` (the `Default`) = closed.
    pub annot_picker: Option<cosmic::widget::ColorPickerModel>,
    /// The annotation right-click context menu anchor (widget-local point), when open.
    pub annot_menu: Option<(f32, f32)>,
    /// Monotonic id source for new annotations.
    pub next_annot_id: u64,
    /// The base picture pixels as a GPU-uploadable frame (DRAGON-330), cached once on decode so
    /// the real-time effects shader ([`crate::widgets::annotation_fx`]) can seed its ping-pong
    /// from the retained original without re-copying every view build. `None` when the decode
    /// fell back to a path handle (no retained pixels) — effects then appear only on export.
    pub fx_base: Option<Arc<super::layers::PixelFrame>>,

    // ── Text annotations (DRAGON-354; IMAGES only) ───────────────────────────────────────
    /// The size in logical POINTS (DRAGON-383) the NEXT text box is created at — the size
    /// dropdown drives it. Kept in POINTS so the dropdown highlight + chip match the presets;
    /// scaled to SOURCE px when it seeds a box. `0.0` means
    /// [`super::text_annot::DEFAULT_TEXT_SIZE`]; read through [`Self::text_size`].
    pub annot_text_size: f32,
    /// The family the NEXT text box uses — the font toggle drives it. Defaults to the
    /// handwritten Excalifont.
    pub annot_text_font: super::text_annot::TextFont,
    /// The IN-FLIGHT text-editing session (blinking caret + live buffer), if any. `Some` gates
    /// keyboard routing (printable keys type into the box; shortcuts are suspended) and the
    /// caret blink subscription. See [`TextEdit`].
    pub text_edit: Option<TextEdit>,
    /// The live TEXT raster layers — ONE PER text annotation (DRAGON-354; split per item by
    /// DRAGON-373), in SCENE order, each a persistent-texture layer keyed by its annotation
    /// ([`super::layers::LayerKey::text`]) so per-keystroke re-renders never churn iced's atlas.
    /// Rendered synchronously by [`super::App::refresh_text_display`]; empty when there is no
    /// text (a blank box has no layer either — there is nothing to draw).
    ///
    /// Per ITEM rather than one composite because the live view has to interleave text with the
    /// vector kinds in true z-order, and one raster can only sit at one depth — see
    /// [`super::layers::LayerSlot::text`].
    pub text_layers: Vec<TextItemLayer>,

    // ── Global dim / spotlight (DRAGON-329; IMAGES only) ─────────────────────────────────
    /// The global dim amount (0..1): `0` = no dim (byte-identical to no dim), higher = darker.
    /// Punched out to full brightness inside the knockout rects (spotlight / box / highlight /
    /// box-highlight). ALWAYS starts at 0 (never persisted across previews). Renders via the GPU
    /// dim pass on display and [`super::annotate::apply_dim`] on bake.
    pub dim: f32,
    /// The dim value at the START of the active slider drag, `Some` while dragging — so a whole
    /// drag coalesces into ONE undo entry (pushed on release; the mirror of `annot_snapshot`).
    pub dim_drag_start: Option<f32>,

    /// The undo DEPTH (`undo_stack.len()`) at which this document was last SAVED — the
    /// history position the file on disk corresponds to. `None` = never saved, or the
    /// save point was stranded on an abandoned redo branch (see [`Self::push_op`]).
    ///
    /// # Why a depth and not a bool (DRAGON-353 follow-up)
    ///
    /// A save no longer clears the history — the editor is non-destructive, so the scene
    /// and its whole undo stack SURVIVE a save and Ctrl+Z still walks back through
    /// everything. That makes a bare `saved: bool` a liar the moment the user undoes past
    /// the save point: the file on disk stops matching the scene, but the flag still says
    /// clean and the document would close silently on work that is once again unsaved.
    ///
    /// The depth is the honest form. `undo_stack.len()` IS the position along a linear
    /// history (undo moves it down, redo moves it back up), so "clean" is exactly "we are
    /// standing where we saved" — see [`unsaved_at`].
    pub saved_depth: Option<usize>,
}

impl EditState {
    /// Whether a plain left-drag PANS the picture — i.e. the HAND tool is armed (DRAGON-392).
    /// THE seam that replaced the old `Viewport::pan_mode` flag: the canvas and the `ZoomPan`
    /// still take one bool, but it is now DERIVED from the armed tool, so the toolbar, the `H`
    /// key and the pointer behaviour can never disagree.
    pub fn pan_active(&self) -> bool {
        self.tool.is_some_and(Tool::is_hand)
    }

    /// Whether an edit needs a bake before sharing: a covermark, a non-empty annotation scene
    /// (any spotlight is an item, so this counts it), OR a non-zero global dim (DRAGON-329) —
    /// any would be silently dropped otherwise.
    ///
    /// This is the BAKE gate, not the unsaved-work gate — it answers "does an export have to
    /// re-encode?", which stays true after a save (the scene still has content). "Are there
    /// changes the file on disk doesn't have?" is [`unsaved_at`] / `PreviewState::unsaved`.
    pub fn dirty(&self) -> bool {
        self.covermark.is_some()
            || !self.annotations.is_empty()
            || self.dim > 0.0
            || self.crop.is_some()
    }

    /// Set (or clear) the committed crop, pushing the prior crop onto the shared undo stack and
    /// clearing redo (DRAGON-382). The crop is applied only at bake, so no raster refresh is owed.
    pub fn set_crop(&mut self, crop: Option<CropRect>) {
        self.push_op(EditOp::Crop(self.crop));
        self.crop = crop;
    }

    /// Arm a dialog-initiated action: dismiss the card, clear any stale failure notice, and
    /// remember that this action owes the user a close. The dialog's four buttons all go
    /// through here (via `App::share_then_close`) before dispatching the plain TOOLBAR
    /// message, which is what keeps the two entry points on one implementation.
    pub fn begin_close_action(&mut self) {
        self.confirm_close = false;
        self.close_error = None;
        self.close_after_share = true;
    }

    /// A dialog-initiated action FAILED: disarm the close, re-raise the dialog and give it
    /// `reason`. Returns whether it actually was dialog-initiated — a toolbar action's
    /// failure raises nothing (its toast already said so and the editor simply stays up).
    ///
    /// Nothing about the document's WORK is touched: the scene, the history and the save
    /// point are exactly as they were, so retrying, exiting anyway and continuing to edit
    /// are all live from here. Disarming the close is what stops a failure from attaching
    /// itself to an unrelated later completion.
    pub fn note_action_failure(&mut self, reason: impl Into<String>) -> bool {
        if !std::mem::take(&mut self.close_after_share) {
            return false;
        }
        self.close_after_bake = false;
        self.confirm_close = true;
        self.close_error = Some(reason.into());
        true
    }

    /// The dialog's "Continue editing" / "Exit anyway" / "Close without saving" all land
    /// here first: the card and its failure notice go, whatever happens next.
    pub fn dismiss_close_dialog(&mut self) {
        self.confirm_close = false;
        self.close_error = None;
    }

    /// Record where the file on disk now sits in the history — called after every save that
    /// actually WROTE something (a dirty Save, a Save As export). From here the document is
    /// clean until the user moves off this position.
    pub fn mark_saved(&mut self) {
        self.saved_depth = Some(self.undo_stack.len());
    }

    /// Push one op onto the shared history: clear redo (a new edit abandons the redone
    /// branch) and, if the SAVE POINT lived on that abandoned branch, forget it.
    ///
    /// The second half is what stops the depth from lying. Save at depth 3, undo to 2, then
    /// draw something new: the stack is back at depth 3, but it is a DIFFERENT depth 3 —
    /// the state the file holds is no longer reachable by any amount of redo. Dropping the
    /// marker makes the document permanently unsaved-relative-to-disk until it is saved
    /// again, which is the truth.
    fn push_op(&mut self, op: EditOp) {
        if self.saved_depth.is_some_and(|d| d > self.undo_stack.len()) {
            self.saved_depth = None;
        }
        // The same treatment for the two DRAGON-467 markers, and for the same reason: a
        // baked artifact or a clipboard write that lived on the abandoned branch describes a
        // state no amount of redo can reach again, so reusing either would serve the wrong
        // pixels. The equality checks in `bake_need` / `clipboard_is_current` handle every
        // other move on their own, because any push changes the depth.
        if self.baked.as_ref().is_some_and(|(d, _)| *d > self.undo_stack.len()) {
            self.baked = None;
        }
        if self.copied_depth.is_some_and(|d| d > self.undo_stack.len()) {
            self.copied_depth = None;
        }
        self.undo_stack.push(op);
        self.redo_stack.clear();
    }

    /// Record the artifact a bake just wrote, at the history position it was rendered from.
    /// Called from the two bake completions (`BakeDone`, `SaveAsBaked`).
    pub fn mark_baked(&mut self, path: &std::path::Path) {
        self.baked = Some((self.undo_stack.len(), path.to_path_buf()));
    }

    /// Record that the CURRENT state reached the clipboard. Called from the one place a copy
    /// outcome is known to be good, so the open-time auto-copy and the explicit Copy both
    /// count.
    pub fn mark_copied(&mut self) {
        self.copied_depth = Some(self.undo_stack.len());
    }

    /// Forget the scene entirely, because the FILE now holds it (DRAGON-467 review, blocker
    /// 1 — the video commit-and-reset). Everything the bake burned in goes: the covermark,
    /// the annotations, the dim, the crop, and the whole undo/redo history, since none of it
    /// can be undone out of a file that has already been rewritten.
    ///
    /// The document is clean afterwards by construction: an empty history at depth 0 with an
    /// empty scene. The caller re-probes the committed file (the video timeline lives on
    /// `VideoPreview`, not here) and tells the user what happened.
    pub fn reset_after_commit(&mut self) {
        self.covermark = None;
        self.annotations.clear();
        self.dim = 0.0;
        self.crop = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.saved_depth = Some(0);
        self.baked = None;
        self.copied_depth = None;
        self.sel.clear();
    }

    /// The PRIMARY selected annotation (DRAGON-341) — what the single-item operations (resize,
    /// duplicate, reorder, kind conversion) act on, and the only one wearing resize handles.
    pub fn selected(&self) -> Option<AnnotId> {
        self.sel.primary()
    }

    /// Drop every freehand PEN group from the selection (DRAGON-341) — what leaving POINTER mode
    /// does. Pen selection exists only under the pointer, so the SET is pruned rather than the
    /// chrome merely hidden: a hidden-but-selected stroke would still be swept up by a group
    /// move or a Delete, which is exactly the ghost the visuals promised was gone. Non-pen
    /// members keep their order, so the primary falls back to the last surviving shape.
    pub fn drop_pen_selection(&mut self) {
        let items = &self.annotations;
        self.sel.retain(|id| {
            !items.iter().any(|it| it.id == id && it.kind.is_pen())
        });
    }

    /// The kind of toolbar flyout currently open (covermark picker / color palette), if any.
    pub fn flyout_kind(&self) -> Option<FlyoutKind> {
        self.flyout.map(|f| f.kind)
    }

    /// The open flyout's highlighted entry index, if any.
    pub fn flyout_selected(&self) -> Option<usize> {
        self.flyout.and_then(|f| f.selected)
    }

    /// Open a flyout of `kind`, highlighting `selected`, over `len` entries.
    pub fn open_flyout(&mut self, kind: FlyoutKind, selected: Option<usize>, len: usize) {
        self.flyout = Some(FlyoutNav { kind, selected, len });
    }

    /// Close any open flyout (covermark picker or color palette) + drop covermark entries.
    pub fn close_flyout(&mut self) {
        self.flyout = None;
        self.picker = None;
    }

    /// The SHARED stroke width for new annotations in logical POINTS (DRAGON-383), falling back
    /// to the default. The caller scales it to this document's SOURCE px
    /// ([`super::annotate::points_to_source_px`]) at the moment it seeds a shape; the chrome
    /// flyout + the cycle compare it against the (point) preset ladder directly.
    pub fn stroke(&self) -> f32 {
        if self.annot_stroke_w > 0.0 {
            self.annot_stroke_w
        } else {
            super::annotate::DEFAULT_ANNOT_STROKE
        }
    }

    /// The side (logical POINTS, DRAGON-383) a newly placed sequence badge takes — the last one
    /// placed or resized in this editor, falling back to [`super::annotate::DEFAULT_BADGE_SIZE`].
    /// The caller scales it to SOURCE px then clamps it into the picture (see
    /// [`super::annotate::badge_placement_rect`]).
    pub fn badge_size(&self) -> f32 {
        if self.annot_badge_size > 0.0 {
            self.annot_badge_size
        } else {
            super::annotate::DEFAULT_BADGE_SIZE
        }
    }

    /// The size (logical POINTS, DRAGON-383) a newly created text box takes — whatever the size
    /// dropdown last selected, falling back to [`super::text_annot::DEFAULT_TEXT_SIZE`]. Scaled to
    /// SOURCE px by the caller as it seeds the box.
    pub fn text_size(&self) -> f32 {
        if self.annot_text_size > 0.0 {
            self.annot_text_size
        } else {
            super::text_annot::DEFAULT_TEXT_SIZE
        }
    }

    /// The SHARED absolute corner radius in logical POINTS (DRAGON-383), falling back to the
    /// default. Each render/bake site scales it to SOURCE px
    /// ([`super::annotate::points_to_source_px`]) to match the source-px shape geometry.
    pub fn curve_radius(&self) -> f32 {
        if self.annot_curve_radius > 0.0 {
            self.annot_curve_radius
        } else {
            super::annotate::DEFAULT_ANNOT_CURVE_RADIUS
        }
    }

    /// Mint the next annotation id.
    pub fn next_annot_id(&mut self) -> AnnotId {
        self.next_annot_id += 1;
        AnnotId(self.next_annot_id)
    }

    /// Record an annotation mutation in the shared history: push the PRE-EDIT scene and
    /// clear redo, mirroring [`Self::push_timeline`].
    pub fn push_annotations(&mut self, prev: Vec<AnnotationItem>) {
        self.push_op(EditOp::Annotations(prev));
    }

    /// Record a global-dim change (DRAGON-329) in the shared history: push the PRE-DRAG value
    /// and clear redo, mirroring [`Self::push_annotations`]. `prev` is the dim BEFORE the drag.
    pub fn push_dim(&mut self, prev: f32) {
        self.push_op(EditOp::Dim(prev));
    }

    /// Holistic spotlight/dim rule (DRAGON-329): a spotlight knocks a hole in the dim, so with no
    /// dim it would be INVISIBLE. Whenever ≥1 spotlight EXISTS on the canvas and the frame isn't
    /// dimmed yet, seed a dim (30% transparent) as its own undo entry — so a spotlight always
    /// reads however it came to exist (drawn, converted from another box, or duplicated). No-op
    /// once any dim is present (so the user can still slide it wherever they want afterward).
    pub fn ensure_dim_for_spotlights(&mut self) {
        const SPOTLIGHT_SEED_DIM: f32 = 0.7; // 30% transparent on the reversed slider
        if self.dim > 0.0 {
            return;
        }
        let has_spotlight = self
            .annotations
            .iter()
            .any(|it| matches!(it.kind, super::annotate::AnnotKind::Spotlight { .. }));
        if !has_spotlight {
            return;
        }
        let prev = self.dim;
        self.push_dim(prev);
        self.dim = SPOTLIGHT_SEED_DIM;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Set (or clear) the active covermark, pushing the prior state onto the undo
    /// stack and clearing redo. The display recomposite is the caller's job (async).
    pub fn set_covermark(&mut self, cm: Option<Covermark>) {
        self.push_op(EditOp::Covermark(self.covermark.clone()));
        self.covermark = cm;
        self.cm_raster.invalidate();
    }

    /// Record a timeline mutation (cut / segment delete) in the shared history:
    /// push the PRE-EDIT spans and clear redo, mirroring `set_covermark`. Called
    /// after the mutation succeeded (refused cuts/deletes never enter history).
    pub fn push_timeline(&mut self, prev: Vec<Span>) {
        self.push_op(EditOp::Timeline(prev));
    }

    /// Live-adjust the active covermark's zoom (no undo entry — it's a continuous
    /// control). No-op when no covermark is set.
    pub fn set_zoom(&mut self, zoom: f32) {
        if let Some(cm) = &mut self.covermark {
            cm.zoom = zoom.max(0.0);
            self.cm_raster.invalidate();
        }
    }

    /// The active covermark's zoom, or 0 when none.
    pub fn zoom(&self) -> f32 {
        self.covermark.as_ref().map(|c| c.zoom).unwrap_or(0.0)
    }

    /// Undo the most recent edit — covermark, timeline, or annotation, whichever was
    /// made last. `timeline` is the video preview's timeline when there is one (an image
    /// preview passes `None`; it never accumulates timeline ops). Returns which artifact
    /// changed so the caller refreshes the owed raster (`None` = the stack was empty). Only a
    /// covermark change owes a raster refresh now — annotations redraw as vectors for free.
    pub fn undo(&mut self, timeline: Option<&mut Timeline>) -> Option<EditKind> {
        match self.undo_stack.pop() {
            Some(EditOp::Covermark(prev)) => {
                self.redo_stack.push(EditOp::Covermark(self.covermark.clone()));
                self.covermark = prev;
                self.cm_raster.invalidate();
                Some(EditKind::Covermark)
            }
            Some(EditOp::Timeline(prev)) => {
                if let Some(tl) = timeline {
                    self.redo_stack.push(EditOp::Timeline(tl.spans.clone()));
                    tl.restore(prev);
                }
                Some(EditKind::Timeline)
            }
            Some(EditOp::Annotations(prev)) => {
                self.redo_stack.push(EditOp::Annotations(self.annotations.clone()));
                self.annotations = prev;
                self.sel.clear();
                Some(EditKind::Annotations)
            }
            Some(EditOp::Dim(prev)) => {
                self.redo_stack.push(EditOp::Dim(self.dim));
                self.dim = prev;
                Some(EditKind::Dim)
            }
            Some(EditOp::Crop(prev)) => {
                self.redo_stack.push(EditOp::Crop(self.crop));
                self.crop = prev;
                Some(EditKind::Crop)
            }
            None => None,
        }
    }

    /// Redo the most recently undone edit (any kind). Returns which artifact changed, as
    /// [`Self::undo`].
    pub fn redo(&mut self, timeline: Option<&mut Timeline>) -> Option<EditKind> {
        match self.redo_stack.pop() {
            Some(EditOp::Covermark(next)) => {
                self.undo_stack.push(EditOp::Covermark(self.covermark.clone()));
                self.covermark = next;
                self.cm_raster.invalidate();
                Some(EditKind::Covermark)
            }
            Some(EditOp::Timeline(next)) => {
                if let Some(tl) = timeline {
                    self.undo_stack.push(EditOp::Timeline(tl.spans.clone()));
                    tl.restore(next);
                }
                Some(EditKind::Timeline)
            }
            Some(EditOp::Annotations(next)) => {
                self.undo_stack.push(EditOp::Annotations(self.annotations.clone()));
                self.annotations = next;
                self.sel.clear();
                Some(EditKind::Annotations)
            }
            Some(EditOp::Dim(next)) => {
                self.undo_stack.push(EditOp::Dim(self.dim));
                self.dim = next;
                Some(EditKind::Dim)
            }
            Some(EditOp::Crop(next)) => {
                self.undo_stack.push(EditOp::Crop(self.crop));
                self.crop = next;
                Some(EditKind::Crop)
            }
            None => None,
        }
    }

    /// The crop APPLIED to the display right now, or `None` — the crop when one is set AND no crop
    /// session is live (a session reveals the whole image so the rect stays repositionable).
    ///
    /// THE rule, in one place: [`super::PreviewState::view_crop`] delegates here, so the model side
    /// (the covermark canvas below) and the view side can never disagree about what is framed.
    pub fn view_crop(&self) -> Option<CropRect> {
        self.crop.filter(|_| self.crop_session.is_none())
    }

    /// Whether the covermark LAYER is drawn right now (DRAGON-402): everywhere except inside a
    /// live crop session.
    ///
    /// The covermark is a treatment of the FINAL FRAMING — DRAGON-391 made its canvas the crop
    /// rect, and the mark is re-rastered for the new canvas the moment a crop is accepted. So
    /// whatever a session could show was never predictive: it is a mark for the OLD framing,
    /// drawn over a view that is deliberately showing something else (the whole image, reframed
    /// and zoomed out for the crop workspace), and guaranteed to be replaced on accept. Hiding it
    /// is more honest than rendering it — and it is what the owner asked for, the mark having
    /// visibly mis-rendered in that state (see the commit for the mechanism).
    ///
    /// Strictly a DISPLAY rule: [`Self::covermark`] is never touched, so a session — cancelled or
    /// accepted — leaves the document exactly as it found it, and the bake (which a session can
    /// never reach) is unaffected.
    pub fn covermark_visible(&self) -> bool {
        self.crop_session.is_none()
    }

    /// The global dim actually RENDERED right now (DRAGON-410): the user's [`Self::dim`]
    /// everywhere except inside a live crop session, which forces it CLEAR (0 = maximum
    /// brightness, fully transparent).
    ///
    /// WHY: you cannot judge a crop through a dimmed image. The session's whole job is showing
    /// the picture being framed, so it shows it undimmed — the same call as hiding the covermark
    /// (see [`Self::covermark_visible`]), and it retires DRAGON-392's carve-out that kept the dim
    /// SLIDER live mid-crop on the reasoning that dim is a viewing aid (it is; the aid just points
    /// the wrong way here, so the session takes the decision instead of offering it).
    ///
    /// **An OVERRIDE, not an edit — and that is the whole design.** The model's `dim` is never
    /// written, so there is nothing to save and nothing to restore: no history entry can be
    /// pushed, [`Self::dirty()`] cannot move, and the bake (`bake_image`, which reads `dim`
    /// directly and which a session can never reach anyway) is untouched. Both exits — accept and
    /// cancel alike — restore the user's dim EXACTLY, by the arithmetic of the session ending, not
    /// by a saved copy that some third exit path could forget to put back. Contrast
    /// [`CropSession::saved_view`] / [`CropSession::saved_tool`], which have to be stashed because
    /// the session genuinely mutates them.
    pub fn view_dim(&self) -> f32 {
        if self.crop_session.is_some() { 0.0 } else { self.dim }
    }

    /// The covermark raster to DRAW, if any — THE one read every mount goes through, so
    /// [`Self::covermark_visible`] cannot be honoured at one mount and forgotten at another.
    ///
    /// Deliberately NOT what decides the layer KEY the view registers: the raster survives a
    /// session untouched in its slot, so keeping the key listed keeps the GPU texture alive and
    /// makes the restore on exit a redraw rather than a re-upload (over-approximating the key set
    /// is explicitly safe — see `layers.rs`; under-approximating is what frees a live texture).
    pub fn covermark_layer(&self) -> Option<&Arc<super::layers::PixelFrame>> {
        self.cm_raster.frame().filter(|_| self.covermark_visible())
    }

    /// The covermark raster's CANVAS in whole SOURCE px — THE IMAGE the mark covers, defaulted for
    /// a not-yet-loaded (0-sized) capture.
    ///
    /// DRAGON-391: once a crop is accepted **the image IS the crop rectangle** (DRAGON-385's
    /// display frame), so that is what the covermark spans — including the black extension an
    /// over-crop adds, and nothing beyond it. NOT the source frame (which left the extension bare),
    /// and NOT the source ∪ crop union (which would pattern past the image the user cropped to).
    /// Un-cropped — and during a crop SESSION, which reveals the whole image — this is the source
    /// frame, exactly what it always was; videos never crop, so they are untouched.
    fn raster_frame(&self) -> (u32, u32) {
        if let Some(c) = self.view_crop() {
            return c.pixel_size();
        }
        match self.frame {
            (0, _) | (_, 0) => (1280u32, 800u32),
            f => f,
        }
    }

    /// The covermark display raster resolution at the current `view_zoom` — the layer's own
    /// ON-SCREEN device-pixel footprint (DRAGON-362), see [`layer_raster_dims`]. The covermark
    /// layer spans the whole image, so its raster is [`Self::raster_frame`] — the crop rect once a
    /// crop is applied (DRAGON-391) — at that scale. `visual_scale` is device px per SOURCE px and
    /// is measured against the same display frame, so the product is the canvas's true on-screen
    /// footprint either way.
    pub fn covermark_raster_size(&self, view_zoom: f32, visual_scale: f32) -> (u32, u32) {
        layer_raster_dims(self.raster_frame(), layer_raster_scale(view_zoom, visual_scale))
    }
}

/// ONE text annotation's live raster layer (DRAGON-373): the pixels, where they go, and the
/// signature of what they hold.
#[derive(Clone)]
pub struct TextItemLayer {
    /// The annotation drawn here — its identity for the texture slot AND its place in the
    /// scene's z-order (the layers list mirrors the item order).
    pub id: super::annotate::AnnotId,
    /// The rendered glyphs. A layer only exists when there is ink, so this is never a blank box.
    pub frame: Arc<super::layers::PixelFrame>,
    /// WHERE and at what resolution [`Self::frame`] was rendered.
    pub geom: TextLayerGeom,
    /// The RASTER-INPUT signature of the drawing in [`Self::frame`] (DRAGON-376): everything
    /// [`super::text_annot::render_into`] actually reads — the derived layout, the origin, the
    /// face, the size, the outline weight and the colour.
    ///
    /// It lets [`super::App::refresh_text_display`] answer "would re-rendering this box produce a
    /// different bitmap?" WITHOUT re-rendering it. Editor CHROME — the caret index, the selection
    /// anchor, the blink phase — lives on [`EditState::text_edit`] and is drawn as canvas
    /// vectors, so it reaches nothing here: a drag-select used to re-run the whole SVG-build →
    /// usvg-parse → resvg → demultiply pipeline once per pointer event to produce a byte-identical
    /// raster (~29 ms/event at 512 px type, against a 125 Hz pointer — the reported lock-up).
    ///
    /// It describes the RASTER, not the scene: the DRAGON-368 gesture proxy deliberately re-places
    /// an existing raster without re-rendering, and leaving this signature at what was actually
    /// drawn is what makes the commit re-render fire exactly once at the end of the gesture.
    pub(super) sig: super::annotate::TextRenderSig,
}

/// The live TEXT layer's geometry (DRAGON-362): the picture REGION it covers, the raster
/// scale it was rendered at, and the resulting pixel dimensions. The text layer is a
/// sub-rect of the canvas — see [`super::annotate::text_layer_region`] — so the view needs
/// its placement, and a zoom step needs its scale to decide whether a re-render would
/// actually change anything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextLayerGeom {
    /// The raster scale (raster px per source px) — see [`layer_raster_scale`].
    pub scale: f32,
    /// The picture region the raster covers, in SOURCE px.
    pub region: super::annotate::AnnotRect,
    /// The raster's pixel dimensions.
    pub px: (u32, u32),
}

impl TextLayerGeom {
    /// The layer's placement within the picture canvas, as the `[x, y, w, h]` fractions
    /// [`super::layers::Layer::dest`] wants. `frame` is the source frame; a degenerate frame
    /// degrades to the whole canvas rather than a divide by zero.
    #[cfg(test)]
    pub fn dest(&self, frame: (u32, u32)) -> super::layers::Dest {
        self.dest_in((0.0, 0.0), (frame.0 as f32, frame.1 as f32))
    }

    /// The layer's placement as fractions of the DISPLAY frame (DRAGON-385): the region shifted
    /// by the view-crop `origin` (SOURCE px) and divided by the display `dims` (SOURCE px). With
    /// `origin = (0, 0)` and `dims` the whole frame this is [`Self::dest`] — byte-identical for an
    /// un-cropped document. When a crop is applied the picture canvas the text rides on is the
    /// crop region, so the caption's fractions are measured against THAT (and a caption outside
    /// the crop lands outside `[0, 1]`, clipped away like every other out-of-crop mark).
    pub fn dest_in(&self, origin: (f32, f32), dims: (f32, f32)) -> super::layers::Dest {
        let (dw, dh) = dims;
        if !(dw > 0.0 && dh > 0.0) {
            return super::layers::DEST_FULL;
        }
        [
            (self.region.x - origin.0) / dw,
            (self.region.y - origin.1) / dh,
            self.region.w / dw,
            self.region.h / dh,
        ]
    }
}

/// The largest texture dimension a live layer raster may reach. `wgpu::Limits::default()`
/// (what iced/libcosmic request) puts `max_texture_dimension_2d` at 8192, so a raster past
/// this could not be uploaded at all — a capture wider than 8192 px (a multi-monitor "all
/// displays" grab is easily 10240) would otherwise blow up now that the raster tracks the
/// SOURCE frame rather than a fixed 1024 box. Layers beyond it degrade to a soft (resampled)
/// overlay instead of failing; the BAKE is unaffected — it composites on the CPU at full
/// source resolution regardless.
pub const MAX_LAYER_DIM: u32 = 8192;

/// The granularity the layer raster scale is snapped UP to (1/16 of the source resolution).
///
/// WHY quantize: the wanted scale now moves CONTINUOUSLY with the zoom slider, and a raster
/// whose dimensions changed on every zoom step would (a) re-render on every tick and (b) force
/// the layer's persistent GPU texture to be RE-CREATED each time (`LayerStackPipeline::upsert`
/// only updates in place while the dimensions hold), which is exactly the churn `layers.rs`
/// exists to avoid. Snapping UP — never down — means the raster is always at least the layer's
/// on-screen size, so quantization can never reintroduce softness; it only spends up to one
/// step's worth of extra pixels.
pub const RASTER_QUANTUM: f32 = 1.0 / 16.0;

/// The fraction of the SOURCE frame's pixels a full-frame live layer occupies ON SCREEN, which
/// is the resolution its raster must be rendered at to be pixel-crisp beside the base image
/// (DRAGON-362).
///
/// The base image is drawn from the untouched source pixels and DOWNSAMPLED into the viewport;
/// a layer rastered smaller than its on-screen footprint is UPSAMPLED into the same area, and
/// that mismatch is what read as "fuzzy text next to crisp pixels" on a large (5120×2880)
/// capture — the old contract pinned every layer to a 1024 box at fit zoom, so a 5K capture
/// shown ~2300 px wide sampled a 1024-wide raster.
///
/// `visual_scale` is [`super::App::preview_visual_scale`] = `fit_scale × source_scale`: the
/// fraction of the picture's NATURAL on-screen size shown at fit, times the source display's
/// backing scale. Their product is exactly `device pixels per source pixel` at fit — the
/// `source_scale` factor is what carries HiDPI, since the preview surface renders `source_scale`
/// device pixels per logical point. So `view_zoom × visual_scale` is the on-screen device-pixel
/// footprint as a fraction of the source frame.
///
/// Capped at `1.0`: there is no detail beyond the source pixels, and stopping there is what
/// keeps the live layer and the bake (which composites at exactly the source frame) agreeing.
/// Floored just above zero so a degenerate scale can never produce an empty raster. Pure.
pub fn layer_raster_scale(view_zoom: f32, visual_scale: f32) -> f32 {
    let want = view_zoom.max(0.0) * visual_scale.max(0.0);
    if !want.is_finite() || want <= 0.0 {
        return 1.0; // unknown geometry → full source resolution, never a soft guess
    }
    // Snap UP to the next quantum (see [`RASTER_QUANTUM`]) and cap at the source resolution.
    ((want / RASTER_QUANTUM).ceil() * RASTER_QUANTUM).min(1.0)
}

/// A layer raster's pixel dimensions: `frame × scale`, never zero, capped at the source frame
/// and at [`MAX_LAYER_DIM`]. The [`MAX_LAYER_DIM`] cap is applied to BOTH axes by the same
/// factor so the raster keeps the region's aspect (an anisotropic clamp would visibly stretch
/// the layer). Pure.
pub fn layer_raster_dims(frame: (u32, u32), scale: f32) -> (u32, u32) {
    let (fw, fh) = (frame.0.max(1), frame.1.max(1));
    let s = scale.clamp(0.0, 1.0);
    let mut w = ((fw as f32 * s).ceil() as u32).clamp(1, fw);
    let mut h = ((fh as f32 * s).ceil() as u32).clamp(1, fh);
    if w > MAX_LAYER_DIM || h > MAX_LAYER_DIM {
        let k = MAX_LAYER_DIM as f32 / w.max(h) as f32;
        w = ((w as f32 * k).floor() as u32).max(1);
        h = ((h as f32 * k).floor() as u32).max(1);
    }
    (w, h)
}

/// THE unsaved-work predicate (DRAGON-353 follow-up), as pure arithmetic over the history
/// position — so the rule can be tested without a document.
///
/// * `saved_depth` — [`EditState::saved_depth`]: where the file on disk sits in the
///   history, or `None` for "never saved / the save point was abandoned".
/// * `depth` — the CURRENT `undo_stack.len()`.
/// * `scene_dirty` — whether the scene has content at all (`PreviewState::dirty`, which
///   ORs in deleted timeline segments).
///
/// Saved ⇒ unsaved exactly when we have moved off the save point, in EITHER direction:
/// undoing past it leaves the file holding more than the scene, redoing past it leaves the
/// scene holding more than the file. Never saved ⇒ fall back to "is there anything to
/// lose", which is the pre-existing behaviour for a document that has never written a file.
pub fn unsaved_at(saved_depth: Option<usize>, depth: usize, scene_dirty: bool) -> bool {
    match saved_depth {
        Some(d) => depth != d,
        None => scene_dirty,
    }
}

/// Rasterize a covermark to a `w`×`h` straight-alpha RGBA (for the video overlay
/// preview). Public so the async recomposite in `preview::mod` can run it off-thread.
pub fn rasterize_preview(cm: &Covermark, w: u32, h: u32) -> Option<RgbaImage> {
    rasterize(cm, w, h)
}

/// The user covermark folder (`~/.config/cosmic-capture-kit/covermarks` on every OS
/// — see [`crate::util::app_config_dir`]), created on first use so it's discoverable.
pub fn covermark_dir() -> Option<PathBuf> {
    let dir = crate::util::app_config_dir()?.join("covermarks");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// The picker's choices: the built-in Confidential mark, the custom-text mark (its
/// text snapshotted from `custom_text`), then every `.svg` in the covermark folder.
pub fn covermark_entries(custom_text: &str) -> Vec<CovermarkKind> {
    let mut entries = vec![
        CovermarkKind::Confidential,
        CovermarkKind::Text(custom_text.to_string()),
    ];
    if let Some(dir) = covermark_dir()
        && let Ok(read) = std::fs::read_dir(dir)
    {
        let mut files: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
            })
            .collect();
        files.sort();
        entries.extend(files.into_iter().map(CovermarkKind::File));
    }
    entries
}

/// The default custom-covermark text before the user configures one in settings.
const DEFAULT_COVERMARK_TEXT: &str = "CONFIGURE TEXT IN SETTINGS";

/// The built-in Confidential SVG bytes (for the picker's preview thumbnail).
pub fn confidential_svg() -> &'static [u8] {
    CONFIDENTIAL_SVG
}

/// The generated text-covermark SVG bytes for `text` (for the picker's preview).
pub fn text_svg_bytes(text: &str) -> Vec<u8> {
    text_svg(text).into_bytes()
}

/// Build a tiled, −45°, gray, borderless text covermark SVG at FULL opacity — the
/// covermark opacity is applied later at composite time (a runtime slider), not baked
/// into the SVG.
fn text_svg(text: &str) -> String {
    // Escape XML-special chars so arbitrary user text can't break the document.
    let safe = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let safe = if safe.trim().is_empty() { DEFAULT_COVERMARK_TEXT.to_string() } else { safe };
    format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1600 1000" width="1600" height="1000">
  <defs>
    <pattern id="mark" width="620" height="200" patternUnits="userSpaceOnUse" patternTransform="rotate(-45)">
      <text x="0" y="70" font-family="sans-serif" font-weight="bold" font-size="52" letter-spacing="4" fill="#888888">{safe}</text>
      <text x="-310" y="170" font-family="sans-serif" font-weight="bold" font-size="52" letter-spacing="4" fill="#888888">{safe}</text>
      <text x="310" y="170" font-family="sans-serif" font-weight="bold" font-size="52" letter-spacing="4" fill="#888888">{safe}</text>
    </pattern>
  </defs>
  <rect width="1600" height="1000" fill="url(#mark)"/>
</svg>"##
    )
}

/// Rasterize a covermark to COVER a `w`×`h` frame (aspect-preserving fill, centered,
/// overflow cropped), returning straight-alpha RGBA the same size as the frame. The
/// covermark's `zoom` multiplies the fill scale (≥ cover, so it always fills). Text
/// elements need fonts: the system fontdb is loaded once and shared.
fn rasterize(cm: &Covermark, w: u32, h: u32) -> Option<RgbaImage> {
    static FONTS: std::sync::OnceLock<Arc<resvg::usvg::fontdb::Database>> =
        std::sync::OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        use resvg::usvg::fontdb;
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        // fontdb resolves generic families ("sans-serif") by its CONFIGURED
        // name — defaulting to the Windows names (Arial…) — not by fuzzy
        // matching; the host's fontconfig aliases normally correct that. On
        // systems without usable aliases (minimal distros, bare containers)
        // the name matches no face, usvg then drops the whole text run, and
        // <text> covermarks rasterize EMPTY. If the generic doesn't resolve,
        // repoint it (and serif, usvg's built-in last resort) at a face that
        // actually exists.
        let resolves = |db: &fontdb::Database, family: fontdb::Family| {
            db.query(&fontdb::Query { families: &[family], ..Default::default() }).is_some()
        };
        if !resolves(&db, fontdb::Family::SansSerif) {
            let pick = ["DejaVu Sans", "Liberation Sans", "Noto Sans", "Cantarell", "Ubuntu", "FreeSans"]
                .into_iter()
                .find(|n| resolves(&db, fontdb::Family::Name(n)))
                .map(str::to_string)
                .or_else(|| db.faces().next().map(|f| f.families[0].0.clone()));
            if let Some(name) = pick {
                db.set_sans_serif_family(name.clone());
                db.set_serif_family(name);
            }
        }
        Arc::new(db)
    });
    let bytes = cm.kind.svg()?;
    let opt = resvg::usvg::Options {
        fontdb: fonts.clone(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(&bytes, &opt).ok()?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 || w == 0 || h == 0 {
        return None;
    }
    // Cover fit, then zoom enlarges from there (never below cover → always fills).
    let cover = (w as f32 / size.width()).max(h as f32 / size.height());
    let scale = cover * (1.0 + cm.zoom.max(0.0));
    let tx = (w as f32 - size.width() * scale) / 2.0;
    let ty = (h as f32 - size.height() * scale) / 2.0;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty),
        &mut pixmap.as_mut(),
    );
    // tiny-skia pixels are premultiplied; unmultiply into straight alpha, then scale
    // alpha by the covermark's opacity (kept out of the SVG so a slider drives it).
    let opacity = cm.opacity.clamp(0.0, 1.0);
    let mut rgba = RgbaImage::new(w, h);
    for (dst, src) in rgba.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        let a = (c.alpha() as f32 * opacity).round() as u8;
        *dst = ::image::Rgba([c.red(), c.green(), c.blue(), a]);
    }
    Some(rgba)
}

/// Alpha-blend `overlay` onto `base` centered (straight alpha, src-over).
fn composite_centered(base: &mut RgbaImage, overlay: &RgbaImage) {
    let (bw, bh) = base.dimensions();
    let (ow, oh) = overlay.dimensions();
    let x0 = (bw.saturating_sub(ow)) / 2;
    let y0 = (bh.saturating_sub(oh)) / 2;
    for (ox, oy, &::image::Rgba([r, g, b, a])) in overlay.enumerate_pixels() {
        if a == 0 {
            continue;
        }
        let (bx, by) = (x0 + ox, y0 + oy);
        if bx >= bw || by >= bh {
            continue;
        }
        let dst = base.get_pixel_mut(bx, by);
        let af = a as u32;
        for (d, s) in dst.0.iter_mut().take(3).zip([r, g, b]) {
            *d = ((s as u32 * af + *d as u32 * (255 - af)) / 255) as u8;
        }
    }
}

/// Composite `cm` onto decoded pixels (shared by the image display recomposite and
/// the image bake). No-op when `cm` is `None`.
pub fn apply_covermark(base: &mut RgbaImage, cm: Option<&Covermark>) {
    if let Some(cm) = cm {
        let (w, h) = base.dimensions();
        if let Some(overlay) = rasterize(cm, w, h) {
            composite_centered(base, &overlay);
        }
    }
}

/// Bake the pending edits onto an image, reading `src` and writing the result to `dst`
/// (they may be the same path for an in-place Save, or differ so Copy can produce an
/// edited file WITHOUT touching the saved original). Returns `dst`'s size. At least one
/// of a covermark or a non-empty annotation scene must hold (the image edits).
///
/// # A still is written as PNG. The destination extension does not select a format.
///
/// That is the whole format rule (DRAGON-455), and it is stated here once because the two
/// halves of this code drifting apart IS the bug it fixes. This function used to branch on
/// `dst`'s extension: with a pixel edit it round-tripped through a temp PNG and re-encoded
/// to whatever the name said, while an UNEDITED save never reached here at all and plainly
/// copied PNG bytes under the same name. The identical user action therefore produced a
/// real JPEG or a mislabeled PNG depending on whether they had happened to draw on the
/// capture first. There is now one answer: PNG, always, through
/// [`crate::media::png::save_png`] — which is also what carries the `--inspect` `Comment`
/// chunk (DRAGON-445).
///
/// The naming side is [`super::naming::png_name`], which makes every still destination SAY
/// png before it gets here, so no caller can produce a name this write would contradict.
/// RECORDINGS are a separate world: [`bake_video`] does honour the destination container.
///
/// Compositing order (DRAGON-330 true-layer stack) — display and bake share the ONE core:
/// 0. the global DIM (DRAGON-329) darkens the base at the very bottom, punched out by the
///    knockout rects (spotlight / box / highlight / box-highlight) via
///    [`super::annotate::apply_dim`] — a no-op when `dim == 0`;
/// 1. the region EFFECTS (highlight / pixelate / blur) composite in true scene z-order via
///    [`super::annotate::apply_effects`], each reading the content accumulated below it;
/// 2. the covermark (privacy mark) as a source-over overlay, spanning THE IMAGE — the crop
///    rectangle once a crop is applied, so it covers an over-crop's black extension and nothing
///    past it (DRAGON-391);
/// 3. the box/arrow annotation scene ON TOP (the active markup, above the privacy mark) —
///    all at full source resolution, position-aware.
pub fn bake_image(
    src: &Path,
    dst: &Path,
    cm: Option<&Covermark>,
    annotations: &[AnnotationItem],
    curve: f32,
    dim: f32,
    crop: Option<CropRect>,
) -> std::io::Result<u64> {
    let err = |e: String| std::io::Error::other(e);
    if cm.is_some() || !annotations.is_empty() || dim > 0.0 || crop.is_some() {
        let mut rgba = ::image::open(src).map_err(|e| err(e.to_string()))?.into_rgba8();
        // The PRISTINE full-res source, used ONLY to size the content-aware pixelate cell — the
        // SAME analysis source the GPU display uses (its retained base pixels), so display + bake
        // pick identical blocks. Cloned only when a pixelate item is present (otherwise unused).
        let analysis = annotations
            .iter()
            .any(|it| matches!(it.kind, super::annotate::AnnotKind::Pixelate { .. }))
            .then(|| rgba.clone());
        // The global dim darkens the base FIRST (the hard floor), punched out inside the
        // knockout rects; a `dim == 0` bake is byte-identical (apply_dim returns early).
        let knockouts = super::annotate::knockout_rects(annotations);
        super::annotate::apply_dim(&mut rgba, dim, &knockouts, curve);
        // The region effects composite in true z-order — the SAME `apply_effects` core the
        // real-time GPU display shader mirrors (DRAGON-330), so what the user saw is what saves.
        // An effect-free scene is a no-op, keeping the covermark-only / annotation-only bakes
        // byte-identical. The pixelate cell size reads the pristine `analysis` (pre-dim) source;
        // with no pixelate item `analysis` is None and a 1×1 placeholder is passed but never read.
        let placeholder = ::image::RgbaImage::new(1, 1);
        let analysis_ref = analysis.as_ref().unwrap_or(&placeholder);
        super::annotate::apply_effects(&mut rgba, analysis_ref, annotations, curve);
        // DRAGON-382 + DRAGON-389 + DRAGON-391: dim / effects composite over the FULL source above
        // (they are content-anchored and cannot exist off-source). Everything BELOW this line works
        // on THE IMAGE — which, once a crop is applied, IS the crop rectangle: cut it FIRST
        // (black-filling any over-crop extension), then lay the covermark and the vector overlay
        // onto that canvas. So both cover the whole framed image, extension included, and neither
        // reaches past it — what the editor shows over the black extension (DRAGON-385's display
        // frame) survives to the saved file instead of being clipped at the old source edge. The
        // annotations draw offset by the crop origin, rounded exactly as `crop_image` cuts, so they
        // land pixel-aligned with it; the covermark simply spans the cut canvas. Z-order is
        // unchanged (covermark under the annotation scene), and with no crop this is the historical
        // `apply_covermark` + `apply_annotations` on the full source — byte-identical.
        if let Some(rect) = crop {
            rgba = super::crop::crop_image(&rgba, rect);
        }
        apply_covermark(&mut rgba, cm);
        match crop {
            Some(rect) => {
                let offset = (rect.x.round(), rect.y.round());
                super::annotate::apply_annotations_at(&mut rgba, annotations, curve, offset);
            }
            None => super::annotate::apply_annotations(&mut rgba, annotations, curve),
        }
        // DRAGON-445: write through `media::png::save_png`, NOT `save_with_format`, so the
        // baked file keeps the `Comment` text chunk `--inspect` reads
        // (`type/source/mode/cursor`). `save_with_format` writes no ancillary chunks, so
        // every edited save silently lost the provenance a plain capture keeps — and it is
        // exactly the edited files someone is most likely to be asked about later.
        //
        // The metadata carried forward is the SOURCE's own comment, not a freshly built
        // one: this file IS that capture, edited. Re-deriving it here would describe the
        // editor session rather than the capture, and the fields (source compositor,
        // selection mode, cursor on/off) are facts about the grab that editing cannot
        // change. A source without a comment (an imported/external image) yields `None`
        // and `save_png` then writes no chunk — the honest answer, not an invented one.
        let provenance = crate::media::png::read_png_metadata(src).unwrap_or_default();
        if !crate::media::png::save_png(&rgba, dst, &provenance) {
            return Err(err(format!("could not write the baked PNG to {}", dst.display())));
        }
        std::fs::metadata(dst).map(|m| m.len())
    } else {
        // No pixel edit at all: this is the plain delivery, which has its own (documented)
        // rule about when bytes may be copied instead of re-encoded.
        save_unedited_still(src, dst)
    }
}

/// The 8-byte PNG signature — the ONLY honest way to ask "is this file a PNG". Deliberately
/// not `ext_of(path) == "png"`: a file's name is the one thing that has already been wrong
/// here, and the whole of DRAGON-455 is about not trusting it.
fn is_png_file(path: &Path) -> bool {
    use std::io::Read;
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut head = [0u8; 8];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head == SIGNATURE
}

/// Deliver an UNEDITED still to `dst` — a Save As (or a Copy) of a capture nobody has drawn
/// on. Returns `dst`'s size.
///
/// `dst` always NAMES png ([`super::naming::png_name`]) and a still is always WRITTEN as PNG
/// ([`bake_image`]), so there are exactly two cases, and the byte copy is a decision rather
/// than something we fell into:
///
/// * a source that already IS a PNG is copied BYTE FOR BYTE. Re-encoding identical pixels
///   would gain nothing, churn the file, and drop the `--inspect` `Comment` chunk unless we
///   re-attached it (DRAGON-445) — the original bytes carry it for free. PNG in, the same
///   PNG out, is the honest answer.
/// * a source that is NOT a PNG — an external image opened with `--preview`, since the
///   editor reads jpg/webp/gif/… too — is decoded and RE-WRITTEN as a PNG. Copying it would
///   put foreign bytes in a file whose name says PNG, which is the same lie from the other
///   direction (DRAGON-455).
///
/// Which case applies is read from the source's MAGIC BYTES ([`is_png_file`]), never from
/// its name. `dst == src` is a no-op: copying a file onto itself truncates it.
///
/// The DECODE in the second case is still `image`'s, which picks its decoder from the
/// source's extension — so a MISLABELED source (JPEG bytes called `.png`) fails loudly here
/// rather than converting. That is deliberate and it keeps the two halves agreeing:
/// [`bake_image`] decodes the same way, and so does the preview's own open path
/// (`super::image`), which means such a file could never have been displayed in the first
/// place. Making the READ side content-based is a separate, larger call.
pub fn save_unedited_still(src: &Path, dst: &Path) -> std::io::Result<u64> {
    if src != dst {
        if is_png_file(src) {
            std::fs::copy(src, dst)?;
        } else {
            let rgba = ::image::open(src).map_err(|e| std::io::Error::other(e.to_string()))?.into_rgba8();
            // No provenance to carry: a non-PNG source cannot have held a PNG text chunk.
            if !crate::media::png::save_png(&rgba, dst, "") {
                return Err(std::io::Error::other(format!(
                    "could not write {} as a PNG to {}",
                    src.display(),
                    dst.display()
                )));
            }
        }
    }
    std::fs::metadata(dst).map(|m| m.len())
}

/// What a video bake works from: the probed pixel size (for the covermark
/// raster), whether the file has a soundtrack (the cut filtergraph must know),
/// and the timeline's kept spans WHEN content was deleted (`None` = uncut, so
/// the historical no-timeline paths — and their exact ffmpeg invocations —
/// still run).
pub struct VideoBake {
    pub w: u32,
    pub h: u32,
    pub has_audio: bool,
    pub keep: Option<Vec<Span>>,
}

/// The `-filter_complex` graph exporting kept spans: per-span `trim`/`atrim`
/// chains re-stamped to zero, concatenated, with the covermark overlaid on the
/// joined video when present. Labels `[v]` (and `[a]` when `has_audio`) are
/// what the caller maps.
fn cut_filtergraph(keep: &[Span], has_audio: bool, overlay: bool) -> String {
    let mut graph = String::new();
    for (i, s) in keep.iter().enumerate() {
        graph.push_str(&format!(
            "[0:v]trim=start={:.3}:end={:.3},setpts=PTS-STARTPTS[v{i}];",
            s.start, s.end
        ));
    }
    for i in 0..keep.len() {
        graph.push_str(&format!("[v{i}]"));
    }
    let vout = if overlay { "[vc]" } else { "[v]" };
    graph.push_str(&format!("concat=n={}:v=1:a=0{vout}", keep.len()));
    if overlay {
        graph.push_str(";[vc][1:v]overlay=(W-w)/2:(H-h)/2[v]");
    }
    if has_audio {
        for (i, s) in keep.iter().enumerate() {
            graph.push_str(&format!(
                ";[0:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS[a{i}]",
                s.start, s.end
            ));
        }
        graph.push(';');
        for i in 0..keep.len() {
            graph.push_str(&format!("[a{i}]"));
        }
        graph.push_str(&format!("concat=n={}:v=0:a=1[a]", keep.len()));
    }
    graph
}

/// The x264 re-encode flags every RE-ENCODING video bake path uses (the cut export and
/// the covermark overlay). Named once so the two can never drift.
const VIDEO_REENCODE: [&str; 8] =
    ["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p"];

/// THE video bake's ffmpeg argument plan, as a pure function (DRAGON-398).
///
/// Everything between the INPUTS (`-i src`, plus `-i overlay.png` when `overlay`) and the
/// OUTPUT path. Split out of [`bake_video`] so the invariant CLAUDE.md is explicit about —
/// **an UNCUT timeline must keep its historical ffmpeg invocations byte-identical** — is
/// pinned by unit tests instead of resting on a reading of the code. The three shapes are:
///
/// * **cut** (`keep` = a non-empty span list, i.e. content was DELETED): the
///   [`cut_filtergraph`] trim/concat export. Both streams re-encode — arbitrary trim
///   points cannot stream-copy.
/// * **covermark only** (uncut, `overlay`): the historical single-`overlay` filtergraph,
///   video re-encoded, audio STREAM-COPIED.
/// * **neither** (defensive): `-map 0 -c copy` — every stream copied, nothing re-encoded.
///
/// `keep` is deliberately taken as `Option<&[Span]>` with an EMPTY list treated as uncut:
/// `VideoBake::keep` is already `None` for a razor-only timeline (the caller filters on
/// `Timeline::edited`), and an empty list reaching here must not produce a `concat=n=0`.
pub(super) fn video_bake_args(
    keep: Option<&[Span]>,
    has_audio: bool,
    overlay: bool,
    ext: &str,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut push = |xs: &[&str]| args.extend(xs.iter().map(|s| s.to_string()));
    match keep.filter(|k| !k.is_empty()) {
        Some(keep) => {
            // Timeline export: keep only the spans, hard-cut seams. Both streams
            // re-encode — trim points are arbitrary, so stream-copy can't hold them.
            let graph = cut_filtergraph(keep, has_audio, overlay);
            push(&["-filter_complex", &graph]);
            push(&["-map", "[v]"]);
            if has_audio {
                push(&["-map", "[a]"]);
            }
            push(&VIDEO_REENCODE);
            if has_audio {
                push(&["-c:a", "aac", "-b:a", "192k"]);
            }
        }
        None if overlay => {
            push(&["-filter_complex", "[0:v][1:v]overlay=(W-w)/2:(H-h)/2[v]"]);
            push(&["-map", "[v]", "-map", "0:a?"]);
            push(&VIDEO_REENCODE);
            push(&["-c:a", "copy"]);
        }
        // No edit to bake (defensive): copy every stream, no re-encode.
        None => push(&["-map", "0", "-c", "copy"]),
    }
    if ext == "mp4" || ext == "m4v" || ext == "mov" {
        push(&["-movflags", "+faststart"]);
    }
    args
}

/// Bake the pending edits onto a video, reading `src` and writing `dst`. Deleted
/// timeline segments export through a `trim`+`concat` filtergraph (video re-encoded,
/// audio re-encoded once); a covermark overlays the (joined) video; with neither,
/// the streams are copied (fast). Either `cm.is_some()` or `video.keep.is_some()`
/// must hold.
pub fn bake_video(src: &Path, dst: &Path, cm: Option<&Covermark>, video: &VideoBake) -> std::io::Result<u64> {
    let err = |e: String| std::io::Error::other(e);
    let dir = PathBuf::from(crate::util::runtime_dir());
    // Rasterize the covermark (if any) up front; remember the temp PNG to clean up.
    let overlay_png = match cm {
        Some(cm) => {
            let overlay = rasterize(cm, video.w.max(1), video.h.max(1))
                .ok_or_else(|| err("covermark rasterize failed".into()))?;
            let p = dir.join("cck-cm.png");
            overlay.save_with_format(&p, ::image::ImageFormat::Png).map_err(|e| err(e.to_string()))?;
            Some(p)
        }
        None => None,
    };
    let mut cmd = crate::util::ffmpeg_command();
    cmd.args(["-y", "-v", "error", "-i"]).arg(src);
    if let Some(p) = &overlay_png {
        cmd.arg("-i").arg(p);
    }
    let ext = super::ext_of(dst).unwrap_or_else(|| "mp4".into());
    let tmp = dir.join(format!("cck-bake.{ext}"));
    // The whole argument plan (cut / covermark-only / stream-copy, plus faststart) is
    // [`video_bake_args`] — pure, so the "an uncut timeline never re-encodes" invariant is
    // unit-tested rather than assumed.
    cmd.args(video_bake_args(
        video.keep.as_deref(),
        video.has_audio,
        overlay_png.is_some(),
        &ext,
    ));
    cmd.arg(&tmp);
    let out = cmd.output()?;
    if let Some(p) = &overlay_png {
        let _ = std::fs::remove_file(p);
    }
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(String::from_utf8_lossy(&out.stderr).into_owned()));
    }
    // Move the encoded result into place (copy+remove across filesystems).
    if std::fs::rename(&tmp, dst).is_err() {
        std::fs::copy(&tmp, dst)?;
        let _ = std::fs::remove_file(&tmp);
    }
    std::fs::metadata(dst).map(|m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(text: &str, caret: usize) -> TextSnapshot {
        TextSnapshot { text: text.to_string(), caret, anchor: None }
    }

    /// DRAGON-354 item 13: the in-session text history — record/undo/redo, typing coalesce, and
    /// redo-dies-on-new-input, all pure.
    #[test]
    fn text_edit_history_records_undo_redo_and_coalesces() {
        let mut h = TextEditHistory::default();
        // A burst of single-char typing "" → "a" → "ab" → "abc" coalesces into ONE step: only the
        // first (pre-burst empty) snapshot is recorded.
        h.record(snap("", 0), true);
        h.record(snap("a", 1), true);
        h.record(snap("ab", 2), true);
        // One undo from "abc" restores the whole burst's pre-state ("").
        assert_eq!(h.undo(snap("abc", 3)), Some(snap("", 0)));
        // Redo returns to "abc".
        assert_eq!(h.redo(snap("", 0)), Some(snap("abc", 3)));
        // Exhausted undo/redo are no-ops.
        assert_eq!(h.undo(snap("abc", 3)), Some(snap("", 0)));
        assert_eq!(h.undo(snap("", 0)), None);

        // A word-break (coalesce=false) starts a NEW step, so two words = two undo steps.
        let mut h = TextEditHistory::default();
        h.record(snap("", 0), true); // 'h'
        h.record(snap("h", 1), true); // 'i' (coalesced)
        h.record(snap("hi", 2), false); // ' ' breaks the burst → its own step
        h.record(snap("hi ", 3), true); // 'y'
        h.record(snap("hi y", 4), true); // 'o' (coalesced)
        assert_eq!(h.undo(snap("hi yo", 5)), Some(snap("hi ", 3)), "undo the second word");
        assert_eq!(h.undo(snap("hi ", 3)), Some(snap("hi", 2)), "undo the space step");
        assert_eq!(h.undo(snap("hi", 2)), Some(snap("", 0)), "undo the first word");
        assert_eq!(h.undo(snap("", 0)), None);
    }

    /// New input after an undo CLEARS the redo stack (standard semantics), and `break_burst`
    /// forces the next typing char to start a fresh step.
    #[test]
    fn text_edit_history_new_input_clears_redo_and_break_ends_burst() {
        let mut h = TextEditHistory::default();
        h.record(snap("", 0), false); // step for "x"
        assert_eq!(h.undo(snap("x", 1)), Some(snap("", 0)));
        // A fresh edit after the undo: redo must be gone.
        h.record(snap("", 0), false);
        assert_eq!(h.redo(snap("y", 1)), None, "redo cleared by new input");

        // break_burst ends coalescing so two same-kind typing chars become two steps.
        let mut h = TextEditHistory::default();
        h.record(snap("", 0), true);
        h.break_burst();
        h.record(snap("a", 1), true);
        assert_eq!(h.undo(snap("ab", 2)), Some(snap("a", 1)), "second char is its own step");
        assert_eq!(h.undo(snap("a", 1)), Some(snap("", 0)));
    }

    #[test]
    fn flyout_nav_wraps_and_starts_from_no_highlight() {
        let mut f = FlyoutNav { kind: FlyoutKind::Color, selected: None, len: 4 };
        f.nav(1);
        assert_eq!(f.selected, Some(0), "None + forward → first");
        f.nav(-1);
        assert_eq!(f.selected, Some(3), "0 - 1 wraps to last");
        f.nav(1);
        assert_eq!(f.selected, Some(0), "last + 1 wraps to first");
        // Covermark uses the same nav; None - 1 → last.
        let mut g = FlyoutNav { kind: FlyoutKind::Covermark, selected: None, len: 3 };
        g.nav(-1);
        assert_eq!(g.selected, Some(2));
        // Empty is a no-op (never a divide/panic).
        let mut h = FlyoutNav { kind: FlyoutKind::Color, selected: None, len: 0 };
        h.nav(1);
        assert_eq!(h.selected, None);
    }

    /// DRAGON-362 — the REPLACEMENT for the old `covermark_raster_size_scales_with_zoom_
    /// capped_at_frame`. That test pinned the OLD contract: a fixed ≤1024 box at fit zoom,
    /// grown by `max(zoom, 1)`. The contract is now "raster at the layer's ON-SCREEN
    /// device-pixel footprint" (`view_zoom × visual_scale` of the source frame), which is what
    /// stops a big capture's overlay being upsampled beside a downsampled base image. The
    /// surviving properties — grows with zoom, never past the source frame, never zero — are
    /// re-asserted here against the new formula.
    #[test]
    fn covermark_raster_size_tracks_the_on_screen_footprint_capped_at_frame() {
        let mut e = EditState { frame: (4000, 2000), ..Default::default() };
        // A 4000-wide capture fitted to ~2000 px of screen (visual_scale 0.5): at fit zoom the
        // covermark is rastered at the 2000 px it actually occupies — under the OLD contract
        // this was 1024, i.e. upsampled ~2× on screen.
        assert_eq!(e.covermark_raster_size(1.0, 0.5), (2000, 1000));
        // Zooming in grows it proportionally...
        assert_eq!(e.covermark_raster_size(1.5, 0.5), (3000, 1500));
        // ...capped at the full source frame (no detail beyond it; matches the bake).
        assert_eq!(e.covermark_raster_size(100.0, 0.5), (4000, 2000));
        // Zooming OUT now genuinely shrinks the raster (it is genuinely smaller on screen) —
        // the old baseline floor is gone, and that is a pure win for the per-edit cost.
        assert_eq!(e.covermark_raster_size(0.5, 0.5), (1000, 500));
        // A zero/unknown frame falls back to a sane default (never a 0-size raster).
        e.frame = (0, 0);
        let (w, h) = e.covermark_raster_size(3.0, 0.5);
        assert!(w > 0 && h > 0);
    }

    /// DRAGON-391 — the SAME DRAGON-362 contract, measured against the covermark's canvas (the
    /// image) rather than the source frame: a crop makes the image the crop rect, so the raster
    /// tracks THAT footprint. The properties are untouched — it still scales with the on-screen
    /// device pixels and still caps at 1:1 with the canvas's own pixels; only WHAT it spans
    /// followed the crop, which is what lets the mark cover the extension without overshooting it.
    #[test]
    fn dragon391_covermark_raster_size_tracks_the_cropped_image() {
        let mut e = EditState { frame: (4000, 2000), ..Default::default() };
        // An over-crop 1000 px LEFT of the source, 2000×2500 (taller than the source — the black
        // extension): at half the on-screen footprint the raster is half the CROP, not the frame.
        e.crop = Some(CropRect { x: -1000.0, y: 0.0, w: 2000.0, h: 2500.0 });
        assert_eq!(e.covermark_raster_size(1.0, 0.5), (1000, 1250));
        // The cap is 1:1 with the crop — no detail beyond it, matching the bake, which rasterizes
        // exactly the cut canvas's pixels.
        assert_eq!(e.covermark_raster_size(100.0, 0.5), (2000, 2500));
        // A crop SMALLER than the frame shrinks the raster with it (it is genuinely smaller now).
        e.crop = Some(CropRect { x: 0.0, y: 0.0, w: 800.0, h: 400.0 });
        assert_eq!(e.covermark_raster_size(1.0, 1.0), (800, 400));
    }

    /// The scale is the on-screen footprint fraction, snapped UP to a [`RASTER_QUANTUM`] step
    /// and capped at the source resolution.
    #[test]
    fn layer_raster_scale_snaps_up_and_caps_at_source() {
        // Exactly on a quantum boundary stays put.
        assert!((layer_raster_scale(1.0, 0.5) - 0.5).abs() < 1e-6);
        // Between boundaries rounds UP — never down, so the raster is never smaller than the
        // layer's on-screen size (which is what would make it soft).
        let s = layer_raster_scale(1.0, 0.4512);
        assert!(s >= 0.4512, "snapped {s} must not fall below the wanted 0.4512");
        assert!((s - 0.5).abs() < 1e-6, "0.4512 snaps up to the 8/16 step, got {s}");
        // Zoom multiplies the footprint...
        assert!((layer_raster_scale(2.0, 0.25) - 0.5).abs() < 1e-6);
        // ...and the cap is the source resolution, never beyond.
        assert!((layer_raster_scale(64.0, 0.5) - 1.0).abs() < 1e-6);
        assert!((layer_raster_scale(1.0, 4.0) - 1.0).abs() < 1e-6);
        // Degenerate geometry (unknown scale) → full source resolution, never a soft guess.
        assert!((layer_raster_scale(1.0, 0.0) - 1.0).abs() < 1e-6);
        assert!((layer_raster_scale(0.0, 0.5) - 1.0).abs() < 1e-6);
        assert!((layer_raster_scale(f32::NAN, 0.5) - 1.0).abs() < 1e-6);
    }

    /// Quantization is what keeps a zoom DRAG from re-rendering (and re-creating the layer's
    /// GPU texture) on every tick: neighbouring zooms inside one step resolve to the SAME
    /// raster, so `refresh_*_for_zoom`'s "wanted == current" check short-circuits.
    #[test]
    fn layer_raster_scale_is_stable_within_a_quantum() {
        let e = EditState { frame: (5120, 2880), ..Default::default() };
        let a = e.covermark_raster_size(1.00, 0.4512);
        let b = e.covermark_raster_size(1.02, 0.4512);
        let c = e.covermark_raster_size(1.05, 0.4512);
        assert_eq!(a, b, "a nudge inside one quantum must not resize the raster");
        assert_eq!(a, c);
        // A step big enough to cross a quantum boundary DOES grow it.
        assert!(e.covermark_raster_size(1.30, 0.4512).0 > a.0);
    }

    /// The 5K case from the bug report, end to end: a 5120×2880 capture shown ~2311 px wide
    /// (visual_scale ≈ 0.4514) rasters its layers at ~the on-screen size, NOT the old 1024 box.
    #[test]
    fn five_k_capture_at_fit_zoom_rasters_at_screen_size_not_1024() {
        let e = EditState { frame: (5120, 2880), ..Default::default() };
        let (w, h) = e.covermark_raster_size(1.0, 2311.0 / 5120.0);
        assert!(w >= 2311, "raster {w} must cover the 2311 px it occupies on screen");
        assert!(w < 5120, "and must not needlessly reach full source resolution: {w}");
        assert_eq!((w, h), (2560, 1440), "the 8/16 quantum above 0.4514");
        // The OLD contract's answer, for the record — 2.26× short of the screen size.
        assert!(w > 1024 * 2);
    }

    /// [`MAX_LAYER_DIM`] bounds the raster for captures larger than a GPU texture: an
    /// "all displays" grab is easily wider than 8192 px, and the frame cap alone would ask for
    /// a texture the device cannot allocate. The clamp is ISOTROPIC — the aspect is preserved.
    #[test]
    fn layer_raster_dims_clamp_to_the_texture_limit_without_stretching() {
        // A 3-monitor 10240×2880 grab at full source scale.
        let (w, h) = layer_raster_dims((10240, 2880), 1.0);
        assert!(w <= MAX_LAYER_DIM && h <= MAX_LAYER_DIM, "{w}x{h} exceeds the texture limit");
        assert_eq!(w, MAX_LAYER_DIM);
        let want = 10240.0 / 2880.0;
        assert!((w as f32 / h as f32 - want).abs() < 1e-2, "aspect drift: {w}x{h}");
        // Below the limit nothing is clamped.
        assert_eq!(layer_raster_dims((5120, 2880), 1.0), (5120, 2880));
        // A TALL over-limit frame clamps on the other axis, same isotropy.
        let (w, h) = layer_raster_dims((2880, 10240), 1.0);
        assert_eq!(h, MAX_LAYER_DIM);
        assert!((w as f32 / h as f32 - 2880.0 / 10240.0).abs() < 1e-2);
    }

    /// Dimensions never collapse to zero, whatever the scale.
    #[test]
    fn layer_raster_dims_never_zero() {
        for frame in [(1u32, 1u32), (5120, 2880), (0, 0)] {
            for scale in [0.0f32, 1e-6, 0.5, 1.0, 2.0] {
                let (w, h) = layer_raster_dims(frame, scale);
                assert!(w >= 1 && h >= 1, "{frame:?} @ {scale} → {w}x{h}");
            }
        }
    }

    /// A placed layer's `dest` is its region expressed as fractions of the picture — the
    /// contract `layers.rs`'s vertex shader consumes. A full-picture region round-trips to
    /// `DEST_FULL`, which is byte-identical to the pre-DRAGON-362 full-canvas quad.
    #[test]
    fn text_layer_geom_dest_is_the_region_as_canvas_fractions() {
        use super::super::annotate::AnnotRect;
        let frame = (5120u32, 2880u32);
        let g = TextLayerGeom {
            scale: 0.5,
            region: AnnotRect { x: 1280.0, y: 720.0, w: 2560.0, h: 720.0 },
            px: (1280, 360),
        };
        assert_eq!(g.dest(frame), [0.25, 0.25, 0.5, 0.25]);
        // The whole picture → the whole canvas (the historical quad).
        let whole = TextLayerGeom {
            scale: 1.0,
            region: AnnotRect { x: 0.0, y: 0.0, w: 5120.0, h: 2880.0 },
            px: frame,
        };
        assert_eq!(whole.dest(frame), super::super::layers::DEST_FULL);
        // A degenerate frame degrades to the whole canvas rather than dividing by zero.
        assert_eq!(g.dest((0, 0)), super::super::layers::DEST_FULL);
    }

    /// DRAGON-385: `dest_in` measures the caption against the DISPLAY frame — the crop region when
    /// a crop is applied — so a text box inside the crop lands right and one outside falls outside
    /// `[0, 1]` (clipped away). With a zero origin + the whole frame it equals the un-cropped
    /// `dest`.
    #[test]
    fn text_layer_geom_dest_in_is_relative_to_the_crop() {
        use super::super::annotate::AnnotRect;
        let g = TextLayerGeom {
            scale: 0.5,
            region: AnnotRect { x: 1280.0, y: 720.0, w: 640.0, h: 360.0 },
            px: (640, 360),
        };
        // Un-cropped (origin 0, whole frame) is the historical placement.
        assert_eq!(
            g.dest_in((0.0, 0.0), (5120.0, 2880.0)),
            [0.25, 0.25, 0.125, 0.125],
        );
        // A crop taken at (1280, 720) sized 1280x720: the box sits at the crop's top-left and
        // spans half its width / height.
        assert_eq!(g.dest_in((1280.0, 720.0), (1280.0, 720.0)), [0.0, 0.0, 0.5, 0.5]);
        // A caption LEFT of the crop origin maps to a negative fraction (clipped away on draw).
        let left = TextLayerGeom {
            scale: 1.0,
            region: AnnotRect { x: 0.0, y: 720.0, w: 100.0, h: 100.0 },
            px: (100, 100),
        };
        assert!(left.dest_in((1280.0, 720.0), (1280.0, 720.0))[0] < 0.0);
    }

    /// DRAGON-396: the two caption PLACEMENTS must agree on screen — the historical form (stretch
    /// the layer across the picture, locate the caption by these `dest_in` fractions) and the crop
    /// session's (place the layer at the caption's own region through the annotation canvas's
    /// `CanvasMap`, which is what lets a caption OUTSIDE the image be drawn at all: a shader is
    /// clipped to its own widget rect, so a picture-wide layer can never leave the picture).
    ///
    /// Switching forms must move a caption's BOUND, never the caption. Checked un-cropped AND
    /// against a crop with a NEGATIVE origin (the over-crop case), since the crop shift reaches the
    /// two forms by different routes: `dest_in`'s origin argument vs the map's own.
    #[test]
    fn dragon396_region_placement_matches_the_picture_fraction_form() {
        use crate::widgets::annotation_canvas::{CanvasMap, region_on_screen};
        use super::super::annotate::AnnotRect;
        for (offset, source) in
            [((0.0f32, 0.0f32), (800.0f32, 600.0f32)), ((-120.0, -40.0), (500.0, 400.0))]
        {
            let map = CanvasMap {
                bounds: (1000.0, 700.0),
                zoom: 0.5,
                pan: (30.0, -12.0),
                disp: (640.0, 480.0),
                source,
                offset,
            };
            let origin = (17.0, 23.0);
            let picture = region_on_screen(&map, origin, (offset.0, offset.1, source.0, source.1));
            // A caption inside the image, and one wholly OUTSIDE it (left of a negative-origin
            // crop) — the case this ticket exists for.
            for region in [
                AnnotRect { x: offset.0 + 40.0, y: offset.1 + 30.0, w: 160.0, h: 90.0 },
                AnnotRect { x: offset.0 - 220.0, y: offset.1 - 60.0, w: 120.0, h: 50.0 },
            ] {
                // Form A (crop session): the region mapped to its own on-screen rect.
                let a = region_on_screen(&map, origin, (region.x, region.y, region.w, region.h));
                // Form B (editor): the picture rect with the quad placed inside it by the `dest`
                // fractions — the arithmetic `layers.rs`'s vertex shader performs.
                let geom = TextLayerGeom { scale: 1.0, region, px: (1, 1) };
                let d = geom.dest_in(offset, source);
                let b = (
                    picture.x + d[0] * picture.width,
                    picture.y + d[1] * picture.height,
                    d[2] * picture.width,
                    d[3] * picture.height,
                );
                for (lhs, rhs, what) in [
                    (a.x, b.0, "x"),
                    (a.y, b.1, "y"),
                    (a.width, b.2, "width"),
                    (a.height, b.3, "height"),
                ] {
                    assert!(
                        (lhs - rhs).abs() < 0.01,
                        "{what}: region form {lhs} vs picture-fraction form {rhs} \
                         (offset {offset:?}, region {region:?})",
                    );
                }
            }
        }
    }

    /// DRAGON-391: the covermark's canvas is THE IMAGE — the crop rect once a crop is applied, the
    /// source frame otherwise — and a live crop SESSION reveals the whole image again, so the
    /// canvas reverts to the source for the duration.
    #[test]
    fn dragon391_covermark_canvas_is_the_crop_rect_once_applied() {
        let frame = (4000u32, 2000u32);
        // Un-cropped: the source frame, exactly as always.
        let plain = EditState { frame, ..Default::default() };
        assert_eq!(plain.covermark_raster_size(100.0, 1.0), (4000, 2000));
        // An over-crop 1000 px LEFT of the source: the image is now the 2000×2500 crop — INCLUDING
        // the black extension it adds, and nothing beyond it (NOT the 5000×2500 source ∪ crop
        // union, which would pattern past the image the user cropped to).
        let cropped = EditState {
            frame,
            crop: Some(CropRect { x: -1000.0, y: 0.0, w: 2000.0, h: 2500.0 }),
            ..Default::default()
        };
        assert_eq!(cropped.covermark_raster_size(100.0, 1.0), (2000, 2500));
        // A crop wholly INSIDE the source is the same rule — the image is the crop.
        let inner = EditState {
            frame,
            crop: Some(CropRect { x: 100.0, y: 100.0, w: 1000.0, h: 500.0 }),
            ..Default::default()
        };
        assert_eq!(inner.covermark_raster_size(100.0, 1.0), (1000, 500));
        // ...and the DRAGON-362 footprint scaling still applies on top of that canvas.
        assert_eq!(inner.covermark_raster_size(1.0, 0.5), (500, 250));
        // While a crop SESSION is live the whole image is revealed, so the canvas is the source
        // frame again — the mark covers what the user can actually see while repositioning.
        let rect = CropRect { x: -1000.0, y: 0.0, w: 2000.0, h: 2500.0 };
        let session = EditState {
            frame,
            crop: Some(rect),
            crop_session: Some(CropSession {
                rect,
                saved_view: Default::default(),
                saved_tool: None,
                drag: None,
            }),
            ..Default::default()
        };
        assert_eq!(session.covermark_raster_size(100.0, 1.0), (4000, 2000));
    }

    /// DRAGON-402: the covermark LAYER is hidden for the duration of a crop session, and the
    /// document is untouched by it — so both exits restore the mark exactly, and cancel especially
    /// leaves everything as it was found.
    #[test]
    fn dragon402_a_crop_session_hides_the_covermark_layer_without_touching_the_document() {
        let mark = Covermark { kind: CovermarkKind::Confidential, zoom: 0.35, opacity: 0.6 };
        let mut e = EditState { frame: (4000, 2000), ..Default::default() };
        e.set_covermark(Some(mark.clone()));
        let before = e.covermark.clone();
        let depth = e.undo_stack.len();
        assert!(e.covermark_visible(), "no session → the mark draws, exactly as before");

        // Enter a session (over-cropping past the source, the case that reframes hardest).
        let rect = CropRect { x: -500.0, y: -200.0, w: 3000.0, h: 2400.0 };
        e.crop_session =
            Some(CropSession { rect, saved_view: Default::default(), saved_tool: None, drag: None });
        assert!(!e.covermark_visible(), "a live crop session hides the layer");
        // HIDE, not clear: the model is untouched, so nothing about the document changed — no
        // history entry, and the mark (with its zoom/opacity) is exactly as applied.
        assert_eq!(e.covermark, before, "the session must not touch the covermark model");
        assert_eq!(e.undo_stack.len(), depth, "hiding a layer is not an edit");
        assert!(e.dirty(), "and the document is still dirty for the mark it still carries");

        // Cancel: the session simply ends, and the mark is visible again, unchanged.
        e.crop_session = None;
        assert!(e.covermark_visible());
        assert_eq!(e.covermark, before, "cancel leaves the document as it was found");
    }

    /// DRAGON-410: a crop session forces the global dim CLEAR (maximum brightness, fully
    /// transparent) for its whole duration — you cannot judge a crop through a dimmed image — and
    /// the user's value comes back EXACTLY on both exits.
    ///
    /// The round-trip is proven on the ACCEPT path and the CANCEL path separately, because the
    /// ticket asks for both; they are the same two lines here precisely because the dim is a view
    /// OVERRIDE (nothing is stashed, so no exit can forget to put it back) rather than a
    /// save-and-restore like `saved_view` / `saved_tool`. The rest of the test is the other half
    /// of the requirement: the session must not enter the undo history, must not move `dirty()`,
    /// and must not change what a bake would read.
    #[test]
    fn dragon410_a_crop_session_forces_the_dim_clear_and_restores_it_on_both_exits() {
        let session = |rect: CropRect| CropSession {
            rect,
            saved_view: Default::default(),
            saved_tool: None,
            drag: None,
        };
        let rect = CropRect { x: 10.0, y: 10.0, w: 800.0, h: 600.0 };

        let mut e = EditState { frame: (1000, 800), ..Default::default() };
        // A user-set dim, applied the ordinary way (one history entry, as the slider commits).
        e.push_dim(e.dim);
        e.dim = 0.62;
        let depth = e.undo_stack.len();
        assert_eq!(e.view_dim(), 0.62, "no session → the view renders the user's dim");
        assert!(e.dirty(), "a non-zero dim is a real edit and still bakes");

        // ── ACCEPT path ──────────────────────────────────────────────────────────────────
        e.crop_session = Some(session(rect));
        assert_eq!(e.view_dim(), 0.0, "a live session renders at maximum brightness");
        // A VIEW override: the model, the history and the bake gate are all untouched, so
        // nothing about the document changed and nothing can leak into a save.
        assert_eq!(e.dim, 0.62, "the session must not touch the dim model");
        assert_eq!(e.undo_stack.len(), depth, "forcing the view clear is not an edit");
        assert!(e.redo_stack.is_empty(), "…and cannot have cleared a redo stack");
        assert!(e.dirty(), "the document is still dirty for the dim it still carries");
        // Accept commits the crop and ends the session; the dim comes back exactly.
        e.set_crop(Some(rect));
        e.crop_session = None;
        assert_eq!(e.view_dim(), 0.62, "accept returns the post-crop view to the user's dim");
        assert_eq!(e.dim, 0.62);

        // ── CANCEL path ──────────────────────────────────────────────────────────────────
        let after_accept = (e.dim, e.undo_stack.len());
        e.crop_session = Some(session(rect));
        assert_eq!(e.view_dim(), 0.0);
        // Cancel discards the session and touches nothing else at all.
        e.crop_session = None;
        assert_eq!(e.view_dim(), 0.62, "cancel restores the user's dim exactly");
        assert_eq!((e.dim, e.undo_stack.len()), after_accept, "cancel changed nothing");

        // A ZERO dim round-trips as zero — the override is not a floor of its own.
        let mut plain = EditState { frame: (100, 80), ..Default::default() };
        assert_eq!(plain.view_dim(), 0.0);
        plain.crop_session = Some(session(CropRect { x: 0.0, y: 0.0, w: 50.0, h: 40.0 }));
        assert_eq!(plain.view_dim(), 0.0);
        plain.crop_session = None;
        assert_eq!(plain.view_dim(), 0.0);
    }

    /// The layer READ is where the rule lives (DRAGON-402), so no mount can honour it and another
    /// forget: with a session live there is no raster to draw, even though the slot still holds one
    /// — which is what makes the restore on exit a redraw rather than a re-render.
    #[test]
    fn dragon402_the_layer_read_is_empty_during_a_session_though_the_slot_is_not() {
        let mut e = EditState { frame: (100, 80), ..Default::default() };
        e.set_covermark(Some(Covermark {
            kind: CovermarkKind::Confidential,
            zoom: 0.0,
            opacity: 1.0,
        }));
        // A raster in the slot, as a live document would have.
        let frame = super::super::layers::PixelFrame::new(vec![0u8; 4 * 4 * 4], 4, 4);
        let generation = e.cm_raster.begin().expect("a fresh slot is not refreshing");
        e.cm_raster.finish(generation, Some(frame));
        assert!(e.cm_raster.frame().is_some(), "the slot holds a raster");
        assert!(e.covermark_layer().is_some(), "…and it draws with no session");

        let rect = CropRect { x: 0.0, y: 0.0, w: 50.0, h: 40.0 };
        e.crop_session =
            Some(CropSession { rect, saved_view: Default::default(), saved_tool: None, drag: None });
        assert!(e.covermark_layer().is_none(), "a session yields nothing to draw");
        assert!(e.cm_raster.frame().is_some(), "…but the slot KEEPS it, ready for the exit");

        e.crop_session = None;
        assert!(e.covermark_layer().is_some(), "the same raster draws again on exit");
    }

    /// DRAGON-352: `dirty()` is THE shared bake gate — Copy/Save (`begin_bake`) and
    /// Save As all read it through `PreviewState::dirty()` (which ORs in DELETED
    /// timeline content for videos). Every result-changing edit kind must trip it
    /// alone, and a pristine state must not (no needless re-encode).
    #[test]
    fn dirty_trips_on_every_result_changing_edit() {
        use super::super::annotate::{AnnotKind, AnnotRect};

        assert!(!EditState::default().dirty(), "pristine state must not bake");
        let cm = EditState {
            covermark: Some(Covermark { kind: CovermarkKind::Confidential, zoom: 0.0, opacity: 1.0 }),
            ..Default::default()
        };
        assert!(cm.dirty(), "a covermark alone must bake");
        let annot = EditState {
            annotations: vec![AnnotationItem {
                id: AnnotId(1),
                color: [255, 0, 0, 255],
                kind: AnnotKind::Box {
                    rect: AnnotRect { x: 1.0, y: 1.0, w: 10.0, h: 10.0 },
                    stroke_w: 2.0,
                    fill: None,
                },
            }],
            ..Default::default()
        };
        assert!(annot.dirty(), "any annotation alone must bake");
        let dim = EditState { dim: 0.25, ..Default::default() };
        assert!(dim.dirty(), "a non-zero global dim alone must bake");
    }

    fn flat(w: u32, h: u32, v: u8) -> RgbaImage {
        RgbaImage::from_pixel(w, h, ::image::Rgba([v, v, v, 255]))
    }

    #[test]
    fn confidential_rasterizes_and_composites() {
        let cm = Covermark { kind: CovermarkKind::Confidential, zoom: 0.0, opacity: 1.0 };
        let Some(overlay) = rasterize(&cm, 400, 300) else {
            panic!("confidential covermark failed to rasterize");
        };
        assert_eq!(overlay.dimensions(), (400, 300));
        assert!(overlay.pixels().any(|p| p.0[3] > 0), "covermark rendered fully transparent");
        let mut base = flat(500, 400, 200);
        let before = base.clone();
        apply_covermark(&mut base, Some(&cm));
        assert_ne!(base, before, "composite left the base unchanged");
    }

    #[test]
    fn text_covermark_renders_configured_text() {
        let cm = Covermark { kind: CovermarkKind::Text("SECRET".into()), zoom: 0.0, opacity: 1.0 };
        assert!(rasterize(&cm, 300, 200).is_some(), "text covermark failed to rasterize");
        // Empty text falls back to the default prompt string, still valid SVG.
        let empty = Covermark { kind: CovermarkKind::Text("   ".into()), zoom: 0.0, opacity: 1.0 };
        assert!(rasterize(&empty, 300, 200).is_some());
    }

    #[test]
    fn zoom_still_covers_the_whole_frame() {
        // A zoomed covermark must still produce a full-frame raster (fill invariant).
        let mut cm = Covermark { kind: CovermarkKind::Confidential, zoom: 0.0, opacity: 1.0 };
        cm.zoom = 2.5;
        let overlay = rasterize(&cm, 320, 240).expect("zoomed rasterize");
        assert_eq!(overlay.dimensions(), (320, 240));
    }

    #[test]
    fn undo_redo_track_covermark_history() {
        let mut edit = EditState::default();
        assert!(!edit.can_undo() && !edit.can_redo());
        edit.set_covermark(Some(Covermark { kind: CovermarkKind::Confidential, zoom: 0.0, opacity: 1.0 }));
        assert!(edit.dirty() && edit.can_undo() && !edit.can_redo());
        assert_eq!(edit.undo(None), Some(EditKind::Covermark));
        assert!(!edit.dirty() && edit.can_redo());
        assert_eq!(edit.redo(None), Some(EditKind::Covermark));
        assert!(edit.dirty());
        assert_eq!(edit.redo(None), None, "redo with an empty stack must be a no-op");
        // Zoom adjusts in place without adding history.
        let undo_depth = edit.undo_stack.len();
        edit.set_zoom(1.5);
        assert_eq!(edit.zoom(), 1.5);
        assert_eq!(edit.undo_stack.len(), undo_depth, "zoom must not push undo history");
    }

    #[test]
    fn undo_redo_interleave_covermark_and_timeline_ops() {
        let mut edit = EditState::default();
        let mut tl = Timeline::new(10.0);
        // Edit sequence: covermark on → cut at 4 → delete the tail segment.
        edit.set_covermark(Some(Covermark { kind: CovermarkKind::Confidential, zoom: 0.0, opacity: 1.0 }));
        let prev = tl.spans.clone();
        assert!(tl.cut_at_source(4.0));
        edit.push_timeline(prev);
        let prev = tl.spans.clone();
        assert!(tl.delete(1));
        edit.push_timeline(prev);
        assert!(tl.edited());
        // Undo walks newest-first: delete, then cut, then covermark.
        assert_eq!(
            edit.undo(Some(&mut tl)),
            Some(EditKind::Timeline),
            "timeline undo must not report a covermark change"
        );
        assert_eq!(tl.spans.len(), 2);
        assert!(!tl.edited(), "undoing the delete restores the content");
        assert_eq!(edit.undo(Some(&mut tl)), Some(EditKind::Timeline));
        assert_eq!(tl.spans.len(), 1, "undoing the cut re-joins the spans");
        assert_eq!(edit.undo(Some(&mut tl)), Some(EditKind::Covermark), "covermark undo reports the change");
        assert!(!edit.dirty());
        // Redo replays in order: covermark, cut, delete.
        assert_eq!(edit.redo(Some(&mut tl)), Some(EditKind::Covermark));
        assert!(edit.dirty());
        assert_eq!(edit.redo(Some(&mut tl)), Some(EditKind::Timeline));
        assert_eq!(tl.spans.len(), 2);
        assert_eq!(edit.redo(Some(&mut tl)), Some(EditKind::Timeline));
        assert!(tl.edited());
        assert!(!edit.can_redo());
        // A fresh timeline edit clears redo, like a fresh covermark choice.
        // (This undo pops the timeline delete — a timeline change, not covermark.)
        assert_eq!(edit.undo(Some(&mut tl)), Some(EditKind::Timeline));
        assert!(edit.can_redo());
        edit.push_timeline(tl.spans.clone());
        assert!(!edit.can_redo());
    }

    #[test]
    fn dim_is_dirty_and_joins_the_shared_undo_history() {
        // DRAGON-329: a non-zero global dim is a bakeable edit; its slider commits ONE undo
        // entry that interleaves with the rest of the shared history.
        let mut edit = EditState::default();
        assert!(!edit.dirty(), "a fresh editor with dim 0 is clean");
        // A drag: pre-value latched, value moved, ONE entry pushed on commit.
        edit.dim = 0.5;
        edit.push_dim(0.0);
        assert!(edit.dirty(), "a non-zero dim needs a bake");
        assert!(edit.can_undo());
        // Undo restores the pre-drag dim and reports a Dim change (no raster owed).
        assert_eq!(edit.undo(None), Some(EditKind::Dim));
        assert_eq!(edit.dim, 0.0);
        assert!(!edit.dirty());
        // Redo replays it.
        assert_eq!(edit.redo(None), Some(EditKind::Dim));
        assert_eq!(edit.dim, 0.5);
        // A fresh edit clears the redo stack, like any other op.
        edit.push_dim(0.5);
        assert!(!edit.can_redo());
    }

    #[test]
    fn ensure_dim_for_spotlights_seeds_only_when_a_spotlight_exists() {
        use crate::app::preview::annotate::{AnnotId, AnnotKind, AnnotRect, AnnotationItem};
        let rect = AnnotRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        let mut edit = EditState::default();
        // Nothing on the canvas → no-op.
        edit.ensure_dim_for_spotlights();
        assert_eq!(edit.dim, 0.0);
        // A non-spotlight annotation → still no dim.
        edit.annotations.push(AnnotationItem {
            id: AnnotId(1),
            color: [255, 0, 0, 255],
            kind: AnnotKind::Box { rect, stroke_w: 4.0, fill: None },
        });
        edit.ensure_dim_for_spotlights();
        assert_eq!(edit.dim, 0.0, "a box doesn't dim");
        // A spotlight with no dim → seed 0.7, as one undo entry.
        edit.annotations.push(AnnotationItem {
            id: AnnotId(2),
            color: [0, 0, 0, 255],
            kind: AnnotKind::Spotlight { rect },
        });
        edit.ensure_dim_for_spotlights();
        assert_eq!(edit.dim, 0.7, "a spotlight seeds the dim");
        assert!(edit.can_undo(), "the seed is undoable");
        // Idempotent once dimmed — never fights a dim the user has already set.
        edit.ensure_dim_for_spotlights();
        assert_eq!(edit.dim, 0.7);
    }

    #[test]
    fn text_outline_width_restyle_joins_the_shared_undo_history() {
        // DRAGON-358: re-styling a text box's line width is ONE `EditOp::Annotations` snapshot on
        // the shared history (the width mirror of the color-restyle flow) — undo restores the
        // prior outline weight, redo re-applies it, exactly like a recolor. This pins the
        // push-snapshot-then-mutate shape `restroke_selected_annotation` uses for text.
        use crate::app::preview::annotate::{AnnotId, AnnotKind, AnnotRect, AnnotationItem};
        let rect = AnnotRect { x: 0.0, y: 0.0, w: 40.0, h: 20.0 };
        let text = |w: f32| AnnotKind::Text {
            rect,
            text: "hi".to_string(),
            size_px: 24.0,
            font: crate::app::preview::text_annot::TextFont::Clean,
            constrained: false,
            stroke_w: w,
        };
        let mut edit = EditState::default();
        edit.annotations.push(AnnotationItem { id: AnnotId(1), color: [255, 0, 0, 255], kind: text(2.0) });
        // Restyle to a wider pencil: snapshot the pre-edit scene, then mutate — one undo entry.
        let prev = edit.annotations.clone();
        if let AnnotKind::Text { stroke_w, .. } = &mut edit.annotations[0].kind {
            *stroke_w = 6.0;
        }
        edit.push_annotations(prev);
        let width = |e: &EditState| match &e.annotations[0].kind {
            AnnotKind::Text { stroke_w, .. } => *stroke_w,
            _ => unreachable!(),
        };
        assert_eq!(width(&edit), 6.0, "the wider outline is applied");
        assert!(edit.can_undo());
        edit.undo(None);
        assert_eq!(width(&edit), 2.0, "undo restores the prior outline weight");
        edit.redo(None);
        assert_eq!(width(&edit), 6.0, "redo re-applies it");
    }

    #[test]
    fn mid_edit_width_restyle_folds_into_the_settle_snapshot() {
        // DRAGON-358 review fix: a width click DURING an active text-edit session must NOT push
        // its own undo entry — the settle owns the single snapshot (the pre-edit scene), exactly
        // like the size/font restyles (`apply_text_style`'s gate) and the recolor. This models
        // the gated flow: mutate with no push while `text_edit` is live, then settle-push the
        // session snapshot once — exactly ONE undo entry, restoring the full pre-edit scene.
        use crate::app::preview::annotate::{AnnotId, AnnotKind, AnnotRect, AnnotationItem};
        let rect = AnnotRect { x: 0.0, y: 0.0, w: 40.0, h: 20.0 };
        let text = |s: &str, w: f32| AnnotKind::Text {
            rect,
            text: s.to_string(),
            size_px: 24.0,
            font: crate::app::preview::text_annot::TextFont::Clean,
            constrained: false,
            stroke_w: w,
        };
        let mut edit = EditState::default();
        edit.annotations.push(AnnotationItem { id: AnnotId(1), color: [255, 0, 0, 255], kind: text("hi", 2.0) });
        // The edit session opens on the box: the settle snapshot IS the pre-edit scene.
        let snapshot = edit.annotations.clone();
        edit.text_edit = Some(TextEdit {
            id: AnnotId(1),
            caret: 2,
            anchor: None,
            snapshot: snapshot.clone(),
            is_new: false,
            blink_on: true,
            history: Default::default(),
        });
        // Mid-edit: type a character AND click a width — both mutate, NEITHER pushes (the
        // restyle gate on `text_edit.is_none()`).
        edit.annotations[0].kind = text("hi!", 6.0);
        assert!(!edit.can_undo(), "nothing pushed while the session is live");
        // The settle: the scene changed, so ONE entry holding the pre-edit scene is pushed.
        let te = edit.text_edit.take().expect("session live");
        assert_ne!(te.snapshot, edit.annotations, "the settle sees a changed scene");
        edit.push_annotations(te.snapshot);
        // Exactly one undo step restores BOTH the text and the outline weight together.
        edit.undo(None);
        assert_eq!(edit.annotations, snapshot, "one undo restores the full pre-edit scene");
        assert!(!edit.can_undo(), "and it was the ONLY entry");
    }

    #[test]
    fn selection_transitions_keep_the_newest_pick_primary() {
        // DRAGON-341: the selection is an ordered SET whose LAST member is the primary — the
        // one wearing resize handles and the target of single-item operations.
        let (a, b, c) = (AnnotId(1), AnnotId(2), AnnotId(3));
        let mut s = Selection::default();
        assert!(s.is_empty() && s.primary().is_none() && s.ids().is_empty());
        // A plain click REPLACES the selection.
        s.set_one(a);
        assert_eq!(s.ids(), &[a]);
        s.set_one(b);
        assert_eq!(s.ids(), &[b], "a plain click replaces, never adds");
        // Ctrl/Shift-click APPENDS — and the new member becomes primary.
        s.toggle(a);
        assert_eq!(s.ids(), &[b, a]);
        assert_eq!(s.primary(), Some(a), "the newly added id is primary");
        // Toggling an existing member REMOVES it; the previous member takes over as primary.
        s.toggle(a);
        assert_eq!(s.ids(), &[b]);
        assert_eq!(s.primary(), Some(b));
        // An additive band ADDS without duplicating what is already selected.
        s.add_all([b, c, a]);
        assert_eq!(s.ids(), &[b, c, a], "existing members keep their place, new ones append");
        // A non-additive band / Ctrl+A REPLACES, de-duplicated in first-seen order.
        s.set_all([c, c, b]);
        assert_eq!(s.ids(), &[c, b]);
        assert!(s.contains(c) && !s.contains(a));
        s.clear();
        assert!(s.is_empty() && s.primary().is_none());
    }

    /// DRAGON-397's live band PREVIEW must show what release will COMMIT. The preview is
    /// computed in the canvas widget ([`crate::widgets::annotation_canvas::band_preview_ids`]),
    /// the commit here (`set_all` / `add_all`) — two places, so this pins them to the same
    /// answer for both modifier states, including the ordering (which decides the primary) and
    /// the already-selected-item case the ticket calls out.
    #[test]
    fn the_band_preview_matches_what_the_commit_would_select() {
        use crate::widgets::annotation_canvas::band_preview_ids;
        let cases: [(&[u64], &[u64]); 4] =
            [(&[], &[1, 2]), (&[7, 8], &[1, 2]), (&[7, 8], &[8, 1]), (&[7, 8], &[])];
        for (existing, hits) in cases {
            for additive in [false, true] {
                let mut sel = Selection::default();
                sel.set_all(existing.iter().copied().map(AnnotId));
                let ids = hits.iter().copied().map(AnnotId);
                if additive {
                    sel.add_all(ids);
                } else {
                    sel.set_all(ids);
                }
                let committed: Vec<u64> = sel.ids().iter().map(|i| i.0).collect();
                assert_eq!(
                    committed,
                    band_preview_ids(existing, hits, additive),
                    "existing={existing:?} hits={hits:?} additive={additive}"
                );
            }
        }
    }

    #[test]
    fn selection_prunes_ids_that_left_the_scene() {
        use crate::app::preview::annotate::{AnnotKind, AnnotRect, AnnotationItem};
        let rect = AnnotRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        let item = |id: u64| AnnotationItem {
            id: AnnotId(id),
            color: [255, 0, 0, 255],
            kind: AnnotKind::Box { rect, stroke_w: 4.0, fill: None },
        };
        let mut s = Selection::default();
        s.set_all([AnnotId(1), AnnotId(2), AnnotId(3)]);
        // Item 2 was deleted (a discarded draw / an erase sweep): it drops out, order kept.
        s.retain_existing(&[item(1), item(3)]);
        assert_eq!(s.ids(), &[AnnotId(1), AnnotId(3)]);
        // Everything gone → an empty selection, never a dangling id.
        s.retain_existing(&[]);
        assert!(s.is_empty());
    }

    #[test]
    fn leaving_pointer_mode_drops_pen_groups_from_the_selection() {
        // DRAGON-341 follow-up: pen selection exists ONLY under the pointer, so arming any other
        // tool prunes pen ids out of the SET (not merely hides their chrome) — a hidden-but-
        // selected stroke would still be swept up by a group move or a Delete.
        use crate::app::preview::annotate::{AnnotKind, AnnotPoint, AnnotRect, AnnotationItem};
        let rect = AnnotRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        let boxed = |id: u64| AnnotationItem {
            id: AnnotId(id),
            color: [255, 0, 0, 255],
            kind: AnnotKind::Box { rect, stroke_w: 4.0, fill: None },
        };
        let pen = |id: u64| AnnotationItem {
            id: AnnotId(id),
            color: [255, 0, 0, 255],
            kind: AnnotKind::Pen {
                paths: vec![vec![AnnotPoint { x: 1.0, y: 1.0 }, AnnotPoint { x: 5.0, y: 5.0 }]],
                pressure: Vec::new(),
                stroke_w: 4.0,
            },
        };
        let mut e = EditState {
            annotations: vec![boxed(1), pen(2), boxed(3), pen(4)],
            ..Default::default()
        };
        // A mixed multi-selection whose PRIMARY is a pen (the last pick).
        e.sel.set_all([AnnotId(1), AnnotId(3), AnnotId(2), AnnotId(4)]);
        assert_eq!(e.selected(), Some(AnnotId(4)), "the pen is primary before the switch");
        e.drop_pen_selection();
        assert_eq!(e.sel.ids(), &[AnnotId(1), AnnotId(3)], "only the shapes survive, in order");
        assert_eq!(e.selected(), Some(AnnotId(3)), "primary falls back to the last survivor");
        // Idempotent, and a pen-ONLY selection empties completely (no ghost primary).
        e.drop_pen_selection();
        assert_eq!(e.sel.ids(), &[AnnotId(1), AnnotId(3)]);
        e.sel.set_all([AnnotId(2), AnnotId(4)]);
        e.drop_pen_selection();
        assert!(e.sel.is_empty() && e.selected().is_none());
        // A shape-only selection is untouched.
        e.sel.set_all([AnnotId(1), AnnotId(3)]);
        e.drop_pen_selection();
        assert_eq!(e.sel.ids(), &[AnnotId(1), AnnotId(3)]);
    }

    #[test]
    fn edit_state_exposes_the_primary_as_the_selected_item() {
        let mut e = EditState::default();
        assert_eq!(e.selected(), None);
        e.sel.set_one(AnnotId(7));
        assert_eq!(e.selected(), Some(AnnotId(7)));
        e.sel.toggle(AnnotId(9));
        assert_eq!(e.selected(), Some(AnnotId(9)), "single-item ops follow the primary");
        assert_eq!(e.sel.len(), 2);
    }

    #[test]
    fn cut_filtergraph_trims_and_concats_both_streams() {
        let keep = [Span { start: 0.0, end: 2.5 }, Span { start: 5.0, end: 10.0 }];
        let g = cut_filtergraph(&keep, true, false);
        assert_eq!(
            g,
            "[0:v]trim=start=0.000:end=2.500,setpts=PTS-STARTPTS[v0];\
             [0:v]trim=start=5.000:end=10.000,setpts=PTS-STARTPTS[v1];\
             [v0][v1]concat=n=2:v=1:a=0[v];\
             [0:a]atrim=start=0.000:end=2.500,asetpts=PTS-STARTPTS[a0];\
             [0:a]atrim=start=5.000:end=10.000,asetpts=PTS-STARTPTS[a1];\
             [a0][a1]concat=n=2:v=0:a=1[a]"
        );
    }

    #[test]
    fn cut_filtergraph_overlays_the_covermark_after_the_join() {
        let keep = [Span { start: 1.0, end: 3.0 }];
        let g = cut_filtergraph(&keep, false, true);
        assert_eq!(
            g,
            "[0:v]trim=start=1.000:end=3.000,setpts=PTS-STARTPTS[v0];\
             [v0]concat=n=1:v=1:a=0[vc];\
             [vc][1:v]overlay=(W-w)/2:(H-h)/2[v]"
        );
        assert!(!g.contains("[a]"), "no audio chain for a silent recording");
    }

    // ── The video bake's ffmpeg plan (DRAGON-398) ────────────────────────────────────

    /// **THE uncut invariant, pinned.** CLAUDE.md: "An UNCUT timeline must keep its
    /// historical ffmpeg invocations byte-identical (stream-copy / overlay-only paths)."
    /// A recording with no edits must never start re-encoding because the save workflow
    /// changed, so the exact argument lists are asserted here rather than inferred.
    ///
    /// Note what is ABSENT from both uncut shapes: no `-filter_complex` at all in the
    /// stream-copy case, and no `trim`/`concat` in the covermark case.
    #[test]
    fn an_uncut_timeline_keeps_its_historical_ffmpeg_invocation() {
        // 1. No cut, no covermark: EVERY stream copied. Nothing is re-encoded, so a plain
        //    Save As / Copy of an untouched recording costs one file copy's worth of ffmpeg.
        assert_eq!(
            video_bake_args(None, true, false, "mp4"),
            ["-map", "0", "-c", "copy", "-movflags", "+faststart"]
        );
        // The audio flag is irrelevant to a stream copy (`-map 0` takes whatever is there).
        assert_eq!(
            video_bake_args(None, false, false, "mp4"),
            video_bake_args(None, true, false, "mp4")
        );
        // 2. A covermark WITHOUT a cut: the historical overlay-only graph — video re-encoded,
        //    audio STREAM-COPIED (`-c:a copy`), and no trim/concat anywhere.
        let overlay_only = video_bake_args(None, true, true, "mp4");
        assert_eq!(
            overlay_only,
            [
                "-filter_complex",
                "[0:v][1:v]overlay=(W-w)/2:(H-h)/2[v]",
                "-map",
                "[v]",
                "-map",
                "0:a?",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "18",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "copy",
                "-movflags",
                "+faststart",
            ]
        );
        assert!(!overlay_only.iter().any(|a| a.contains("trim") || a.contains("concat")));
        // 3. A razor-only timeline reaches here as `keep: None` (the caller filters on
        //    `Timeline::edited`), and an EMPTY span list is treated the same way — neither
        //    may produce a `concat=n=0` or drag the file through a re-encode.
        for keep in [None, Some(&[][..])] {
            assert_eq!(
                video_bake_args(keep, true, false, "mp4"),
                ["-map", "0", "-c", "copy", "-movflags", "+faststart"],
                "an uncut timeline must stream-copy"
            );
        }
    }

    /// A CUT timeline is the one shape that re-encodes both streams — arbitrary trim points
    /// cannot stream-copy. The graph itself is `cut_filtergraph`'s (tested above); this pins
    /// the maps and codecs around it, and that the audio maps/encoder appear only when there
    /// IS a soundtrack (mapping `[a]` on a silent recording would fail the whole export).
    #[test]
    fn a_cut_timeline_re_encodes_both_streams() {
        let keep = [Span { start: 0.0, end: 1.0 }, Span { start: 2.0, end: 3.0 }];
        let with_audio = video_bake_args(Some(&keep), true, false, "mp4");
        assert_eq!(with_audio[0], "-filter_complex");
        assert_eq!(with_audio[1], cut_filtergraph(&keep, true, false));
        assert_eq!(
            &with_audio[2..],
            [
                "-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-preset", "veryfast", "-crf",
                "18", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "192k", "-movflags",
                "+faststart",
            ]
        );
        // Silent recording: no `[a]` map and no audio encoder.
        let silent = video_bake_args(Some(&keep), false, false, "mp4");
        assert!(!silent.iter().any(|a| a == "[a]" || a == "aac" || a == "-b:a"));
        assert!(silent.iter().any(|a| a == "libx264"), "video still re-encodes");
        // A cut WITH a covermark folds the overlay into the same graph (one ffmpeg run, not
        // two), so the argument shape is the cut one.
        let both = video_bake_args(Some(&keep), true, true, "mp4");
        assert_eq!(both[1], cut_filtergraph(&keep, true, true));
        assert_eq!(&both[2..], &with_audio[2..]);
    }

    /// `+faststart` is an MP4-family container flag, so it rides exactly the extensions that
    /// understand it — every bake shape, and no others (ffmpeg errors on an unknown movflag).
    #[test]
    fn faststart_follows_the_container_not_the_edit() {
        let keep = [Span { start: 0.0, end: 1.0 }];
        for shape in [None, Some(&keep[..])] {
            for overlay in [true, false] {
                for ext in ["mp4", "m4v", "mov"] {
                    let a = video_bake_args(shape, true, overlay, ext);
                    assert_eq!(&a[a.len() - 2..], ["-movflags", "+faststart"], "{ext}");
                }
                for ext in ["mkv", "webm", "avi", "MP4"] {
                    let a = video_bake_args(shape, true, overlay, ext);
                    assert!(!a.iter().any(|x| x == "-movflags"), "{ext} takes no movflags");
                }
            }
        }
    }

    // ── The save point (DRAGON-353 follow-up) ─────────────────────────────────────────

    /// A NEVER-saved document falls back to "is there anything in the scene", which is the
    /// behaviour that predates the save point.
    #[test]
    fn an_unsaved_document_is_dirty_exactly_when_the_scene_has_content() {
        assert!(!unsaved_at(None, 0, false));
        assert!(unsaved_at(None, 3, true));
        // The DEPTH is irrelevant while there is no save point: a user who drew and then
        // undid everything back to empty has nothing to lose.
        assert!(!unsaved_at(None, 7, false));
    }

    /// THE cycle the owner asked about: save → undo → redo → save. The history SURVIVES
    /// the save, so "clean" has to mean "standing where we saved", not "we saved once".
    #[test]
    fn dirty_tracks_the_save_point_across_undo_and_redo() {
        // Saved at depth 2 with a scene on screen.
        assert!(!unsaved_at(Some(2), 2, true), "standing on the save point is clean");
        // Undo past it: the file holds MORE than the scene does — dirty again.
        assert!(unsaved_at(Some(2), 1, true));
        assert!(unsaved_at(Some(2), 0, false), "even undone to an empty scene");
        // Redo back onto it: clean again, no re-save needed.
        assert!(!unsaved_at(Some(2), 2, true));
        // Redo/edit PAST it: the scene holds more than the file — dirty.
        assert!(unsaved_at(Some(2), 3, true));
    }

    /// The real state machine on an `EditState`: edit → save → edit → undo → redo, with
    /// the history intact throughout (a save must never clear it).
    #[test]
    fn a_save_keeps_the_history_and_marks_the_position() {
        let mut e = EditState::default();
        e.push_dim(0.0);
        e.dim = 0.5;
        assert_eq!(e.undo_stack.len(), 1);
        assert!(e.saved_depth.is_none());

        e.mark_saved();
        assert_eq!(e.saved_depth, Some(1));
        assert!(e.can_undo(), "the save must NOT clear the history");
        assert!(!unsaved_at(e.saved_depth, e.undo_stack.len(), e.dirty()));

        // Undo past the save: the history still works, and the document is dirty again.
        e.undo(None);
        assert_eq!(e.undo_stack.len(), 0);
        assert!(e.can_redo());
        assert!(unsaved_at(e.saved_depth, e.undo_stack.len(), e.dirty()));

        // Redo back onto the save point: clean again.
        e.redo(None);
        assert_eq!(e.undo_stack.len(), 1);
        assert!(!unsaved_at(e.saved_depth, e.undo_stack.len(), e.dirty()));
    }

    /// BRANCH INVALIDATION: undo past the save point, then make a NEW edit. The stack
    /// returns to the same DEPTH, but the state the file holds is no longer reachable by
    /// any amount of redo — so the marker is dropped and the document stays dirty until it
    /// is saved again. Without this the depth would silently claim "clean" for a scene the
    /// file has never seen.
    #[test]
    fn a_new_edit_on_an_abandoned_branch_forgets_the_save_point() {
        let mut e = EditState::default();
        e.push_dim(0.0);
        e.dim = 0.5;
        e.mark_saved();
        assert_eq!(e.saved_depth, Some(1));

        e.undo(None); // back to depth 0, redo available
        e.push_annotations(Vec::new()); // a NEW edit: redo is abandoned...
        assert!(!e.can_redo());
        assert_eq!(e.undo_stack.len(), 1, "same depth as the save point");
        assert_eq!(e.saved_depth, None, "...and the save point went with it");
        assert!(unsaved_at(e.saved_depth, e.undo_stack.len(), true));
    }

    // ── The dirty-close dialog's failure state machine (DRAGON-353 follow-up) ────────

    /// action → FAILURE → retry → success. A failed dialog action re-raises the card with
    /// the reason and DISARMS the close; the retry arms it again from a clean slate; the
    /// success clears everything. At no point does the failure leave a flag set that would
    /// silently swallow or auto-close a later action.
    #[test]
    fn a_failed_dialog_action_reraises_the_dialog_and_a_retry_starts_clean() {
        let mut e = EditState::default();

        // The dialog's button: card down, close armed.
        e.begin_close_action();
        assert!(e.close_after_share && !e.confirm_close && e.close_error.is_none());

        // ...the action fails.
        assert!(e.note_action_failure("Disk full"));
        assert!(e.confirm_close, "the card comes back");
        assert_eq!(e.close_error.as_deref(), Some("Disk full"));
        assert!(!e.close_after_share, "the close is DISARMED — we are not leaving");
        assert!(!e.close_after_bake);

        // A SECOND failure report with nothing armed changes nothing (a toolbar action's
        // failure never raises this dialog, and a duplicate completion can't either).
        let mut stale = EditState::default();
        assert!(!stale.note_action_failure("ignored"));
        assert!(!stale.confirm_close && stale.close_error.is_none());

        // Retry: the stale reason goes and the close is armed afresh — nothing wedged.
        e.begin_close_action();
        assert!(e.close_after_share && !e.confirm_close && e.close_error.is_none());
    }

    /// "Exit anyway" and "Continue editing" both clear the card and its notice; they differ
    /// only in what the CALLER does next (close vs. stay), which is why they share one
    /// dismissal and one discard route.
    #[test]
    fn dismissing_the_dialog_clears_the_failure_either_way() {
        for _ in 0..2 {
            let mut e = EditState::default();
            e.begin_close_action();
            e.note_action_failure("Permission denied");
            e.dismiss_close_dialog();
            assert!(!e.confirm_close && e.close_error.is_none() && !e.close_after_share);
        }
    }

    /// A failure NEVER touches the work: the scene, the history and the save point are
    /// exactly as they were, so "Continue editing" really does continue.
    #[test]
    fn a_failure_leaves_the_document_untouched() {
        let mut e = EditState::default();
        e.push_dim(0.0);
        e.dim = 0.4;
        e.mark_saved();
        e.push_dim(0.4);
        e.dim = 0.9;
        let (depth, saved, dim) = (e.undo_stack.len(), e.saved_depth, e.dim);

        e.begin_close_action();
        e.note_action_failure("Nope");
        assert_eq!(e.undo_stack.len(), depth, "history intact");
        assert_eq!(e.saved_depth, saved, "save point intact");
        assert_eq!(e.dim, dim, "scene intact");
        assert!(unsaved_at(e.saved_depth, e.undo_stack.len(), e.dirty()), "still dirty");
    }

    /// An edit made while standing ON or AFTER the save point does NOT invalidate it — the
    /// save is still reachable by undoing back to it.
    #[test]
    fn an_edit_forward_of_the_save_point_keeps_it() {
        let mut e = EditState::default();
        e.push_dim(0.0);
        e.mark_saved();
        e.push_dim(0.5); // depth 2, the save point at 1 is still behind us
        assert_eq!(e.saved_depth, Some(1));
        assert!(unsaved_at(e.saved_depth, e.undo_stack.len(), true));
        e.undo(None);
        assert!(!unsaved_at(e.saved_depth, e.undo_stack.len(), true), "back at the save");
    }

    // ── DRAGON-389: the over-crop bake carries annotations onto the black extension ───────────

    const CURVE: f32 = super::super::annotate::DEFAULT_ANNOT_CURVE_RADIUS;

    fn filled_box(id: u64, x: f32, y: f32, w: f32, h: f32) -> AnnotationItem {
        AnnotationItem {
            id: AnnotId(id),
            color: [255, 0, 0, 255],
            kind: super::super::annotate::AnnotKind::Box {
                rect: super::super::annotate::AnnotRect { x, y, w, h },
                stroke_w: 3.0,
                fill: Some([255, 0, 0, 255]),
            },
        }
    }

    fn is_red(p: &::image::Rgba<u8>) -> bool {
        p.0[0] > 180 && p.0[1] < 80 && p.0[2] < 80
    }

    /// A unique temp PNG path for this process + tag, removed on drop.
    struct TmpPng(std::path::PathBuf);
    impl Drop for TmpPng {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn tmp_png(tag: &str) -> TmpPng {
        TmpPng(std::env::temp_dir().join(format!("cck-dragon389-{}-{tag}.png", std::process::id())))
    }

    // ── DRAGON-445: the bake keeps the --inspect provenance ──────────────────

    /// The capture comment a real grab embeds, in the exact shape
    /// `App::screenshot_metadata` writes.
    const PROVENANCE: &str =
        "Cosmic Capture Kit | type=photo | source=cosmic | mode=region | cursor=off";

    /// THE regression: an edited/baked PNG keeps the `Comment` chunk `--inspect` reads.
    ///
    /// `bake_image` wrote through `image::save_with_format`, which emits no ancillary
    /// chunks, so every save that went through the editor silently dropped the provenance a
    /// plain capture keeps — and an edited file is exactly the one someone is later asked
    /// where it came from.
    #[test]
    fn dragon445_a_baked_png_keeps_its_capture_provenance() {
        let src = tmp_png("prov-src");
        let dst = tmp_png("prov-dst");
        // Write the source the way a real capture does: pixels PLUS the comment.
        assert!(crate::media::png::save_png(&flat(40, 40, 128), &src.0, PROVENANCE));
        assert_eq!(crate::media::png::read_png_metadata(&src.0).as_deref(), Some(PROVENANCE));

        // Any real edit takes the bake path; a filled box is the cheapest.
        let items = vec![filled_box(1, 4.0, 4.0, 12.0, 12.0)];
        bake_image(&src.0, &dst.0, None, &items, CURVE, 0.0, None).unwrap();

        assert_eq!(
            crate::media::png::read_png_metadata(&dst.0).as_deref(),
            Some(PROVENANCE),
            "the baked PNG must carry the source capture's Comment chunk"
        );
        // And it is still a real edited image, not a copy of the source.
        let out = ::image::open(&dst.0).unwrap().into_rgba8();
        assert_eq!(out.dimensions(), (40, 40));
        assert!(is_red(out.get_pixel(9, 9)), "the edit is present in the baked pixels");
    }

    /// An IN-PLACE save (src == dst, the ordinary "Save" button) keeps it too — the read of
    /// the source comment has to happen before the file is rewritten.
    #[test]
    fn dragon445_an_in_place_bake_keeps_its_provenance() {
        let f = tmp_png("prov-inplace");
        assert!(crate::media::png::save_png(&flat(40, 40, 128), &f.0, PROVENANCE));
        let items = vec![filled_box(1, 4.0, 4.0, 12.0, 12.0)];
        bake_image(&f.0, &f.0, None, &items, CURVE, 0.0, None).unwrap();
        assert_eq!(crate::media::png::read_png_metadata(&f.0).as_deref(), Some(PROVENANCE));
    }

    /// A source with NO comment (an imported/external image opened with `--preview`) must
    /// not gain an invented one. `save_png` writes no chunk for an empty string, which is
    /// the honest answer: we do not know where that file came from.
    #[test]
    fn dragon445_a_source_without_provenance_gains_none() {
        let src = tmp_png("prov-none-src");
        let dst = tmp_png("prov-none-dst");
        // Written the plain way — no Comment chunk, like any third-party PNG.
        flat(40, 40, 128).save_with_format(&src.0, ::image::ImageFormat::Png).unwrap();
        assert_eq!(crate::media::png::read_png_metadata(&src.0), None);

        let items = vec![filled_box(1, 4.0, 4.0, 12.0, 12.0)];
        bake_image(&src.0, &dst.0, None, &items, CURVE, 0.0, None).unwrap();
        assert_eq!(
            crate::media::png::read_png_metadata(&dst.0),
            None,
            "a bake must not invent provenance the source never had"
        );
    }

    #[test]
    fn dragon389_bake_carries_annotation_onto_the_extension() {
        let src = tmp_png("survive-src");
        let dst = tmp_png("survive-dst");
        flat(40, 40, 128).save_with_format(&src.0, ::image::ImageFormat::Png).unwrap();
        // A crop extending 20px LEFT of the source → a 60×40 output; output x in [0,20) is the black
        // extension, [20,60) the source.
        let crop = Some(CropRect { x: -20.0, y: 0.0, w: 60.0, h: 40.0 });
        // A filled box wholly inside the extension (source x -16..-4, all < 0).
        let items = vec![filled_box(1, -16.0, 10.0, 12.0, 12.0)];
        bake_image(&src.0, &dst.0, None, &items, CURVE, 0.0, crop).unwrap();
        let out = ::image::open(&dst.0).unwrap().into_rgba8();
        assert_eq!(out.dimensions(), (60, 40), "output is the crop's pixel size");
        // The box lands at output x 4..16 (source -16 shifted by -(-20) = +20); its interior is red.
        assert!(is_red(out.get_pixel(9, 15)), "the extension annotation survives the bake: {:?}", out.get_pixel(9, 15));
    }

    #[test]
    fn dragon389_bake_renders_straddling_annotation_continuously() {
        let src = tmp_png("straddle-src");
        let dst = tmp_png("straddle-dst");
        flat(40, 40, 128).save_with_format(&src.0, ::image::ImageFormat::Png).unwrap();
        let crop = Some(CropRect { x: -20.0, y: 0.0, w: 60.0, h: 40.0 });
        // A box spanning source x -10..10 — straddling the source's left edge (source x=0 = output
        // x=20). On the output it covers x 10..30.
        let items = vec![filled_box(1, -10.0, 8.0, 20.0, 20.0)];
        bake_image(&src.0, &dst.0, None, &items, CURVE, 0.0, crop).unwrap();
        let out = ::image::open(&dst.0).unwrap().into_rgba8();
        // Red on BOTH sides of the boundary at output x=20 — extension side (x=13) and source side
        // (x=27) — so the shape renders continuously across the source/extension seam.
        assert!(is_red(out.get_pixel(13, 16)), "red on the extension side: {:?}", out.get_pixel(13, 16));
        assert!(is_red(out.get_pixel(27, 16)), "red on the source side: {:?}", out.get_pixel(27, 16));
    }

    // ── DRAGON-391: the over-crop bake patterns the COVERMARK onto the black extension ────────

    /// A deterministic two-colour covermark: the left half of a square viewBox is RED, the right
    /// half BLUE — no text, so it rasterizes identically on every box (no font dependency), and the
    /// colour boundary makes the pattern's placement and scale directly observable.
    struct TmpSvg(std::path::PathBuf);
    impl Drop for TmpSvg {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn half_and_half_mark(tag: &str) -> (TmpSvg, Covermark) {
        let path =
            std::env::temp_dir().join(format!("cck-dragon391-{}-{tag}.svg", std::process::id()));
        std::fs::write(
            &path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
<rect x="0" y="0" width="50" height="100" fill="#ff0000"/>
<rect x="50" y="0" width="50" height="100" fill="#0000ff"/>
</svg>"##,
        )
        .unwrap();
        let cm = Covermark { kind: CovermarkKind::File(path.clone()), zoom: 0.0, opacity: 1.0 };
        (TmpSvg(path), cm)
    }

    fn is_blue(p: &::image::Rgba<u8>) -> bool {
        p.0[2] > 180 && p.0[0] < 80 && p.0[1] < 80
    }

    #[test]
    fn dragon391_bake_covers_the_whole_cropped_image_extension_included() {
        let src = tmp_png("cm-extend-src");
        let dst = tmp_png("cm-extend-dst");
        let (_svg, cm) = half_and_half_mark("extend");
        flat(40, 40, 128).save_with_format(&src.0, ::image::ImageFormat::Png).unwrap();
        // A crop extending 20 px LEFT of the source → a 60×40 image; output x in [0,20) is the
        // black extension, [20,60) the source. The image IS that 60×40 rect, so the square mark
        // cover-fits IT (scale 0.6): red half over output x 0..30, blue over 30..60.
        let crop = Some(CropRect { x: -20.0, y: 0.0, w: 60.0, h: 40.0 });
        bake_image(&src.0, &dst.0, Some(&cm), &[], CURVE, 0.0, crop).unwrap();
        let out = ::image::open(&dst.0).unwrap().into_rgba8();
        assert_eq!(out.dimensions(), (60, 40), "output is the crop's pixel size");
        // Ink on BOTH sides of the source's left edge (output x = 20) — the mark covers the black
        // extension continuously with the source, instead of stopping at the old image edge.
        assert!(is_red(out.get_pixel(10, 20)), "the extension side: {:?}", out.get_pixel(10, 20));
        assert!(is_red(out.get_pixel(25, 20)), "the source side: {:?}", out.get_pixel(25, 20));
        assert!(is_blue(out.get_pixel(45, 20)), "the mark's blue half: {:?}", out.get_pixel(45, 20));
        // EVERY pixel of the image carries the mark — no bare corner anywhere in the extension.
        for (x, y) in [(0u32, 0u32), (0, 39), (59, 0), (59, 39), (19, 20), (20, 20)] {
            let p = out.get_pixel(x, y);
            assert!(is_red(p) || is_blue(p), "bare pixel at ({x},{y}): {p:?}");
        }
    }

    /// The canvas is the CROP RECT — not the source, and not the source ∪ crop union. The union is
    /// strictly larger whenever the crop does not contain the source, and fitting the mark to it
    /// would pattern past the image the user cropped to.
    #[test]
    fn dragon391_bake_patterns_over_the_crop_not_the_union() {
        let src = tmp_png("cm-canvas-src");
        let dst = tmp_png("cm-canvas-dst");
        let (_svg, cm) = half_and_half_mark("canvas");
        flat(40, 40, 128).save_with_format(&src.0, ::image::ImageFormat::Png).unwrap();
        // A 40×40 crop 20 px LEFT of the source covers source x -20..20; the union (-20..40) would
        // be 60 wide. Fitted to the CROP (scale 0.4) the mark's red/blue boundary lands at output
        // x 20; fitted to the UNION (scale 0.6) it would land at x 30 and x 25 would still be red.
        let crop = Some(CropRect { x: -20.0, y: 0.0, w: 40.0, h: 40.0 });
        bake_image(&src.0, &dst.0, Some(&cm), &[], CURVE, 0.0, crop).unwrap();
        let out = ::image::open(&dst.0).unwrap().into_rgba8();
        assert_eq!(out.dimensions(), (40, 40));
        assert!(is_red(out.get_pixel(10, 20)), "extension side: {:?}", out.get_pixel(10, 20));
        assert!(
            is_blue(out.get_pixel(25, 20)),
            "the mark must be fitted to the CROP, not the source ∪ crop union: {:?}",
            out.get_pixel(25, 20),
        );
    }

    /// The same rule with no over-crop at all: after an inner crop the image is that crop, so the
    /// mark re-fits to it. This DELIBERATELY replaces the pre-DRAGON-391 behaviour (a fragment of a
    /// source-fitted mark, whatever happened to fall inside the crop) — "the image is the crop" is
    /// one rule with no special case, and it is what makes live and bake agree.
    #[test]
    fn dragon391_inner_crop_bake_refits_the_mark_to_the_crop() {
        let src = tmp_png("cm-inner-src");
        let dst = tmp_png("cm-inner-dst");
        let (_svg, cm) = half_and_half_mark("inner");
        flat(40, 40, 128).save_with_format(&src.0, ::image::ImageFormat::Png).unwrap();
        // A 20×24 crop inside the source: the mark cover-fits THAT, so its red/blue boundary sits
        // at the output's own midpoint (x = 10), wherever in the source the crop was taken.
        let rect = CropRect { x: 8.0, y: 6.0, w: 20.0, h: 24.0 };
        bake_image(&src.0, &dst.0, Some(&cm), &[], CURVE, 0.0, Some(rect)).unwrap();
        let out = ::image::open(&dst.0).unwrap().into_rgba8();
        assert_eq!(out.dimensions(), (20, 24));
        assert!(is_red(out.get_pixel(5, 12)), "left half: {:?}", out.get_pixel(5, 12));
        assert!(is_blue(out.get_pixel(15, 12)), "right half: {:?}", out.get_pixel(15, 12));
        // The equivalent uncropped bake is untouched: the mark fits the 40×40 source, boundary at
        // x = 20 — so nothing about the un-cropped path moved.
        let plain = tmp_png("cm-inner-plain");
        bake_image(&src.0, &plain.0, Some(&cm), &[], CURVE, 0.0, None).unwrap();
        let full = ::image::open(&plain.0).unwrap().into_rgba8();
        assert!(is_red(full.get_pixel(10, 20)) && is_blue(full.get_pixel(30, 20)));
    }

    #[test]
    fn dragon389_uncropped_bake_matches_apply_annotations() {
        let src = tmp_png("uncrop-src");
        let dst = tmp_png("uncrop-dst");
        flat(40, 40, 128).save_with_format(&src.0, ::image::ImageFormat::Png).unwrap();
        let items = vec![filled_box(1, 8.0, 8.0, 16.0, 12.0)];
        // The historical path: load the source, composite the annotations directly onto it.
        let mut expected = flat(40, 40, 128);
        super::super::annotate::apply_annotations(&mut expected, &items, CURVE);
        bake_image(&src.0, &dst.0, None, &items, CURVE, 0.0, None).unwrap();
        let out = ::image::open(&dst.0).unwrap().into_rgba8();
        assert_eq!(out.dimensions(), (40, 40));
        assert_eq!(out.as_raw(), expected.as_raw(), "an uncropped bake stays the historical apply_annotations, byte-for-byte");
    }

    // ── DRAGON-455: a still is written as PNG, whatever the destination is called ─────

    /// A temp path with an ARBITRARY extension, removed on drop — the point of these tests
    /// is that the name does not decide the contents, so they need names that are not `.png`.
    fn tmp_named(tag: &str, ext: &str) -> TmpPng {
        TmpPng(
            std::env::temp_dir()
                .join(format!("cck-dragon455-{}-{tag}.{ext}", std::process::id())),
        )
    }

    /// What a file ACTUALLY is, read from its first bytes. Every assertion below goes
    /// through this: trusting the extension is precisely the mistake that produced the bug.
    fn sniff(path: &std::path::Path) -> &'static str {
        let bytes = std::fs::read(path).expect("written file");
        match bytes.as_slice() {
            [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => "png",
            [0xFF, 0xD8, 0xFF, ..] => "jpeg",
            _ => "unknown",
        }
    }

    /// THE RULE: an EDITED still bakes to PNG no matter what the destination is called. This
    /// used to be the transcode branch — `shot.jpg` really became a JPEG — while the
    /// unedited half of the same action copied PNG bytes under the same name.
    #[test]
    fn dragon455_an_edited_still_is_png_whatever_the_destination_is_called() {
        let src = tmp_png("rule-src");
        assert!(crate::media::png::save_png(&flat(40, 40, 128), &src.0, PROVENANCE));
        let items = vec![filled_box(1, 4.0, 4.0, 12.0, 12.0)];
        for ext in ["png", "jpg", "JPG", "webp", "xyz"] {
            let dst = tmp_named("rule-dst", ext);
            bake_image(&src.0, &dst.0, None, &items, CURVE, 0.0, None).unwrap();
            assert_eq!(sniff(&dst.0), "png", "a .{ext} destination must still receive PNG bytes");
            // And it is a real edited image, decodable as the PNG it claims to be.
            let out = ::image::load(
                std::io::BufReader::new(std::fs::File::open(&dst.0).unwrap()),
                ::image::ImageFormat::Png,
            )
            .unwrap()
            .into_rgba8();
            assert_eq!(out.dimensions(), (40, 40));
            assert!(is_red(out.get_pixel(9, 9)), "the edit is present in the .{ext} file");
            // DRAGON-445 rides along on every one of them now, not just the `.png` arm.
            assert_eq!(
                crate::media::png::read_png_metadata(&dst.0).as_deref(),
                Some(PROVENANCE),
                "provenance must survive to a .{ext} destination too"
            );
        }
    }

    /// The UNEDITED half, which never reached `bake_image` at all: a PNG source is delivered
    /// BYTE FOR BYTE. That is the deliberate answer, not a shortcut — identical pixels, and
    /// the `--inspect` chunk carried through for free (DRAGON-445).
    #[test]
    fn dragon455_an_unedited_png_is_delivered_byte_for_byte() {
        let src = tmp_png("copy-src");
        let dst = tmp_png("copy-dst");
        assert!(crate::media::png::save_png(&flat(40, 40, 128), &src.0, PROVENANCE));
        save_unedited_still(&src.0, &dst.0).unwrap();
        assert_eq!(sniff(&dst.0), "png");
        assert_eq!(
            std::fs::read(&dst.0).unwrap(),
            std::fs::read(&src.0).unwrap(),
            "a PNG to PNG save must not re-encode"
        );
        assert_eq!(crate::media::png::read_png_metadata(&dst.0).as_deref(), Some(PROVENANCE));
        // Saving onto ITSELF is a no-op, not a truncation.
        save_unedited_still(&src.0, &src.0).unwrap();
        assert_eq!(crate::media::png::read_png_metadata(&src.0).as_deref(), Some(PROVENANCE));
    }

    /// The other direction, and the one that would have re-created the bug: the editor opens
    /// external jpg/webp/… files, so an UNEDITED save of one must be RE-WRITTEN as a PNG.
    /// Copying its bytes to a `.png` name is the same lie, just from the source side.
    #[test]
    fn dragon455_an_unedited_non_png_source_is_rewritten_as_a_real_png() {
        let src = tmp_named("jpeg-src", "jpg");
        let dst = tmp_png("jpeg-dst");
        // A real JPEG on disk, sniffed to prove the fixture itself is honest.
        ::image::DynamicImage::ImageRgba8(flat(40, 40, 128))
            .to_rgb8()
            .save_with_format(&src.0, ::image::ImageFormat::Jpeg)
            .unwrap();
        assert_eq!(sniff(&src.0), "jpeg", "the fixture must really be a JPEG");

        save_unedited_still(&src.0, &dst.0).unwrap();
        assert_eq!(sniff(&dst.0), "png", "a non-PNG source must be re-written, never copied");
        let out = ::image::load(
            std::io::BufReader::new(std::fs::File::open(&dst.0).unwrap()),
            ::image::ImageFormat::Png,
        )
        .unwrap()
        .into_rgba8();
        assert_eq!(out.dimensions(), (40, 40));
        // No provenance is invented for a file that never carried any.
        assert_eq!(crate::media::png::read_png_metadata(&dst.0), None);
    }

    /// The decision is read from the SOURCE'S BYTES, never from its name — a mislabeled file
    /// is exactly what this bug used to produce, so it must not be able to steer the write.
    #[test]
    fn dragon455_the_copy_shortcut_is_decided_by_magic_bytes() {
        // PNG bytes under a `.jpg` name: still a byte copy, because it IS a PNG.
        let liar = tmp_named("liar-src", "jpg");
        let dst = tmp_png("liar-dst");
        assert!(crate::media::png::save_png(&flat(8, 8, 200), &liar.0, PROVENANCE));
        assert!(is_png_file(&liar.0), "the sniff reads the bytes, not the .jpg name");
        save_unedited_still(&liar.0, &dst.0).unwrap();
        assert_eq!(std::fs::read(&dst.0).unwrap(), std::fs::read(&liar.0).unwrap());

        // The mirror image — JPEG bytes under a `.png` name — takes the re-write arm rather
        // than the copy arm, so the lie is never propagated. It then FAILS there, loudly and
        // with nothing written, because `image`'s decoder is chosen by the extension: the
        // same way `bake_image` and the preview's own open path read a file, so a source
        // like this could never have been displayed to edit in the first place.
        let other = tmp_named("liar2-src", "png");
        let dst2 = tmp_png("liar2-dst");
        ::image::DynamicImage::ImageRgba8(flat(8, 8, 200))
            .to_rgb8()
            .save_with_format(&other.0, ::image::ImageFormat::Jpeg)
            .unwrap();
        assert!(!is_png_file(&other.0));
        assert!(save_unedited_still(&other.0, &dst2.0).is_err(), "a mislabeled source must not be copied");
        assert!(!dst2.0.exists(), "a refused save writes nothing");
    }
}

#[cfg(test)]
mod bake_need_tests {
    use super::{BakeNeed, EditState, bake_need, clipboard_is_current};

    /// The depth a marker records, for readability at the call sites below.
    fn baked_at(e: &EditState) -> Option<usize> {
        e.baked.as_ref().map(|(d, _)| *d)
    }

    /// THE re-bake gate (DRAGON-467 review, major 4). A clean document needs nothing; a dirty
    /// one standing on its last bake serves that artifact; anything else renders.
    #[test]
    fn a_share_standing_on_its_last_bake_reuses_the_artifact() {
        // Clean: the file as it stands IS the answer, whatever was baked before.
        for baked in [None, Some(0), Some(3)] {
            assert_eq!(bake_need(false, 3, baked), BakeNeed::None, "{baked:?}");
        }
        // Dirty, and the artifact was rendered from exactly this state.
        assert_eq!(bake_need(true, 3, Some(3)), BakeNeed::Reuse);
        // Dirty, and it was not — either nothing has been baked, or the scene moved since.
        assert_eq!(bake_need(true, 3, None), BakeNeed::Fresh);
        assert_eq!(bake_need(true, 3, Some(2)), BakeNeed::Fresh, "an edit since the bake");
        assert_eq!(bake_need(true, 2, Some(3)), BakeNeed::Fresh, "an undo since the bake");
    }

    /// THE double-copy gate: the clipboard already holding this exact state means the exit
    /// copy has nothing to add.
    #[test]
    fn the_clipboard_is_current_only_at_the_depth_it_was_written_from() {
        assert!(clipboard_is_current(Some(2), 2));
        assert!(!clipboard_is_current(Some(2), 3), "an edit since the copy");
        assert!(!clipboard_is_current(Some(3), 2), "an undo since the copy");
        assert!(!clipboard_is_current(None, 0), "nothing copied yet is never current");
    }

    /// THE LIFE CYCLE the two gates exist for, walked end to end on a real `EditState`.
    ///
    /// Open (auto-copy at depth 0) -> annotate -> Save -> Escape. The exit copy must REUSE
    /// the save's own artifact rather than running a second full render, which for a
    /// recording is a second ffmpeg pass over the whole take.
    #[test]
    fn save_then_exit_reuses_the_saves_artifact_instead_of_re_baking() {
        let mut e = EditState::default();
        // The editor's open-time automatic copy, of the untouched capture.
        e.mark_copied();
        assert!(clipboard_is_current(e.copied_depth, e.undo_stack.len()));
        // The user annotates. The clipboard is now stale, and nothing is baked.
        e.push_dim(0.0);
        e.dim = 0.5;
        let depth = e.undo_stack.len();
        assert_eq!(depth, 1, "one edit, one history entry");
        assert!(!clipboard_is_current(e.copied_depth, depth), "the copy is stale now");
        assert_eq!(bake_need(true, depth, baked_at(&e)), BakeNeed::Fresh);
        // Save: the bake writes the destination and the document adopts it.
        e.mark_baked(std::path::Path::new("/home/me/Capture/shot.png"));
        e.mark_saved();
        // Escape, with copy-on-exit ON. The scene is still dirty (that is what `dirty()`
        // means), but the artifact on disk IS this state, so no second render.
        assert_eq!(
            bake_need(true, e.undo_stack.len(), baked_at(&e)),
            BakeNeed::Reuse,
            "the exit copy must serve the file the save just wrote"
        );
        // And the copy DOES happen, because the clipboard still holds the untouched capture.
        assert!(!clipboard_is_current(e.copied_depth, e.undo_stack.len()));
        // Once it lands, a second Escape would add nothing.
        e.mark_copied();
        assert!(clipboard_is_current(e.copied_depth, e.undo_stack.len()));
    }

    /// An explicit toolbar Copy followed by Escape with no edits between must not copy twice.
    #[test]
    fn an_explicit_copy_then_exit_does_not_copy_twice() {
        let mut e = EditState::default();
        e.push_dim(0.0);
        e.dim = 0.5;
        e.mark_baked(std::path::Path::new("/run/user/1000/cck-copy.png"));
        e.mark_copied();
        assert!(
            clipboard_is_current(e.copied_depth, e.undo_stack.len()),
            "the exit copy has nothing to add"
        );
        // One more edit and both gates re-arm.
        e.push_dim(0.5);
        e.dim = 0.7;
        let depth = e.undo_stack.len();
        assert!(!clipboard_is_current(e.copied_depth, depth));
        assert_eq!(bake_need(true, depth, baked_at(&e)), BakeNeed::Fresh);
    }

    /// An artifact (or a clipboard write) stranded on an ABANDONED redo branch is dropped, so
    /// it can never be served for a state no amount of redo can reach. Same rule, same place
    /// and same reason as `saved_depth` (see `push_op`).
    #[test]
    fn an_abandoned_branch_drops_both_markers() {
        let mut e = EditState::default();
        e.push_dim(0.0);
        e.dim = 0.3;
        e.push_dim(0.3);
        e.dim = 0.6;
        e.mark_baked(std::path::Path::new("/tmp/a.png"));
        e.mark_copied();
        assert_eq!(baked_at(&e), Some(2));
        assert_eq!(e.copied_depth, Some(2));
        // Undo one, then edit down a DIFFERENT branch: the old depth 2 is unreachable.
        e.undo(None);
        e.push_annotations(Vec::new());
        assert_eq!(e.undo_stack.len(), 2, "back at depth 2, but a different depth 2");
        assert!(e.baked.is_none(), "the stale artifact must not be reusable");
        assert!(e.copied_depth.is_none(), "and the clipboard is not current either");
    }

    /// The video COMMIT (DRAGON-467 review, blocker 1) leaves a genuinely clean document:
    /// nothing in the scene, no history, and neither marker able to serve stale bytes.
    #[test]
    fn a_commit_reset_leaves_nothing_behind() {
        let mut e = EditState::default();
        e.push_dim(0.0);
        e.dim = 0.5;
        e.mark_baked(std::path::Path::new("/rec/take.mp4"));
        e.mark_copied();

        e.reset_after_commit();
        assert_eq!(e.undo_stack.len(), 0);
        assert!(!e.can_undo() && !e.can_redo(), "the history cannot outlive the file it described");
        assert!(!e.dirty(), "the file IS the edit now");
        assert!(e.baked.is_none() && e.copied_depth.is_none());
        assert_eq!(e.dim, 0.0);
        assert!(e.covermark.is_none() && e.annotations.is_empty() && e.crop.is_none());
        // Standing on the save point, so an immediate Escape asks nothing.
        assert!(!super::unsaved_at(e.saved_depth, e.undo_stack.len(), e.dirty()));
    }
}

#[cfg(test)]
mod bake_prep_tests {
    use super::{BakePrep, bake_prep};

    /// THE pristine-source invariant (DRAGON-467 review, blocker 1): a bake whose destination
    /// IS its source has to be prepared for, and the two media kinds are prepared differently
    /// because copying a multi-GB take is not an option.
    #[test]
    fn saving_over_the_source_is_prepared_for_per_media_kind() {
        // A different destination needs nothing at all, for either kind.
        for is_video in [true, false] {
            assert_eq!(bake_prep(false, is_video), BakePrep::Direct, "is_video={is_video}");
        }
        // Saving in place: a still snapshots aside, a recording commits and resets.
        assert_eq!(bake_prep(true, false), BakePrep::SnapshotStill);
        assert_eq!(bake_prep(true, true), BakePrep::CommitVideo);
    }

    /// The forbidden outcome, stated as a property: NO combination answers `Direct` when the
    /// destination is the source. A `Direct` there is a bake reading its own output, which is
    /// doubled annotations for a still and a re-applied cut for a recording.
    #[test]
    fn no_in_place_bake_is_ever_left_unprepared() {
        for is_video in [true, false] {
            assert_ne!(
                bake_prep(true, is_video),
                BakePrep::Direct,
                "an in-place bake (is_video={is_video}) must never read its own output"
            );
        }
    }
}
