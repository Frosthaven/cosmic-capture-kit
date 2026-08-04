//! `SettingsMsg` sub-enum split out of the former flat `Msg` (see app/mod.rs).

use crate::app::DirTarget;
use crate::shortcuts::{Action, Shortcut};
use cosmic::widget::color_picker::ColorPickerUpdate;

/// macOS/Windows (DRAGON-295): which of the three resident-daemon global capture hotkeys
/// a settings row edits. Each is an independent spec string persisted separately (see the
/// `capture_*_hotkey` fields); the daemon registers each that is set. Gated to the two OSes
/// with a daemon-owned global hotkey so Linux's `SettingsMsg` stays byte-identical.
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureHotkeySlot {
    /// "Capture All In One" — opens the full region/window/monitor picker overlay
    /// (the former single "Start Capture" hotkey).
    AllInOne,
    /// "Capture Active Window" — immediately captures the frontmost window, no picker.
    ActiveWindow,
    /// "Capture Active Monitor" — immediately captures the monitor under the cursor, no picker.
    ActiveMonitor,
    /// DRAGON-428: "Capture All In One (no editor)" — the same picker overlay, but the
    /// capture is saved, copied and notified without the preview editor opening.
    AllInOneNoEditor,
    /// DRAGON-428: "Capture Active Window (no editor)".
    ActiveWindowNoEditor,
    /// DRAGON-428: "Capture Active Monitor (no editor)".
    ActiveMonitorNoEditor,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl CaptureHotkeySlot {
    /// Every slot, in the order the settings page lists them AND the order both daemons
    /// register them (`load_specs` on Windows, the `Spawn` slot list on macOS). One order,
    /// three consumers: change it here and the rows, the registration and the DRAGON-452
    /// duplicate check move together.
    pub const ALL: [Self; 6] = [
        Self::AllInOne,
        Self::ActiveWindow,
        Self::ActiveMonitor,
        Self::AllInOneNoEditor,
        Self::ActiveWindowNoEditor,
        Self::ActiveMonitorNoEditor,
    ];

    /// This slot's position in [`Self::ALL`] — the index the pure
    /// [`crate::shortcuts::capture_hotkey_conflict`] check speaks in.
    pub fn index(self) -> usize {
        match self {
            Self::AllInOne => 0,
            Self::ActiveWindow => 1,
            Self::ActiveMonitor => 2,
            Self::AllInOneNoEditor => 3,
            Self::ActiveWindowNoEditor => 4,
            Self::ActiveMonitorNoEditor => 5,
        }
    }

    /// The row label, and the name used when telling the user which shortcut lost its
    /// binding (DRAGON-452). Matches the daemons' slot names word for word.
    pub fn label(self) -> &'static str {
        match self {
            Self::AllInOne => "Capture All In One",
            Self::ActiveWindow => "Capture Active Window",
            Self::ActiveMonitor => "Capture Active Monitor",
            Self::AllInOneNoEditor => "Capture All In One (no editor)",
            Self::ActiveWindowNoEditor => "Capture Active Window (no editor)",
            Self::ActiveMonitorNoEditor => "Capture Active Monitor (no editor)",
        }
    }
}

