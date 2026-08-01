//! `CaptureMsg` sub-enum split out of the former flat `Msg` (see app/mod.rs).

use crate::app::{Hover, Kind, Mode};
use crate::platform::compositor::WinRect;
use crate::selection::GlobalRect;

#[derive(Debug, Clone)]
pub enum CaptureMsg {
    SetMode(Mode),
    SetKind(Kind),
    /// DRAGON-322: ~1s poll while a capture overlay is up — re-check whether another
    /// instance is recording (`crate::instance::any_other_recording`) and refresh
    /// `external_recording`, so the video kind disables/re-enables live if a recording
    /// starts or ends elsewhere while this overlay is open.
    ExternalRecordingTick,
    /// Monitor mode: the cursor is over this output's overlay — set it as the single
    /// hovered output (drives the whole-monitor highlight). Whichever overlay the cursor
    /// is in wins, so it reassigns cleanly across monitors (no cursor-left dependence).
    HoverOutput(String),
    /// While windows are loading: advance the spinner + poll the pre-capture slot.
    LoadingTick,
    /// DRAGON-336: ~100ms drain of the preview-HANDOFF channel while this process hosts
    /// (`App::preview_host` bound). Each inbound request is another process's finished
    /// capture: open it as a new preview document and ACK in the same arm, so the child
    /// may exit. Lives in the CAPTURE domain rather than `PreviewMsg` because a handoff
    /// poll has no preview to address — it may be what CREATES the first one, and
    /// `Msg::Preview` now requires a `window::Id`. Never constructed off unix (no
    /// transport, so nothing ever hosts — see `crate::preview_ipc`).
    #[cfg_attr(not(unix), allow(dead_code))]
    HandoffPoll,
    /// macOS (DRAGON-148 option C): drain the DEFERRED frozen-flats grab slot into
    /// `self.frozen` + redraw against the still image. Fired by the drain poll
    /// while `frozen_pending`. Never constructed on Linux (synchronous grab).
    FrozenReady,
    /// macOS (DRAGON-200): drain the DEFERRED per-output picker wallpaper slot into
    /// `self.wallpaper_handles`. Fired by the drain poll while `wallpaper_pending`.
    /// Runs after `FrozenReady` (the wallpaper SCK grabs are strictly after the
    /// launch-critical frozen flats). Never constructed on Linux (inline resolve).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    WallpaperReady,
    /// DRAGON-213: drain the DEDICATED launch cursor grab slot into `frozen_cursor` (+
    /// its display handle). Fired by the drain poll while `cursor_pending`, on BOTH
    /// platforms — the pointer is locked at LAUNCH (its own thread), so the region/
    /// monitor selector's on-overlay indicator and every launch-locked capture path
    /// read the launch instant, never a stale mid-selection position.
    CursorReady,
    /// DRAGON-215: the off-thread window focus-then-grab COMPLETED — open the preview
    /// spinner NOW (its focus steal is safe only after the grab; the DRAGON-194 invariant
    /// stands) at the carried `(width, height)`, while compose/save continue on the
    /// worker. `None` = a spinner was already pre-opened as the defocus focus-sink, so
    /// this is a no-op (no second spinner). Sent on both platforms for a window pick.
    WindowGrabbed(Option<(u32, u32)>),
    /// macOS (DRAGON-151) / Windows (DRAGON-276): while the countdown/recording
    /// overlays are click-through, poll the pointer against each output's toolbar-chip
    /// rect and re-solidify just the hovered overlay (all others stay passthrough).
    /// Fired by `sub_passthrough`. Never constructed on Linux (layer-shell input
    /// zones handle this natively).
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "windows")),
        allow(dead_code)
    )]
    PassthroughPoll,
    /// Pointer entered/left a floating overlay button.
    SetHover(Hover),
    /// Open/close the region capture group's delay menu.
    ToggleDelayMenu,
    /// Pick a delay from the region capture group's menu (and close it).
    PickDelay(usize),
    /// One-second countdown tick.
    Tick,
    /// ✕ on the countdown badge — abort the capture.
    CancelCapture,
    RegionChange(GlobalRect),
    RegionDone,
    Capture { output: String },
    /// Window mode: capture the picked window's pixels directly (by stable id).
    CaptureWindow { id: String, rect: WinRect },
    /// Fires one tick after teardown: grab pixels now that the overlay is gone.
    DoPixelCapture,
    /// DRAGON-456: fires one tick after a scan refresh blanked the overlay — the screen is
    /// clean of our own UI now, so run the re-grab. The twin of `DoPixelCapture` for a
    /// session that CONTINUES (nothing was torn down; the overlay just stopped painting).
    ScanShotTick,
    /// DRAGON-460: advance the refresh button's spin while a scan is in flight. Carries no
    /// data — the angle lives on the App and this only says "another frame".
    ScanSpinTick,
    /// Linux: run a DEFERRED overlay-less immediate capture
    /// (`--active-window` / `--active-monitor`). Kicked once, a short settle after the FIRST
    /// output registers, so the REST of the outputs have populated `self.outputs` first —
    /// the cursor-output probe + the preview then resolve on the monitor the cursor is on.
    /// Only constructed on Linux (mac/Windows resolve immediately in `seed_outputs_mac`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    RunImmediate,
    // DRAGON-451: `CopySelection` lived here — the region quick-action (primary+C in
    // region-draw mode) that captured the drawn selection, force-copied it and skipped the
    // editor. Retired with its shortcut: the global "(no editor)" hotkeys (DRAGON-428) do
    // the same thing without needing the overlay, and the "force" stopped meaning anything
    // when DRAGON-353 removed the copy-to-clipboard setting.
    /// Drag delta (logical px) moving the toolbar on a given output (by name, so each
    /// monitor's toolbar moves independently).
    ToolbarPan(String, f32, f32),
    /// A toolbar drag finished — re-sync the active overlay's input region.
    ToolbarDragEnd,
    /// Result of the startup ScreenCast-portal probe (reachable, advertised source-type bitflags).
    PipewireProbed(bool, u32),
    /// The in-flight portal ScreenCast request finished — its result is in `pw_slot`.
    // Matched (and handled) cross-platform, but only CONSTRUCTED on Linux (the portal
    // cast path); gating it would fragment the shared `update_capture` match, so allow
    // the never-constructed lint on this variant alone.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    PipewireCastReady,
    /// An off-UI-thread screenshot finished (path, outcome) — share or report + exit.
    /// Used by both the PipeWire path and the async window-capture path.
    ///
    /// DRAGON-415: the outcome was a bare `bool`, which collapsed "no image", "could not
    /// write the file" and "the worker panicked" into one indistinguishable exit. It now
    /// names which of the three happened so the failure can be reported honestly;
    /// `Saved` is exactly the old `true`.
    ShotSaved(std::path::PathBuf, crate::app::failure::ShotOutcome),
    /// Clear the transient overlay toast (fired by its expiry timer).
    DismissToast,
}
