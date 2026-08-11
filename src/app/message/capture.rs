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
    /// DRAGON-612: re-ask a HELD accept, from `sub_accept_pending`.
    ///
    /// Its ONLY producer is that re-ask: an accept KEY PRESS never sends a message at all. The
    /// lane in `app::keyboard` calls `App::request_accept` directly, because only the press
    /// knows which surface it was delivered to, and that is a term the accept needs. This
    /// variant exists purely so the held request has something to arrive on, and it resolves
    /// through the same `request_accept`, which is the only thing that decides.
    AcceptPending,
    /// While windows are loading: advance the spinner + poll the pre-capture slot.
    LoadingTick,
    /// DRAGON-336: ~100ms drain of the preview-HANDOFF channel while this process hosts
    /// (`App::handoff_host` bound). Each inbound request is another process's finished
    /// capture: open it as a new preview document and ACK in the same arm, so the child
    /// may exit. Lives in the CAPTURE domain rather than `PreviewMsg` because a handoff
    /// poll has no preview to address — it may be what CREATES the first one, and
    /// `Msg::Preview` now requires a `window::Id`. Never constructed where no transport
    /// exists (neither unix sockets nor Windows named pipes, DRAGON-651 — see
    /// `crate::preview_ipc`), because nothing ever hosts there.
    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    HandoffPoll,
    /// DRAGON-148 option C, then DRAGON-212: drain the DEFERRED frozen-flats grab slot into
    /// `self.frozen` + redraw against the still image. Fired by the drain poll
    /// while `frozen_pending`.
    ///
    /// Constructed on EVERY platform. This said "never constructed on Linux (synchronous
    /// grab)", which was true when only macOS deferred the grab and stopped being true at
    /// DRAGON-212, when Linux (and Windows) started deferring it too: `sub_frozen_ready`
    /// carries no `cfg` at all. The line is corrected rather than deleted because DRAGON-606
    /// now hangs the dim's fade-in off this drain on every platform, so a reader who believes
    /// the old claim concludes the fade can never start on Linux.
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
    /// DRAGON-563: ~80ms drain of the countdown TRAY item's clicks while the countdown
    /// digits live in the tray (every session; on the `lab/flatpak` fallback path the
    /// selection window is closed, so the tray's Cancel countdown entry is the only
    /// cancel surface there). A Cancel click routes to the ordinary Cancelled ending
    /// (`teardown`).
    CountdownTrayPoll,
    RegionChange(GlobalRect),
    RegionDone,
    /// DRAGON-599: MOVE the drawn region one logical pixel, from an arrow key or its vim
    /// letter (`shortcuts::nudge_direction`, gated by `keyboard::region_nudge_fires`).
    ///
    /// A [`crate::shortcuts::Direction`] rather than a `(dx, dy)` pair, because that type is
    /// where the arrows and `hjkl` converge: nothing past key dispatch can tell which key was
    /// pressed, so the two families cannot drift.
    ///
    /// Distinct from [`Self::RegionChange`] on purpose, twice over. It never re-homes the
    /// toolbar (a one-pixel move is not a fresh selection, and yanking a toolbar the user has
    /// dragged would be a surprise), and it never raises `region_dragging`, which HIDES the
    /// toolbar for as long as it is set: a per-keypress flash of it disappearing is exactly
    /// what nobody wants while lining a region up. Persisting is the key RELEASE's job, so a
    /// held arrow writes the config once instead of once per repeat.
    NudgeRegion(crate::shortcuts::Direction),
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
    /// DRAGON-600 (Linux, tray-menu launches): poll the held frozen-flats grab. Carries no
    /// data; the two clocks it consults live on `App::menu_hold`. Fires only while the
    /// hold is up, and the hold ends the first time it releases the grab.
    MenuHoldTick,
    /// DRAGON-606: advance the dim's fade-in by one frame. Carries no data; the start
    /// instant lives on `App::dim_fade`. Fires only while the fade is actually running and
    /// stops for good when it completes, so a finished animation idles no timer.
    DimFadeTick,
    /// Linux: run a DEFERRED overlay-less immediate capture
    /// (`--active-window` / `--active-monitor`). Kicked once, a short settle after the FIRST
    /// output registers, so the REST of the outputs have populated `self.outputs` first —
    /// the cursor-output probe + the preview then resolve on the monitor the cursor is on.
    /// Only constructed on Linux (mac/Windows resolve immediately in `seed_outputs_mac`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    RunImmediate,
    /// DRAGON-479: capture the CURRENTLY drawn region and deliver it WITHOUT the preview
    /// editor — the clipboard + system-notification path every editor-less capture takes
    /// (`finish_share`). Raised by the fixed primary+C chord in the capture overlay
    /// (`shortcuts::is_region_copy_chord`, gated by `keyboard::region_copy_fires`).
    ///
    /// This restores what DRAGON-451 retired as `CopySelection` — same name, same job — minus
    /// the configurable `Action::RegionCopy` / `Context::Region` keymap lane that made two
    /// meanings of Ctrl+C collide. The old variant also called itself a FORCED copy; there is
    /// nothing left to force (DRAGON-353 removed the copy-to-clipboard setting it bypassed),
    /// so this is simply the editor-less delivery, reached one keypress earlier.
    ///
    /// A no-op unless the gate's conditions hold; the message re-checks them, since it could
    /// arrive by some other route than the chord.
    CopySelection,
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
    /// `lab/flatpak` (Linux): the SEED-TIME portal request of the fallback overlay path
    /// finished; its result is in `pw_slot`. Distinct from [`Self::PipewireCastReady`]
    /// because the seed grant does not COMMIT a capture: it freezes the granted monitor
    /// and mints the fallback selection window instead of proceeding to record/save.
    #[cfg(target_os = "linux")]
    FallbackCastReady,
    /// `lab/flatpak` (Linux): the seed-time single-frame grab posted into
    /// `fallback_frame_slot` (or its worker died): build the frozen backdrop and mint
    /// the fallback window, or fail the session out loud.
    #[cfg(target_os = "linux")]
    FallbackFrozenReady,
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
