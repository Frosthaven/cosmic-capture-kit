//! `PreviewMsg` — actions of the post-capture preview overlay.

use crate::app::AnnotId;
use crate::app::PixelFrame;
use crate::widgets::annotation_canvas::{Grab, Tool};
use crate::widgets::crop_canvas::CropHandle;
use std::path::PathBuf;
use std::sync::Arc;

/// Video facts probed from a recording (ffprobe), needed for playback + the scrubber.
#[derive(Debug, Clone, Copy)]
pub struct VideoMeta {
    /// Duration in seconds.
    pub duration: f32,
    /// Frames per second.
    pub fps: f32,
    /// Source pixel width.
    pub w: u32,
    /// Source pixel height.
    pub h: u32,
    /// Whether the file has an audio stream to play.
    pub has_audio: bool,
}

#[derive(Debug, Clone)]
pub enum PreviewMsg {
    /// Save the document: open the destination picker, pre-filled with the path a plain
    /// overwrite-save would have written (DRAGON-467 — Save IS Save As now, so there is one
    /// message and one button where there were two).
    Save,
    /// Save As result: the chosen destination (`None` = cancelled, stay open). A non-null
    /// pick kicks off the background export (bake/move to the destination).
    SaveAsResult(Option<PathBuf>),
    /// The background Save As export finished — `Some(dest)` on success, `None` on failure.
    /// The worker already wrote the file + posted the reveal notification; this keeps the
    /// app alive until it lands, then closes (auto-close on) or reopens the editor on the
    /// just-saved file (auto-close off).
    SaveAsBaked(Option<PathBuf>),
    /// Copy the capture to the clipboard. This NEVER closes the editor by itself — pending
    /// edits bake to a throwaway temp so the saved file stays clean, and a toast reports the
    /// outcome. The exit path arms a close around it when "Automatically copy changes on
    /// exit" is on (DRAGON-467).
    Copy,
    /// Hand the current file to the system share sheet (DRAGON-467). Only pressable where
    /// `platform::services::share_available()` is true (Windows since DRAGON-474, macOS
    /// since DRAGON-480). See `share::share_sheet`'s module doc for where the other
    /// platforms stand.
    Share,
    /// The share sheet finished presenting (DRAGON-480, macOS only — Windows and Linux share
    /// synchronously and never produce this). `Err` shows a toast; `Ok` is silent, exactly
    /// like the synchronous platforms' success case.
    ShareDone(Result<(), String>),
    /// Upload the capture to a configured account (DRAGON-467). The accounts feature does not
    /// exist yet, so the toolbar button ships DISABLED and never emits this; the variant is
    /// the wiring that account work will land on.
    Upload,
    /// Open the SETTINGS pane from the preview editor's chrome. A fullscreen-overlay
    /// document converts itself to a window first (an exclusive layer surface would sit
    /// over the settings window); the pane itself is a detached `--settings` child, or a
    /// `--focus-settings` poke when one is already open — this multi-document HOST must
    /// never convert ITSELF into the settings pane the way the capture overlay's gear does.
    OpenSettings,
    /// Toggle inline playback of a recording (play / pause).
    Play,
    /// Poll the playback worker for the next decoded frame (while playing).
    PlayerTick,
    /// Seek to an absolute position in seconds (scrubber drag).
    Seek(f32),
    /// Ruler click/drag (SOURCE seconds): scrub the playhead there. Seeking
    /// lives in the measurement ruler; lane clicks select instead.
    TimelineSeek(f32),
    /// Lane click with the pointer tool: update the selection WITHOUT moving
    /// the playhead. `(clicked SOURCE time — None = away from any segment,
    /// ctrl, shift)`: plain replaces (or deselects on None), ctrl toggles,
    /// shift range-selects from the anchor.
    TimelineSelect(Option<f32>, bool, bool),
    /// Pointer box-select over the lanes: `(EDITED span start, end, additive)`
    /// — select the intersecting segments; `additive` (ctrl/shift held) keeps
    /// the current selection.
    TimelineBoxSelect(f32, f32, bool),
    /// Razor click (SOURCE seconds): split the segment at that instant.
    TimelineCut(f32),
    /// Arm (`true`) or disarm the timeline's razor (cut) tool — the transport's
    /// pointer/razor segmented toggle sets it explicitly.
    TimelineRazor(bool),
    /// Delete the selected timeline segment (later segments slide left).
    TimelineDelete,
    /// Right-click on the timeline: open the context menu — the clicked SOURCE
    /// time ("Cut here" splits there) and the widget-local point anchoring it.
    TimelineMenuOpen(f32, f32, f32),
    /// Dismiss the timeline context menu without acting.
    TimelineMenuClose,
    /// The soundtrack's L/R peak buckets finished extracting (the timeline's
    /// audio lanes; `None` audio never sends this).
    WaveformReady(Arc<Vec<(f32, f32)>>),
    /// Step the playhead by N frames (`-1` previous, `+1` next).
    FrameStep(i32),
    /// A single scrubbed/stepped frame finished decoding (`None` = decode failed).
    SeekFrameReady(Option<Arc<PixelFrame>>),
    /// The still-image decode never reported back — its thread died (DRAGON-415). The
    /// capture IS on disk; only the editor is lost, and this used to be indistinguishable
    /// from an ordinary [`PreviewMsg::Cancel`], i.e. from the user closing the window.
    LoadFailed,
    /// Windows (DRAGON-436 round 2) / macOS (DRAGON-415): the failure alert raised by
    /// [`Self::LoadFailed`] has been dismissed (or, on Windows, its bounded wait ran out),
    /// so the close it was holding up may now run. Neither platform may close inline: a
    /// `MessageBox` does not block, so closing right after raising it would take the
    /// process down within milliseconds and the box with it, unread; `NSAlert::runModal`
    /// DOES block, but only safely off the winit thread, so mac's "say so, then close" has
    /// to be sequenced through this message the same way. `cfg(any(windows,
    /// target_os = "macos"))` so Linux stays byte-identical.
    #[cfg(any(windows, target_os = "macos"))]
    LoadFailedAlertDismissed,
    /// Close the preview without deleting the file.
    Cancel,
    /// Flip the preview between the fullscreen overlay and a resizable window, live —
    /// closing the current surface and reopening in the other appearance, preserving
    /// the loaded content + edit state. Also persists the new default.
    ToggleAppearance,
    /// Windowed preview CSD header-bar controls (act on the preview window).
    WindowDrag,
    WindowMaximize,
    /// Only the Linux CSD header draws a minimize button; macOS uses the native
    /// traffic-light miniaturize (DRAGON-146) and Windows the native DWM caption
    /// minimize button (DRAGON-284) instead.
    #[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
    WindowMinimize,
    // DRAGON-467: `Delete` (unlink the capture and close) lived here. The editor no longer
    // deletes anything — with "Automatically save originals" off an unwanted capture never
    // reaches the user's folder in the first place, so closing IS the discard. Its button,
    // its `Ctrl+Shift+X` action and the unlink plumbing went with it.
    /// The decoded image is ready — replace the loading spinner with it. Carries the
    /// raw pixels too, so preview edits recomposite from the untouched original.
    ImageReady(cosmic::widget::image::Handle, Option<Arc<::image::RgbaImage>>),
    /// The open-time automatic clipboard copy (DRAGON-353) finished on its worker thread —
    /// `true` if the platform reported the write as done. Its ONLY job is to post the toast
    /// that used to be posted inline; nothing gates on it (DRAGON-454, which moved the copy
    /// off the UI thread — see [`crate::app::App::auto_copy_preview_on_open`]).
    AutoCopied(bool),
    /// Open/close the covermark picker flyout.
    Covermark,
    /// Apply a specific covermark picker entry (mouse click).
    PickerPick(usize),
    // ── Shared toolbar-flyout keyboard nav (covermark picker + color palette) ─────────
    /// Move the OPEN flyout's keyboard highlight by ±1 (wraps).
    FlyoutNav(i32),
    /// Apply the OPEN flyout's highlighted entry (Enter).
    FlyoutApply,
    /// Close the OPEN flyout without applying (Esc / outside click).
    FlyoutClose,
    /// Zoom the displayed image by a wheel step (+ = in, ctrl+scroll), toward the cursor:
    /// `(step, cursor_dx, cursor_dy)` where the deltas are the cursor's offset from the
    /// viewport centre (screen px).
    Zoom(f32, f32, f32),
    /// macOS (DRAGON-147): drain any pending trackpad pinch magnification and apply
    /// it as a zoom. Fired by `sub_preview_pinch` while a preview is open.
    #[cfg(target_os = "macos")]
    PinchPoll,
    /// Set the view zoom multiplier directly (the zoom-scale slider). 1.0 = fit.
    SetViewZoom(f32),
    /// Pick a zoom preset by index into the zoom-scale dropdown (Fit / 100% / a % level).
    ZoomPreset(usize),
    /// Open/close the zoom preset combo menu.
    ToggleZoomMenu,
    /// Pan the displayed image by a screen-px delta (alt+scroll / alt+drag).
    Pan(f32, f32),
    // DRAGON-392: there is no pan MODE any more. Panning is the HAND TOOL
    // (`annotation_canvas::Tool::Hand`), armed through the ordinary `SelectTool` /
    // `ToolPressed` path like every other tool — so the toolbar, the `H` key and the canvas all
    // read ONE fact (`edit.tool`) instead of a tool plus a parallel mode flag. Do not
    // reintroduce a mode message here.
    /// Set the active covermark's zoom (0 = default cover fit). Live from the slider —
    /// updates the value only; the (blink-free) re-raster happens on `CommitCovermarkEdit`.
    SetZoom(f32),
    /// Set the active covermark's opacity (0..1). Live from the slider — value only.
    SetOpacity(f32),
    /// A covermark slider was released — re-raster the covermark overlay now (debounced so
    /// dragging doesn't churn the GPU texture).
    CommitCovermarkEdit,
    /// Set the global dim amount (0..1) — the dim/spotlight slider (DRAGON-329). Live from the
    /// slider; renders straight from the model via the GPU dim pass (no raster to debounce).
    SetDim(f32),
    /// The dim slider was released — coalesce the whole drag into ONE undo entry.
    CommitDimEdit,
    /// Undo the most recent covermark change.
    Undo,
    /// Redo the most recently undone covermark change.
    Redo,
    /// An async covermark-overlay raster finished (generation, raw RGBA overlay). Stacked
    /// over the base image/video via a persistent-texture shader (no atlas churn → no
    /// blink). Stale generations are dropped. Shared by image + video previews.
    CovermarkRasterReady(u64, Option<Arc<PixelFrame>>),
    /// A bake (export re-encode) finished: the new file size, or `None` on failure.
    BakeDone(Option<u64>),
    // ── Unsaved-changes close guard (DRAGON-353) ─────────────────────────────────────
    /// Dismiss the unsaved-changes dialog and stay in the editor ("Keep editing").
    KeepEditing,
    /// Close the document, abandoning the pending edits ("Close without saving"). The
    /// FILE is untouched — only the un-baked edits are lost.
    DiscardAndClose,
    /// The unsaved-changes dialog's ONE action button: save, THEN close the document. Arms
    /// `EditState::close_after_share` and delegates to the plain toolbar action, so the save
    /// flow itself never learns about closing.
    ///
    /// DRAGON-467 reduced the card to three options (Save / Continue editing / Close without
    /// saving), so its siblings are gone: `SaveAsAndClose` with the separate Save As button,
    /// `CopyAndClose` because a copy neither saves nor discards the edits the card is asking
    /// about, and `DeleteAndClose` with the delete feature itself.
    SaveAndClose,
    /// Sweep this document's expired toasts (the `sub_preview_toasts` tick).
    ToastTick,
    /// A recording's poster frame + probed metadata finished (`None` poster = none could
    /// be made; `None` meta = ffprobe failed, so playback/scrub stay disabled).
    PosterReady(Option<cosmic::widget::image::Handle>, Option<VideoMeta>),