/// The Cloud Accounts settings page's own messages (DRAGON-482).
///
/// A sub-enum under [`SettingsMsg::Cloud`] rather than eight more flat variants: the page
/// owns a small state machine (an add dialog, a provider choice, a disconnect
/// confirmation) that nothing else in settings shares, and keeping it in one type is what
/// lets stage B2 add to it without editing the outer enum every time.
///
/// Stage A1 declared the vocabulary and handled every variant as a documented no-op; the
/// settings-page stage filled the bodies in and added the variants the live flow needs
/// (the provider list's open state, the connect outcome, the folder step, and the two
/// refresh messages). A device-code fallback flow lived here too for a while (DRAGON-482)
/// but was removed (DRAGON-489 follow-up): Google's needed a client type this app doesn't
/// register, and the QR-code/link browser flow already covers "sign in from another device"
/// better than a typed code.
///
/// # The generation number
///
/// Five variants carry a `u64` generation: [`Self::Connected`], [`Self::BrowserUrl`],
/// [`Self::FoldersLoaded`], and DRAGON-506's [`Self::FolderCreated`] and
/// [`Self::FolderDeleted`]. A connect runs on a background thread that CANNOT be aborted
/// (the loopback listener owns its own deadline), so "Cancel" cannot stop it, only stop
/// caring about it. The page bumps its generation whenever the user cancels or closes the
/// dialog, and a reply stamped with an older one is dropped. Without it, a cancelled
/// sign-in finished in the browser would silently add an account minutes later.
///
/// The folder-manager replies carry it for the same reason and for one more: every navigation
/// between levels bumps the generation too, so a listing (or a create, or a delete) started at
/// one level can never land on the list of another.
#[derive(Debug, Clone)]
pub enum CloudSettingsMsg {
    /// Open the "add an account" dialog.
    AddOpen,
    /// Close it without connecting.
    AddClose,
    /// Choose which provider to connect, by index into `cloud::registry()`. An index, not
    /// an id, because it comes straight from a dropdown; the handler resolves it against
    /// the registry and ignores one that is out of range.
    AddProviderSelected(usize),
    /// Open (`true`) or close (`false`) the provider list in the add dialog.
    ///
    /// The list is a popover the page builds itself rather than a `widget::dropdown`,
    /// because two of the five providers are listed but NOT selectable, which a dropdown
    /// cannot express. So its open state is ours to hold.
    AddProviderMenu(bool),
    /// Start the OAuth flow for the chosen provider, in the browser. A no-op for a
    /// provider whose `AuthKind` is `Unofficial`, which the page also says on the row.
    Connect,
    /// The interactive (browser) flow now knows its sign-in page's own address: the
    /// generation, and the URL (DRAGON-489). Reported moments after the Browser step begins.
    /// The page does NOT open it automatically (DRAGON-489 follow-up: an automatic launch can
    /// fail silently, no default browser set, the wrong browser opens, a headless/remote
    /// session, with nothing else on screen to fall back to); instead it shows a clickable
    /// accent link plus a scannable QR code, and the user picks whichever suits them,
    /// including finishing on their phone.
    BrowserUrl(u64, String),
    /// Copy the browser sign-in URL to the clipboard.
    CopyBrowserUrl(String),
    /// Open the browser sign-in URL: the browser step's own click-through, since the page is
    /// no longer opened automatically (DRAGON-489 follow-up).
    ///
    /// This needs no host check: the URL is built entirely by our own code from the hardcoded
    /// per-provider `auth_url` constant (see `cloud::AuthKind::OAuthPkce`), so it is already
    /// trusted before it ever reaches the UI, unlike provider-supplied network input.
    OpenBrowserUrl(String),
    /// Open the cloud accounts setup guide in the user's browser (DRAGON-508): the add
    /// dialog's empty-state link, shown when this build has no provider configured at all.
    ///
    /// Carries NO payload on purpose. The URL is the compile-time
    /// `pages::cloud::CLOUD_GUIDE_URL` constant, read by the handler, so there is no way for a
    /// URL from anywhere else to reach the OS opener along this path.
    OpenGuide,
    /// A connect attempt finished: the generation, and either the account it saved (tokens
    /// already stored through `cloud::secrets`) or a message saying why not.
    Connected(u64, Result<crate::cloud::accounts::CloudAccount, String>),
    /// The folder setup for the account being configured finished: the generation, and either
    /// the account's app folder plus what is inside it (`cloud::providers::FolderSetup`) or a
    /// message.
    ///
    /// Carried the folder list alone until DRAGON-498. It carries the ROOT with it now because
    /// finding (or creating) that folder is part of the same round trip, and because on Google
    /// Drive the root is a real folder id the account has to remember: a listing without it
    /// would leave "the app folder" unaddressable.
    FoldersLoaded(u64, Result<crate::cloud::providers::FolderSetup, String>),
    /// The folder browser OPENED, walked back down to the account's saved destination
    /// (DRAGON-517): the generation, and either the view to show (the app folder, the levels
    /// below it the walk resolved, and what is inside the deepest one) or a message.
    ///
    /// Separate from [`Self::FoldersLoaded`], which carries ONE level, because opening the
    /// browser can take several: an account remembers its destination as a path of display names,
    /// and the ids the breadcrumb needs for each level of it are not on disk. The walk is one
    /// detached job (`cloud::providers::restore_browse`) so the step spins its refresh glyph once
    /// and lands once, however deep the saved folder is.
    ///
    /// `Err` means not one level came back, which is the same "the folder list could not be read"
    /// path a failed listing takes. A walk that got partway carries its reason INSIDE the view
    /// instead, because there is still something to show.
    FolderViewRestored(u64, Result<crate::cloud::providers::RestoredBrowse, String>),
    /// Re-run the folder listing for the account the setup step is configuring (DRAGON-503).
    ///
    /// The refresh button that replaces the "Reading your folders" line once a listing has
    /// landed: a user who just made a subfolder at the provider needs a way to see it without
    /// closing the step and re-opening it. It stamps a fresh generation like every other
    /// connect-adjacent flow, so a slower earlier listing cannot land on top of this one.
    FolderRefresh,
    // ── Tombstone: `FolderPicked(Option<usize>)` (DRAGON-517) ────────────────────────────────
    //
    // "Put my captures in this folder": an index into the listing, or `None` for the level the
    // browser was inside. It was the OTHER press the folder step had, alongside the one that
    // navigated, and it is gone with the distinction. Where the browser IS, is where the captures
    // go, so [`Self::FolderCrumbIn`] and [`Self::FolderCrumbTo`] are the whole of choosing now,
    // and [`Self::SetupDone`] is what writes the answer.
    //
    // A `FolderPathInput` (a typed destination path, for the one provider that addresses folders
    // by path) had already been deleted from beside it in DRAGON-498, when every provider became
    // confined to one app folder.
    /// Go INTO one of the listed folders, by index (DRAGON-506), which since DRAGON-517 is also
    /// how a destination is chosen: the level the browser is showing is where this account's
    /// captures land, so descending into a folder selects it.
    ///
    /// Nothing is written here. The account's record is updated once, by [`Self::SetupDone`], so
    /// browsing costs no file writes and Cancel really does leave the account as it was found.
    FolderCrumbIn(usize),
    /// Go back OUT to a level already in the breadcrumb, by crumb index (DRAGON-506): 0 is the
    /// app folder, 1 the first folder descended into, and so on. The elision a deep trail draws
    /// sends nothing, since it names no single level to go to.
    ///
    /// The mirror of [`Self::FolderCrumbIn`] under DRAGON-517: coming back out moves the
    /// destination out with the view.
    FolderCrumbTo(usize),
    /// Open the inline "new folder" row at the level being viewed (DRAGON-506).
    FolderCreateOpen,
    /// Close it without creating anything.
    FolderCreateCancel,
    /// The new folder's name field changed. Buffered raw, exactly as
    /// [`Self::AccountLabelInput`] is, so a user clearing it sees an empty field; the name is
    /// validated when they submit.
    FolderCreateInput(String),
    /// Create the folder now (the tick beside the field, or Enter in it).
    FolderCreateSubmit,
    /// A create finished: the generation, and either the folder the provider made or a message
    /// saying why not.
    FolderCreated(u64, Result<crate::cloud::providers::RemoteFolder, String>),
    /// Ask to delete one of the listed folders, by index (DRAGON-506). Opens a confirmation,
    /// because a folder takes everything inside it when it goes.
    FolderDelete(usize),
    /// The user backed out of the delete.
    FolderDeleteCancel,
    /// The user confirmed it. This is the one that calls the provider.
    FolderDeleteConfirm,
    /// A delete finished: the generation, and either success or a message. Either way the level
    /// is re-listed, so what is on screen is what the provider actually has.
    FolderDeleted(u64, Result<(), String>),
    /// The account name field on the setup step changed (DRAGON-489 follow-up). Buffered
    /// straight onto the in-progress account and applied at [`Self::SetupDone`], which since
    /// DRAGON-498 is the only thing that step still has left to commit.
    ///
    /// Buffered RAW, empty string included, so a user clearing the field to retype it sees an
    /// empty field. An empty name becomes the provider default on the way to disk instead
    /// (DRAGON-495; see the page's `commit_label`).
    AccountLabelInput(String),
    /// Finish the setup step: write the account's name, and (where the provider has folders) the
    /// level the browser is showing, onto the account and close.
    ///
    /// **The destination is written HERE and nowhere else** (DRAGON-517). It used to be written
    /// the instant a folder was picked, because picking was a separate act from browsing; with the
    /// browser's level as the destination, every crumb press would otherwise be a file write.
    ///
    /// Named `FolderDone` until DRAGON-495, when the step stopped being about folders alone
    /// (it is now shown for every provider, and carries the account's name).
    SetupDone,
    /// Re-read the accounts file into the page (and re-probe the stored sign-ins). Sent
    /// when the settings window opens and after anything that changes the list.
    Reload,
    /// The off-thread sign-in probe finished: the ids of the accounts whose stored tokens
    /// warrant a Reconnect. Off-thread because reading a token means reading the platform
    /// keyring, which is a D-Bus round trip on Linux.
    HealthLoaded(Vec<String>),
    /// Connect an already-listed account again, by id, because its authorization is gone
    /// (see `cloud::oauth::RECONNECT_PREFIX`). Opens the same dialog as adding one, with
    /// the provider fixed.
    Reconnect(String),
    /// Re-open the setup step for an already-connected account, by id (the gear on its row,
    /// DRAGON-489 follow-up). No OAuth: the account is already authorized, so this only
    /// re-lists its folders and lets the user RENAME it or pick a different destination. Opens
    /// the add dialog jumped straight to the Setup step (see `CloudAdd::edit_account`).
    ///
    /// Named `EditFolder` until DRAGON-495. It is offered on EVERY row now, including a
    /// provider with no folders at all (YouTube), because renaming is the half of the step
    /// every account has, and gating the button on folders left those rows unrenameable.
    EditAccount(String),
    /// Disconnect an account, by id: asks first (see [`Self::DisconnectConfirm`]), because
    /// it drops the stored tokens and cannot be undone.
    Disconnect(String),
    /// The user confirmed the disconnect, by id. This is the one that removes the account
    /// from `cloud::accounts` AND its tokens from `cloud::secrets`.
    DisconnectConfirm(String),
    /// The user backed out of the disconnect.
    DisconnectCancel,
    /// The disconnect finished: the account id (so the page can clear ONLY that id's
    /// in-flight "Disconnecting..." marker, DRAGON-489 follow-up), and either success or a
    /// message. `Err` is the LOCAL removal failing (a revoke that the provider refused is
    /// best-effort and never reported here), which the page shows as a notice, because an
    /// account still listed after a disconnect needs explaining.
    Disconnected(String, Result<(), String>),
    /// Cancel pressed on the post-connect setup step (DRAGON-489 follow-up).
    /// By this point the OAuth handshake already completed and saved a real, connected
    /// account (`connect_and_save`), so unlike every earlier step this one cannot just close
    /// the dialog: for a FRESH connect (`CloudAdd::reconnect_of` is `None`) that would leave a
    /// stray connected-but-unconfigured account behind, so this disconnects it too. For a
    /// RECONNECT of an existing account, cancelling just closes the dialog and keeps the
    /// (now freshly refreshed) tokens; the account already existed before this flow started
    /// and Cancel must not delete it.
    ///
    /// Since DRAGON-517 it also DISCARDS wherever the folder browser was left: the destination is
    /// written by [`Self::SetupDone`] alone, so an account opened from the gear, browsed around
    /// and then cancelled keeps the folder it had. That is what Cancel has always claimed to do,
    /// and could not while a pick wrote itself to disk on the spot.
    ///
    /// Named `CancelFolderStep` until DRAGON-495; the step it cancels is now the whole
    /// post-connect setup, not the folder choice alone.
    CancelSetupStep,
    /// A tick while the Browser step is showing (DRAGON-489 follow-up): nothing in state
    /// changes here, it exists purely to force the view to rebuild once a second so
    /// `cloud_browser_step`'s countdown (read from `CloudAdd::browser_started` and
    /// `Instant::now()` at render time) stays live.
    CloudBrowserTick,
    /// A tick while a folder listing is in flight (DRAGON-514): advance the path row's refresh
    /// glyph by one step, so it SPINS. That spin is the only indication the browser gives that
    /// a listing is running; the "Reading your folders..." line it replaced is deleted.
    ///
    /// Unlike [`Self::CloudBrowserTick`] this one does change state (`CloudAdd::folder_spin`),
    /// because the angle it advances has nowhere else to live. It stops with the listing.
    FolderSpinTick,
}

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
    /// Settings: toggle staying resident (the tray/menu-bar RESIDENT process). Emitted
    /// by the "System tray icon" row on macOS (menu-bar daemon), Linux (ksni
    /// tray resident, DRAGON-173), and Windows (Win32 tray daemon, DRAGON-237); a no-op on
    /// any platform without a resident, so the variant is gated to the three that construct it.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    SetResident(bool),
    /// Settings (DRAGON-296): toggle "Automatically start on login" — persist
    /// `autostart_on_login` and reconcile the OS login item (registered iff the tray is on
    /// AND this is on). Emitted by the row directly below "System tray icon", which is
    /// hidden while the tray is off. Gated to the three OSes with a resident (same set as
    /// `SetResident`), so Linux/other `SettingsMsg` shapes stay byte-identical elsewhere.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    SetAutostartOnLogin(bool),
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
    /// Settings: draw the group captions under the preview editor's top toolbar
    /// (DRAGON-478). Also changes the top bar's HEIGHT, so an open editor re-fits.
    SetPreviewToolbarLabels(bool),
    /// Settings → Health → Debug: write the opt-in debug log (DRAGON-419). Takes effect
    /// immediately, with no restart — a customer turns it on, reproduces the failure once,
    /// and mails the file.
    SetDebugLogging(bool),
    /// Settings → Health → Debug: open the macOS permission-checker window
    /// (`app::permissions`) from within Settings, rather than only via `--permissions`
    /// or missing-grant routing. Focuses it if already open. macOS-only, mirroring
    /// `PermissionsMsg`'s own cfg pattern; the row that fires this only renders on
    /// macOS, the sole platform with TCC grants to manage.
    #[cfg(target_os = "macos")]
    OpenPermissionsWindow,
    /// Settings: the image editor puts its edits on the clipboard as it closes (DRAGON-467).
    SetPreviewCopyOnExit(bool),
    /// Settings: write every screenshot to the configured save folder as it is taken
    /// (DRAGON-467). Off routes it through the session runtime directory instead.
    SetPreviewSaveOriginals(bool),
    /// Settings: ask before closing the image editor over unsaved edits (DRAGON-467).
    SetPreviewAskToSave(bool),
    /// Settings: the VIDEO editor's own copy-on-exit (DRAGON-467) — the three below mirror
    /// the three above for recordings, and are deliberately separate messages writing
    /// separate fields, so a video document can differ from an image one.
    SetPreviewVideoCopyOnExit(bool),
    /// Settings: the VIDEO editor's own save-originals (DRAGON-467), over `record_dir`.
    SetPreviewVideoSaveOriginals(bool),
    /// Settings: the VIDEO editor's own ask-to-save (DRAGON-467).
    SetPreviewVideoAskToSave(bool),
    /// Settings: open the folder picker for a save directory, then apply it.
    PickDir(DirTarget),
    DirPicked(DirTarget, Option<std::path::PathBuf>),
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
    /// Keyboard Shortcuts (macOS/Windows): edit one of the resident daemon's three global
    /// capture hotkey specs (DRAGON-295 — the `slot` picks which: All In One / Active Window
    /// / Active Monitor), e.g. "PrintScreen", "Cmd+Shift+2". Persisted; when the daemon is
    /// running, it is restarted so the new keys take effect. Gated to the two OSes with a
    /// daemon-owned global hotkey so Linux's `SettingsMsg` stays byte-identical (Linux's
    /// capture key is a COSMIC custom shortcut, not ours).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    SetCaptureHotkey(CaptureHotkeySlot, String),
    /// Keyboard Shortcuts (macOS/Windows): start (or cancel) RECORDING one of the three
    /// global capture hotkeys (DRAGON-295 — the `slot` picks which) — the next chord the
    /// user presses is captured, serialized to a daemon spec, and applied via
    /// `SetCaptureHotkey`. Mirrors `BeginRebind` for the in-app rows: press the button to
    /// begin, press again or Esc to cancel.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BeginCaptureHotkeyRebind(CaptureHotkeySlot),
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
    /// Cloud Accounts page (DRAGON-482). One variant carrying that page's own sub-enum;
    /// see [`CloudSettingsMsg`] for why the page keeps its messages together.
    Cloud(CloudSettingsMsg),
}
