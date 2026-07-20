//! `SettingsMsg` sub-enum split out of the former flat `Msg` (see app/mod.rs).

use crate::app::DirTarget;
use crate::shortcuts::{Action, Shortcut};
use cosmic::widget::color_picker::ColorPickerUpdate;

/// Which window-capture border a colour-picker edit targets (DRAGON-191).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderColorTarget {
    /// The ACTIVE (focused / single-window) border. Its colour is an `Option` (a Reset
    /// clears it back to "follow the system accent").
    Active,
    /// The INACTIVE (unfocused) border. Always a concrete colour.
    Inactive,
}

#[derive(Debug, Clone)]
pub enum SettingsMsg {
    /// Settings: toggle whether captures include the mouse cursor.
    SetCaptureCursor(bool),
    /// Settings: toggle whether window captures keep their transparency.
    SetCaptureTransparency(bool),
    /// Settings: toggle whether region/monitor captures include the wallpaper.
    SetCaptureWallpaper(bool),
    /// Settings (DRAGON-191): single-window capture focus appearance (dropdown index
    /// 0 = Active, 1 = Inactive).
    SetWindowFocusAppearance(usize),
    /// Settings (DRAGON-209): the region selection box thickness (slider, 1-8 px).
    SetSelectionBoxThickness(u32),
    /// Settings (DRAGON-191): the ACTIVE window-capture border width (slider, 0-10 px).
    SetActiveBorderWidth(u32),
    /// Settings (DRAGON-191): the INACTIVE window-capture border width (slider, 0-10 px).
    SetInactiveBorderWidth(u32),
    /// Settings (DRAGON-191): reset the ACTIVE window-capture border to defaults (colour
    /// back to "follow accent" = None, width back to the default).
    ResetActiveBorder,
    /// Settings (DRAGON-191): reset the INACTIVE window-capture border to defaults
    /// (default colour + default width).
    ResetInactiveBorder,
    /// Settings (DRAGON-191): toggle the reconstructed drop shadow on window captures.
    SetWindowDropShadow(bool),
    /// Settings (DRAGON-191): open (`true`) / close (`false`) the border colour-picker
    /// sidebar, carrying which border it edits.
    ToggleBorderColorEditor(BorderColorTarget, bool),
    /// Settings (DRAGON-191): drive the border colour picker (hex/RGB input, hue, save /
    /// reset / cancel). Save/Reset persist + apply the colour and close the panel.
    BorderColorPicker(ColorPickerUpdate),
    // Transparency multiplier parked (linear-light over() makes it redundant):
    // /// Settings: window-capture transparency multiplier (0..1).
    // SetWindowTransparencyMultiplier(f32),
    /// Settings: toggle the transparent margin around window captures.
    SetWindowPadding(bool),
    /// Settings: set the window-capture padding width (px), from its text input.
    SetWindowPaddingPx(String),
    /// Settings: toggle freeze-pixels (takes effect next launch).
    SetFreeze(bool),
    /// Settings: toggle allowing multiple instances (takes effect next launch).
    SetAllowMultiple(bool),
    /// Settings: toggle staying resident (the tray/menu-bar RESIDENT process). Emitted
    /// by the "Keep running in the background" row on macOS (menu-bar daemon), Linux (ksni
    /// tray resident, DRAGON-173), and Windows (Win32 tray daemon, DRAGON-237); a no-op on
    /// any platform without a resident, so the variant is gated to the three that construct it.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    SetResident(bool),
    /// Settings: region selection overlay dim opacity.
    SetRegionOpacity(f32),
    /// Settings: active (countdown/recording) overlay dim + line opacity.
    SetActiveOpacity(f32),
    /// Settings: post-capture preview overlay dim opacity.
    SetPreviewOpacity(f32),
    /// Settings: recording frame rate text field.
    SetRecordFps(String),
    /// Settings: recording target bitrate (Kbps).
    SetRecordBitrate(String),
    /// Settings: recording max-resolution preset index.
    SetRecordResPreset(usize),
    /// Settings: custom recording max width / height.
    SetRecordMaxWidth(String),
    SetRecordMaxHeight(String),
    /// Settings: pick the NVENC preset (index into `NVENC_PRESETS`).
    SetNvencPreset(usize),
    /// Settings: pick the x264 preset (index into `X264_PRESETS`).
    SetX264Preset(usize),
    /// Settings: pick the VAAPI compression level (index into `VAAPI_CL_VALUES`).
    SetVaapiPreset(usize),
    /// Settings: toggle experimental GPU zero-copy capture.
    #[cfg(feature = "zero-copy")]
    SetRecordZeroCopy(bool),
    /// Settings: pick the video codec (index into `CODEC_VALUES`).
    SetRecordCodec(usize),
    /// Settings: pick the preferred encoder (index into `encoders`).
    SetPreferredEncoder(usize),
    /// Windows (DRAGON-238): the off-thread encoder probe finished — store the list.
    /// Kicked when the settings window opens so the video page never blocks on the
    /// `ffmpeg -encoders` + hardware probe-encodes; fills the process-wide cache.
    #[cfg(windows)]
    EncodersProbed(Vec<crate::encode::EncoderInfo>),
    /// Settings: pick which monitor the encoder benchmark tests (index into the
    /// enumerated `bench_monitors`).
    SetBenchMonitor(usize),
    /// Settings: run the encoder benchmark against the selected monitor's true dims.
    RunBenchmark,
    /// Refresh the benchmark progress/results while it runs.
    BenchPoll,
    /// Settings: recording save directory.
    SetRecordDir(String),
    /// Settings: recording capture method (a stable `platform::backend` id).
    SetRecordBackend(String),
    /// Settings: screenshot capture method (a stable `platform::backend` id).
    SetScreenshotBackend(String),
    /// Settings: clear the saved ScreenCast portal permission (restore token).
    ResetScreencastPermission,
    /// Settings: screenshot save directory.
    SetScreenshotDir(String),
    /// Settings: the "Custom text" covermark's text.
    SetCovermarkText(String),
    /// Settings: toggle push-to-talk (mic muted while recording unless the hotkey is held).
    SetPushToTalk(bool),
    /// Settings (DRAGON-174): toggle hiding the floating toolbar on full-screen
    /// captures (when it can't fit outside the recording area).
    SetHideToolbarFullscreen(bool),
    /// Settings: switch the General page's in-page tab (Settings vs Appearance;
    /// DRAGON-138). In the settings domain — it's General-page view state, not
    /// window chrome like the nav-rail `SetConfigTab`.
    SetGeneralTab(cosmic::widget::segmented_button::Entity),
    /// Settings: switch the Capture Modes page's in-page tab (Scanner / Screenshots /
    /// Screen Recordings; DRAGON-140). Same domain rationale as `SetGeneralTab`.
    SetCaptureTab(cosmic::widget::segmented_button::Entity),
    /// Settings: switch the Audio & Video page's in-page tab (Audio / Video;
    /// DRAGON-141). Same domain rationale as `SetGeneralTab`, but it also drives the
    /// live mic sensitivity meter's lifecycle (gated on the Audio tab being active).
    SetAudioVideoTab(cosmic::widget::segmented_button::Entity),
    /// Settings: switch the Keyboard Shortcuts page's in-page tab (Capture /
    /// Recording / Preview; DRAGON-142). Same domain rationale as `SetGeneralTab`.
    SetShortcutsTab(cosmic::widget::segmented_button::Entity),
    /// Settings: preview editor appearance (windowed vs overlay).
    SetPreviewWindowed(bool),
    /// Settings: auto-close the preview editor after a save/copy.
    SetAutoClosePreview(bool),
    /// Settings: COSMIC-only tiling exception so the windowed preview floats.
    SetPreviewFloatCosmic(bool),
    /// Settings: open the folder picker for a save directory, then apply it.
    PickDir(DirTarget),
    DirPicked(DirTarget, Option<std::path::PathBuf>),
    /// Settings: toggle copy-to-clipboard.
    SetCopyToClipboard(bool),
    /// Settings: toggle real-time noise reduction on the captured mic.
    SetNoiseReduction(bool),
    /// Settings: pick the microphone input device (0 = System / automatic).
    SetMicDevice(usize),
    /// Settings: toggle echo cancellation (cancel speaker bleed into the mic).
    SetEchoCancellation(bool),
    /// Settings: pick the speaker output device used as the echo reference
    /// (0 = System / automatic). Only constructed by the Linux Output picker;
    /// macOS has no output section (DRAGON-132).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    SetSpeakerDevice(usize),
    /// Settings: toggle input-sensitivity threshold between automatic and manual.
    SetInputSensitivityAuto(bool),
    /// Settings: set the manual input-sensitivity (voice gate) threshold (0..1).
    SetInputSensitivity(f32),
    /// Settings: toggle Automatic Gain Control.
    SetAutoGain(bool),
    /// Settings: toggle Advanced Voice Activity (neural VAD gating).
    SetAdvancedVad(bool),
    /// Settings: open the live microphone test dialog (starts mic capture).
    OpenMicTest,
    /// Close the microphone test dialog (stops mic capture).
    CloseMicTest,
    /// Repaint tick for the live mic-test waveform (snapshots fresh samples).
    MicTestTick,
    /// Settings: clipboard size-limit field (MB).
    SetClipboardMaxMb(String),
    /// Settings: toggle the post-capture preview window.
    SetPreviewAfterCapture(bool),
    /// Settings: toggle muting other apps' audio while a video preview plays.
    SetMuteOthersDuringPreview(bool),
    /// Settings: toggle ducking the recorded system audio under mic speech.
    SetDuckSystemAudio(bool),
    /// Appearance (DRAGON-139): follow the system theme (ON) vs use the overrides
    /// below (OFF). When turned back ON the live theme reverts to following the
    /// system; the override values are kept but ignored.
    SetUseSystemAppearance(bool),
    /// Appearance override: base mode (0 automatic / 1 dark / 2 light).
    SetAppearanceMode(u8),
    /// Appearance override: accent colour (`Some` = a palette swatch's sRGB, `None`
    /// = keep the base theme's own accent). Applied live.
    SetAppearanceAccent(Option<[f32; 3]>),
    /// Appearance override: corner-rounding style (0 round / 1 slightly / 2 square).
    SetAppearanceRoundness(u8),
    /// Appearance (DRAGON-289): toggle "Automatic Contrast Boost" — adapt the selected
    /// accent for optimal contrast (ON) vs use the exact picked colour everywhere (OFF).
    /// Applied live. Only meaningful while System Default is off.
    SetAppearanceContrastBoost(bool),
    /// Appearance: open (`true`) / close (`false`) the hand-rolled custom-accent
    /// colour-picker sidebar panel.
    ToggleAccentEditor(bool),
    /// Appearance: drive the custom-accent colour picker (hex/RGB input, hue, save /
    /// reset / cancel). Save/Reset persist + apply the accent and close the panel.
    AccentPicker(ColorPickerUpdate),
    /// Keyboard Shortcuts: start capturing a new binding for `action` (the next key
    /// press is consumed as its shortcut). Press again or Esc to cancel.
    BeginRebind(Action),
    /// Keyboard Shortcuts: set `action`'s binding (from a captured key, or a reset).
    SetShortcut(Action, Shortcut),
    /// Keyboard Shortcuts: clear `action`'s binding (the "x" button).
    UnbindShortcut(Action),
    /// Keyboard Shortcuts (macOS/Windows): edit the resident daemon's global "Start Capture"
    /// hotkey spec (e.g. "PrintScreen", "Cmd+Shift+2"). Persisted; when the spec is
    /// valid and the daemon is running, it is restarted so the new key takes effect.
    /// Gated to the two OSes with a daemon-owned global hotkey so Linux's `SettingsMsg`
    /// stays byte-identical (Linux's capture key is a COSMIC custom shortcut, not ours).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    SetCaptureHotkey(String),
    /// Keyboard Shortcuts (macOS/Windows): start (or cancel) RECORDING the "Start Capture"
    /// global hotkey — the next chord the user presses is captured, serialized to a
    /// daemon spec, and applied via `SetCaptureHotkey`. Mirrors `BeginRebind` for the
    /// in-app rows: press the button to begin, press again or Esc to cancel.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BeginCaptureHotkeyRebind,
    /// Keyboard Shortcuts (macOS/Windows): while the "Start Capture" chord recorder is armed,
    /// a ~1s timer sends this so the running daemon SUSPENDS its global hotkey (the key
    /// then reaches this app to be recorded). Fire-and-forget signal; the daemon
    /// auto-resumes after the pings stop.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    SuspendDaemonHotkeyPing,
    /// Health (macOS): open a System Settings > Privacy & Security pane for a TCC
    /// permission (Screen Recording / Microphone) — the honest recovery for a denied
    /// grant (a re-request prompt only fires once per code identity). macOS-only so
    /// Linux's `SettingsMsg` stays byte-identical.
    #[cfg(target_os = "macos")]
    OpenTccPane(crate::platform::mac::tcc::PrivacyPane),
    /// Health (macOS): request Microphone TCC access — fires the one-shot OS prompt
    /// when the status is NotDetermined. macOS-only (see `OpenTccPane`).
    #[cfg(target_os = "macos")]
    RequestMicTcc,
    /// Health (macOS): request Screen Recording TCC access — fires the one-shot OS
    /// prompt and marks it spent (`mac_first_run_seen`), after which the row's
    /// action falls back to `OpenTccPane`. Only offered while the prompt hasn't
    /// been fired this TCC lifetime. macOS-only (see `OpenTccPane`).
    #[cfg(target_os = "macos")]
    RequestScreenTcc,
    /// About (DRAGON-175): start (or restart) a background update check — fired on
    /// the "Check for updates" button and when the settings window opens.
    CheckForUpdates,
    /// About: a background update check finished; carries the resolved status,
    /// cached in `App::update_status` (drives the nav tint + About page rows).
    UpdateChecked(crate::update::UpdateStatus),
    /// About (macOS): start the one-click download + verify + install of the
    /// available update. A no-op unless the status is `Available` with an artifact.
    /// Only constructed by the macOS About page (Linux has no one-click yet);
    /// compiled (and type-checked) everywhere on purpose, like `Msg::Permissions`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    InstallUpdate,
    /// About (macOS): the one-click install finished. `Staged` means the swap
    /// helper is armed and the app must now quit so the swap + relaunch can run;
    /// `Failed` surfaces the reason inline. Only constructed on macOS (see
    /// `InstallUpdate`).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    UpdateInstallDone(crate::update::InstallOutcome),
    /// About (DRAGON-177): toggle "Notify me when an update is available" — the
    /// persisted `notify_updates` setting that gates the launch-time update dialog.
    SetNotifyUpdates(bool),
    /// Land the settings window on the About page (a post-update relaunch, or a
    /// settings child spawned with `CCK_SETTINGS_TAB=about`), so the new
    /// version's "What's new" notes are immediately visible.
    ShowAboutPage,
    /// Launch dialog (DRAGON-177): the "Don't remind me again" checkbox in the
    /// update dialog changed; carries its new state (applied to `notify_updates`
    /// when a dialog button is clicked).
    UpdateDialogRemindToggled(bool),
    /// Launch dialog (DRAGON-177): "Update Now" pressed — apply the checkbox to
    /// `notify_updates`, dismiss the dialog, and run the platform update flow (the
    /// macOS one-click install / the Linux release-page link).
    UpdateDialogNow,
    /// Launch dialog (DRAGON-177): "Update Later" pressed — apply the checkbox to
    /// `notify_updates` and dismiss the dialog for this session (no update action).
    UpdateDialogLater,
}