    // ── Annotation editor (DRAGON-321; IMAGES only) ──────────────────────────────────
    /// Switch the active annotation tool (Select / Arrow / Box).
    SelectTool(Tool),
    /// An action-tray tool BUTTON was pressed (DRAGON-339). Arms the tool exactly like
    /// [`Self::SelectTool`], but also feeds the tray's double-click detector: a second press of
    /// the SAME button within the double-click window spawns a ready-made item in the middle of
    /// the picture. Only the tray emits this — hotkeys stay on [`Self::SelectTool`], so
    /// double-TAPPING a tool key never places anything.
    ToolPressed(Tool),
    /// A tool SLOT's cycle key was pressed (DRAGON-369), carried as the slot's own cycle
    /// [`crate::shortcuts::Action`] — the slot's identity everywhere. Arms the slot's current
    /// member, or advances to the next when the slot is already armed (see
    /// `preview::chrome::next_slot_tool`). An action that names no slot is a no-op.
    CycleToolSlot(crate::shortcuts::Action),
    /// Set the current annotation color (applies to NEW shapes).
    SetAnnotColor([u8; 4]),
    /// Set the current annotation stroke width (SOURCE px; applies to NEW box/arrow shapes
    /// and re-strokes the SELECTED box/arrow). Driven by the width toggle group.
    SetAnnotStrokeW(f32),
    /// Cycle the annotation stroke width to the next preset (2 → 5 → 8 → 2), the `L` hotkey.
    CycleAnnotStrokeW,
    /// Open/close the TEXT-size dropdown flyout (DRAGON-354).
    ToggleTextSizeFlyout,
    /// Open/close the text FONT dropdown (DRAGON-357 item 16): Hand / Clean.
    ToggleTextFontFlyout,
    /// Set the text size (SOURCE px) for NEW text boxes, and re-size the SELECTED text box —
    /// picked from the size dropdown.
    SetTextSize(f32),
    /// Switch the text FONT family for NEW text boxes, and re-font the SELECTED text box
    /// (handwritten Excalifont vs clean Inter).
    SetTextFont(crate::app::preview::text_annot::TextFont),
    /// Re-open the in-canvas editor on the existing TEXT annotation `id` (DRAGON-354): a press
    /// on a text box with the Text tool armed, or a double-click on one.
    EditText(AnnotId),
    /// A caret-blink tick while a text box is being edited: toggle the caret's visible phase.
    TextCaretBlink,
    /// A pointer PRESS inside the actively-edited text box (DRAGON-354 item 12): place the caret
    /// at image point `(x, y)`. `extend` (Shift held) keeps the current anchor and extends the
    /// selection; `word` (a double-click) selects the whole word under the point; `all` (a
    /// triple-click) selects the whole box, the same target as Cmd/Ctrl+A.
    TextClickAt { x: f32, y: f32, extend: bool, word: bool, all: bool },
    /// A drag inside the actively-edited text box (DRAGON-354 item 12): extend the selection to
    /// image point `(x, y)` (the caret end moves, the press anchor stays).
    TextDragTo(f32, f32),
    /// The OS input method committed a string while a text box was being edited (DRAGON-359):
    /// insert it at the caret, replacing any selection. This is how the emoji picker
    /// (Ctrl+Cmd+Space / fn-E on macOS, the Windows emoji panel) and CJK composition deliver
    /// their result — the canvas widget publishes `InputMethod::Enabled` so the OS knows a text
    /// field is focused, then routes the `Ime::Commit` here.
    TextImeCommit(String),
    /// Open/close the color-swatch palette popover.
    ToggleAnnotPalette,
    /// Swap the ACTIVE annotation color to its COMPANION (its complement), toggling back on a
    /// second press (DRAGON-386; the `X` hotkey, Photoshop's foreground/background swap). Reuses
    /// the [`Self::SetAnnotColor`] path, so it recolors any selection and persists identically to
    /// picking the companion in the flyout. See [`crate::app::preview::annotate::companion_swap`].
    AnnotColorCompanionSwap,
    /// Open (`true`) / close (`false`) the custom color-wheel picker, from the palette's "+".
    AnnotColorEditor(bool),
    /// A libcosmic color-picker interaction (wheel drag, hue slider, hex/rgb input): routed
    /// straight into the open [`cosmic::widget::ColorPickerModel`].
    AnnotColorPickerUpdate(cosmic::widget::color_picker::ColorPickerUpdate),
    /// Apply the color-wheel picker's color (Apply button): read the model's color, set
    /// `annot_color`, push the MRU, persist, and close.
    AnnotColorApply,
    /// Select (`Some`) or deselect (`None`) an annotation — REPLACES the whole selection.
    SelectAnnotation(Option<AnnotId>),
    /// Ctrl/Shift-click in pointer mode (DRAGON-341): toggle one annotation in the
    /// multi-selection, keeping the rest. A newly added id becomes the primary.
    ToggleAnnotationSelected(AnnotId),
    /// A pointer rubber band finished (DRAGON-341): select every annotation the band
    /// `(x0, y0)`–`(x1, y1)` (image source px) touches. The `bool` is ADDITIVE (Ctrl/Shift):
    /// keep the current selection and add to it.
    BandSelectAnnotations(f32, f32, f32, f32, bool),
    /// Select every annotation in the scene (DRAGON-341 — Ctrl+A), engaging pointer mode so the
    /// selection is immediately usable.
    SelectAllAnnotations,
    /// Begin drawing a new shape of `tool` at image point `(x, y)` (source px).
    AnnotDrawBegin(Tool, f32, f32),
    /// Begin manipulating the selected item (grab kind + press image point).
    AnnotGrabBegin(Grab, f32, f32),
    /// Live drag update to image point `(x, y)`. `scale_type` is the DRAGON-370 modifier
    /// (Ctrl held during a RESIZE = scale the type instead of reflowing the box), sampled fresh
    /// on every motion event — see `crate::widgets::annotation_canvas::AnnotEvent::GestureTo`.
    AnnotGestureTo(f32, f32, bool),
    /// The active canvas gesture committed (pointer released after a real drag).
    AnnotGestureEnd,
    /// Delete the selected annotation(s) — the whole selection, as one undo entry.
    DeleteSelected,
    /// Duplicate the selected annotation, offset toward the frame center. One undo entry.
    DuplicateSelected,
    /// Set the selected annotation's color to the CURRENT annotation color (any kind;
    /// a highlight keeps its translucent fill with the new hue). One undo entry.
    SetSelectedColor,
    /// Raise the selected annotation one step in the z-stack.
    RaiseSelected,
    /// Lower the selected annotation one step.
    LowerSelected,
    /// Bring the selected annotation to the front (top).
    SelectionToFront,
    /// Send the selected annotation to the back (bottom).
    SelectionToBack,
    /// Open the annotation context menu at widget-local `(x, y)`.
    AnnotMenuOpen(f32, f32),
    /// Dismiss the annotation context menu.
    AnnotMenuClose,

