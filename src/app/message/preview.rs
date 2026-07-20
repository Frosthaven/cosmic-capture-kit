//! `PreviewMsg` — actions of the post-capture preview overlay.

use crate::app::PixelFrame;
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
    /// Keep the capture where it was auto-saved, then finish.
    Save,
    /// Save As: open a file picker to choose a destination.
    SaveAs,
    /// Save As result: the chosen destination (`None` = cancelled, stay open). A non-null
    /// pick kicks off the background export (bake/move to the destination).
    SaveAsResult(Option<PathBuf>),
    /// The background Save As export finished — `Some(dest)` on success, `None` on failure.
    /// The worker already wrote the file + posted the reveal notification; this keeps the
    /// app alive until it lands, then closes (auto-close on) or reopens the editor on the
    /// just-saved file (auto-close off).
    SaveAsBaked(Option<PathBuf>),
    /// Copy the capture to the clipboard, then finish.
    Copy,
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
    /// Delete the file, then finish.
    Delete,
    /// The decoded image is ready — replace the loading spinner with it. Carries the
    /// raw pixels too, so preview edits recomposite from the untouched original.
    ImageReady(cosmic::widget::image::Handle, Option<Arc<::image::RgbaImage>>),
    /// Open/close the covermark picker.
    Covermark,
    /// Move the covermark picker's keyboard selection by ±1.
    PickerNav(i32),
    /// Apply the picker's keyboard-selected covermark.
    PickerApply,
    /// Apply a specific picker entry (mouse click).
    PickerPick(usize),
    /// Close the picker without applying.
    PickerClose,
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
    /// Switch the preview pointer tool: `false` = normal pointer, `true` = pan (grabby hand).
    SetPanMode(bool),
    /// Set the active covermark's zoom (0 = default cover fit). Live from the slider —
    /// updates the value only; the (blink-free) re-raster happens on `CommitCovermarkEdit`.
    SetZoom(f32),
    /// Set the active covermark's opacity (0..1). Live from the slider — value only.
    SetOpacity(f32),
    /// A covermark slider was released — re-raster the covermark overlay now (debounced so
    /// dragging doesn't churn the GPU texture).
    CommitCovermarkEdit,
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
    /// Confirm overwriting the original `--preview` file with the edited version.
    ConfirmOverwrite,
    /// Dismiss the overwrite confirmation dialog, staying in the preview.
    CancelOverwrite,
    /// A recording's poster frame + probed metadata finished (`None` poster = none could
    /// be made; `None` meta = ffprobe failed, so playback/scrub stay disabled).
    PosterReady(Option<cosmic::widget::image::Handle>, Option<VideoMeta>),
}