    // ── Crop tool (DRAGON-382; IMAGES only) ──────────────────────────────────────────
    /// Enter the crop tool: open a session over the current crop (or the whole frame), zoom
    /// the media out to leave blank margin on all sides. Toggles OFF (accepts) when a session
    /// is already open, so the single toolbar icon both opens and confirms.
    CropEnter,
    /// Accept the live crop: commit the session rect (Enter, or the checkmark button).
    CropAccept,
    /// Cancel the live crop, discarding the session (Escape, or the x button).
    CropCancel,
    /// A crop drag grabbed `handle` at image source point `(x, y)`.
    CropDragBegin(CropHandle, f32, f32),
    /// A crop drag moved to image source point `(x, y)`; the `bool` is the snap-override
    /// modifier (Cmd/Ctrl) held.
    CropDragTo(f32, f32, bool),
    /// A crop drag released.
    CropDragEnd,
}

impl PreviewMsg {
    /// Whether this message means the user has got HANDS-ON with the document — the
    /// signal that shortens any live toast on it to
    /// [`crate::app::preview::toast::TOAST_INTERACTION_TTL`] (DRAGON-353 follow-up).
    ///
    /// The classifier lives beside the enum on purpose: a new variant is classified where
    /// it is declared, not in a match buried in the update loop that nobody remembers to
    /// extend.
    ///
    /// # What qualifies
    ///
    /// Direct manipulation of the picture or its timeline: arming a tool (the select/hand
    /// toggle included — picking a tool IS the start of working), any canvas press, drag or
    /// selection, and every timeline gesture (scrub, razor, select, delete, right-click).
    ///
    /// # What deliberately does NOT
    ///
    /// * **Hover / pointer motion** — reading a toast requires the pointer to be
    ///   somewhere; moving it is not a decision.
    /// * **The action bar and header buttons** (Save, Copy, Delete, appearance, window
    ///   chrome). These are what PRODUCE toasts; letting them shorten would mean an action
    ///   racing its own confirmation. Ordering makes this belt-and-braces — the shortening
    ///   is applied BEFORE the handler runs, so a toast a handler posts always gets the
    ///   full TTL — but they are excluded on merit as well.
    /// * **Wheel zoom / zoom presets / the pinch poll** — a zoom is looking, not editing,
    ///   and a wheel event is easy to send by accident while reading.
    /// * **The covermark / dim / stroke sliders** — toolbar controls, not the canvas.
    /// * **Playback transport** (Play, frame-step) and every async completion tick.
    ///
    /// `Pan` is the one grey area and it QUALIFIES: it is direct manipulation of the
    /// picture. That also catches alt+scroll panning, which the message cannot distinguish
    /// from an alt+drag — panning with the wheel is still moving the picture, so the
    /// inability to tell them apart costs nothing.
    pub fn is_document_interaction(&self) -> bool {
        matches!(
            self,
            // ── Tool arming ──────────────────────────────────────────────────────────
            Self::SelectTool(_)
                | Self::ToolPressed(_)
                | Self::CycleToolSlot(_)
                | Self::TimelineRazor(_)
                // ── Canvas press / drag / selection ──────────────────────────────────
                | Self::AnnotDrawBegin(..)
                | Self::AnnotGrabBegin(..)
                | Self::AnnotGestureTo(..)
                | Self::AnnotGestureEnd
                | Self::SelectAnnotation(_)
                | Self::ToggleAnnotationSelected(_)
                | Self::BandSelectAnnotations(..)
                | Self::AnnotMenuOpen(..)
                | Self::TextClickAt { .. }
                | Self::TextDragTo(..)
                | Self::TextImeCommit(..)
                | Self::Pan(..)
                // ── Video timeline / scrubbing ───────────────────────────────────────
                | Self::Seek(_)
                | Self::TimelineSeek(_)
                | Self::TimelineSelect(..)
                | Self::TimelineBoxSelect(..)
                | Self::TimelineCut(_)
                | Self::TimelineDelete
                | Self::TimelineMenuOpen(..)
                // ── Crop gestures ─────────────────────────────────────────────────────
                | Self::CropEnter
                | Self::CropDragBegin(..)
                | Self::CropDragTo(..)
                | Self::CropDragEnd
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::annotation_canvas::Tool;

    /// The interaction classifier: hands-on gestures shorten a document's toasts, the
    /// controls that PRODUCE toasts (and mere looking) do not.
    #[test]
    fn hands_on_messages_are_interactions_and_toast_producers_are_not() {
        for m in [
            PreviewMsg::SelectTool(Tool::Pointer),
            PreviewMsg::ToolPressed(Tool::Rect),
            PreviewMsg::CycleToolSlot(crate::shortcuts::Action::PreviewAnnotShapeCycle),
            // DRAGON-392: arming the hand IS a tool arming, no special message.
            PreviewMsg::SelectTool(Tool::Hand),
            PreviewMsg::AnnotDrawBegin(Tool::Arrow, 1.0, 2.0),
            PreviewMsg::AnnotGestureEnd,
            PreviewMsg::SelectAnnotation(None),
            PreviewMsg::Pan(3.0, 4.0),
            PreviewMsg::Seek(1.5),
            PreviewMsg::TimelineCut(2.0),
            PreviewMsg::TimelineDelete,
            PreviewMsg::TimelineRazor(true),
        ] {
            assert!(m.is_document_interaction(), "{m:?} must count as interaction");
        }
        for m in [
            // The actions that create toasts must never shorten the toast they create.
            PreviewMsg::Save,
            PreviewMsg::Copy,
            PreviewMsg::Share,
            PreviewMsg::Upload,
            PreviewMsg::SaveAndClose,
            PreviewMsg::KeepEditing,
            PreviewMsg::DiscardAndClose,
            // Looking, not editing.
            PreviewMsg::Zoom(1.0, 0.0, 0.0),
            PreviewMsg::ZoomPreset(1),
            PreviewMsg::SetViewZoom(2.0),
            // Transport + async ticks.
            PreviewMsg::Play,
            PreviewMsg::PlayerTick,
            PreviewMsg::FrameStep(1),
            PreviewMsg::ToastTick,
            // DRAGON-454: the open-time copy's completion POSTS a toast, so it belongs with
            // the producers above — an arrival that shortened its own confirmation would be
            // the exact bug that rule exists to prevent, and this one arrives from a worker
            // thread rather than from anything the user did.
            PreviewMsg::AutoCopied(true),
            // Toolbar sliders.
            PreviewMsg::SetOpacity(0.5),
            PreviewMsg::SetDim(0.3),
        ] {
            assert!(!m.is_document_interaction(), "{m:?} must NOT count as interaction");
        }
    }
}
