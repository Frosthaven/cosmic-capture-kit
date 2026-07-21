//! The Settings window: a real toplevel built to COSMIC-Settings design
//! semantics — a CSD header bar (with a nav-rail hamburger + an expanding
//! search field), a collapsible `nav_bar` rail of pages on the left, and page
//! content built from native `settings::section`/`settings::item` rows (group
//! header + subdued helper text + right-aligned control).
//!
//! Opened on demand (the gear button) or directly via `--settings`. This module
//! owns the settings window's UI state and view; the persisted config *values*
//! (opacities, capture flags, scan options, paths, …) live on `App`, since the
//! capture/record logic shares them.

use super::*;

mod mic_test;
mod row;
mod deps;
mod pages;

// Re-export at the settings:: level so `super::settings::MIC_WAVE_COLUMNS` in
// actions.rs keeps resolving without any path change.
pub(super) use mic_test::MIC_WAVE_COLUMNS;

// Bring row helpers into this module's scope for rendered_sections.
use self::row::{reset_button, severity_caption, severity_title, Severity, SectionSpec};

/// The settings window title — also used to find/focus an already-open settings
/// window in another instance.
pub(crate) const WINDOW_TITLE: &str = "Cosmic Capture Kit";

/// macOS (DRAGON-135): width reserved at the header's leading edge for the native
/// traffic-light buttons (they end around x=62 in a standard window; 72 leaves a
/// comfortable gap before the first CSD control).
#[cfg(target_os = "macos")]
pub(super) const TRAFFIC_LIGHTS_INSET: f32 = 72.0;
/// Windows (DRAGON-284): LOGICAL width reserved at the header's TRAILING edge for the native
/// DWM caption-button cluster (minimize / maximize / close, top-right — the Windows mirror of
/// mac's leading `TRAFFIC_LIGHTS_INSET`). The Win11 cluster is ~3 × ~46px = ~138px; 146
/// leaves a small gap so the header title/content never slides under the buttons. A LOGICAL
/// value, so it's DPI-correct (the physical cluster scales with DPI, and the header lays out
/// in logical units). Verified on device against the live
/// [`crate::platform::windows::caption::caption_inset_logical`] (`DWMWA_CAPTION_BUTTON_BOUNDS`
/// ÷ DPI scale), logged at install time; tune this constant if that measurement differs.
#[cfg(windows)]
pub(super) const WIN_CAPTION_INSET: f32 = 146.0;
/// macOS: the compact header icon buttons' halo (padding around the 16px glyph)
/// and resulting box size — half the Linux halo, per the tighter hover boxes the
/// traffic lights sit beside. Tuned live on-device.
#[cfg(target_os = "macos")]
const MAC_HEADER_GLYPH_HALO: u16 = 4;
#[cfg(target_os = "macos")]
const MAC_HEADER_BTN: f32 = 16.0 + 2.0 * MAC_HEADER_GLYPH_HALO as f32;

/// "Window focus appearance" dropdown options (DRAGON-191): index 0 = Active, 1 =
/// Inactive — maps to `window_single_active` as a bool. Selects how a SINGLE-window
/// capture is portrayed; the old "Inactive with shadow" (shadow is a toggle now) and
/// "Raw" (a 0-width border) entries were removed.
const WINDOW_FOCUS_APPEARANCES: [&str; 2] = ["Active", "Inactive"];

/// The glyph the About nav entry swaps to when an update is available (DRAGON-175):
/// a "software update available" download icon. Standard freedesktop symbolic name
/// (COSMIC ships it); tinted to the theme success colour by `update_about_nav_icon`
/// and `about_rail_button_class`.
pub(in crate::app) const ABOUT_UPDATE_ICON: &str = "software-update-available-symbolic";


/// Settings pages — the nav-rail entries (formerly horizontal tabs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigTab {
    General,
    /// Scanner + Screenshots + Screen Recordings, consolidated into one nav page
    /// with in-page tabs (DRAGON-140; see [`CaptureTab`]).
    CaptureModes,
    /// Audio + Video, consolidated into one nav page with in-page tabs (DRAGON-141;
    /// see [`AudioVideoTab`]).
    AudioVideo,
    Shortcuts,
    Health,
    About,
}

/// The General page's in-page tabs (DRAGON-138): a horizontal strip splitting the
/// page into "Settings" (behavior / capture preview / after-capture) and
/// "Appearance" (the overlay-opacity group). Selection lives in
/// `SettingsState::general_tab`'s model — it is NOT persisted; the page always
/// opens on "Settings".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GeneralTab {
    Settings,
    Appearance,
}

/// The Capture Modes page's in-page tabs (DRAGON-140): a horizontal strip
/// splitting the consolidated page into "Scanner", "Screenshots" and "Screen
/// Recordings" — the three pages this nav entry replaced. Selection lives in
/// `SettingsState::capture_tab`'s model; it is NOT persisted — the page always
/// opens on "Scanner" (deep links, e.g. the missing-ffmpeg notice, may override
/// it to "Screen Recordings" for that open).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaptureTab {
    Scanner,
    Screenshots,
    Recordings,
}

/// The Audio & Video page's in-page tabs (DRAGON-141): a horizontal strip
/// splitting the consolidated page into "Audio" and "Video" — the two pages this
/// nav entry replaced. Selection lives in `SettingsState::audio_video_tab`'s
/// model; it is NOT persisted — the page always opens on "Audio" (so the live
/// mic sensitivity meter, gated on that tab being active, behaves predictably).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AudioVideoTab {
    Audio,
    Video,
}

/// The Keyboard Shortcuts page's in-page tabs (DRAGON-142): a horizontal strip
/// splitting the page's sections along the app's usage phases — "Capture" (the
/// capture overlay and app-wide contexts: the macOS global hotkey, OCR text
/// recognition, the settings panel, region selection), "Recording" (the live
/// recording session) and "Preview" (the post-capture editor, by far the largest
/// group). Selection lives in `SettingsState::shortcuts_tab`'s model; it is NOT
/// persisted — the page always opens on "Capture".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShortcutsTab {
    Capture,
    Recording,
    Preview,
}

impl ShortcutsTab {
    /// The in-page tab a shortcut section lives under, keyed by its group title
    /// ([`crate::shortcuts::Action::group`] — the single source of section grouping).
    /// Used by both the page view (which sections render under the active tab) and
    /// the per-tab reset scoping in `reset_page`.
    pub fn for_group(group: &str) -> Self {
        match group {
            "Recording" => ShortcutsTab::Recording,
            // The post-capture Preview Editor's two groups (DRAGON-158):
            // action shortcuts + the video editor's transport shortcuts.
            "Action Shortcuts" | "Video Editor Shortcuts" => ShortcutsTab::Preview,
            // Global (the macOS Start Capture hotkey), OCR Text Recognition,
            // Region Selection — the capture overlay / app-wide contexts.
            _ => ShortcutsTab::Capture,
        }
    }
}

/// A pending "reset to defaults" awaiting confirmation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResetScope {
    /// Reset just one settings page.
    Page(ConfigTab),
    /// Reset everything (factory reset).
    Factory,
}

/// All UI state for the settings window, grouped so `App` carries a single field.
pub struct SettingsState {
    /// The settings toplevel window, when open (`None` while closed).
    pub window: Option<window::Id>,
    /// Launched via `--settings`: closing the settings window exits the app.
    pub only: bool,
    /// Nav-rail model — one entry per page.
    pub nav: widget::segmented_button::SingleSelectModel,
    /// The General page's in-page tab strip model (Settings vs Appearance;
    /// DRAGON-138). Not persisted — `reset_general_tab` returns it to "Settings"
    /// each time the window opens.
    pub general_tab: widget::segmented_button::SingleSelectModel,
    /// The Capture Modes page's in-page tab strip model (Scanner / Screenshots /
    /// Screen Recordings; DRAGON-140). Not persisted — `reset_capture_tab` returns
    /// it to "Scanner" each time the window opens.
    pub capture_tab: widget::segmented_button::SingleSelectModel,
    /// The Capture Modes nav entity (activated when recording is attempted without
    /// ffmpeg, so the "install ffmpeg" notice on the Screen Recordings tab is
    /// surfaced — the deep link also sets `capture_tab` to that tab).
    pub capture_modes: widget::segmented_button::Entity,
    /// The Audio & Video page's in-page tab strip model (Audio / Video; DRAGON-141).
    /// Not persisted — `reset_audio_video_tab` returns it to "Audio" each time the
    /// window opens (the live mic sensitivity meter is gated on the Audio tab being
    /// active).
    pub audio_video_tab: widget::segmented_button::SingleSelectModel,
    /// The Keyboard Shortcuts page's in-page tab strip model (Capture / Recording /
    /// Preview; DRAGON-142). Not persisted — `reset_shortcuts_tab` returns it to
    /// "Capture" each time the window opens.
    pub shortcuts_tab: widget::segmented_button::SingleSelectModel,
    /// The Health nav entity — its stored icon is refreshed to the current overall
    /// severity (glyph + colour) so the EXPANDED rail shows green/amber/red too,
    /// matching the collapsed rail (see `App::update_health_nav_icon`).
    pub health: widget::segmented_button::Entity,
    /// The About nav entity — its stored icon/text-colour is refreshed to a
    /// download glyph in the theme success colour when an update is available
    /// (DRAGON-175), so the EXPANDED rail lights up too, matching the collapsed
    /// rail (see `App::update_about_nav_icon`).
    pub about: widget::segmented_button::Entity,
    /// Whether the nav rail is expanded (the header hamburger toggles it).
    pub nav_open: bool,
    /// Whether the header search field is expanded.
    pub search_active: bool,
    /// Current search query.
    pub search: String,
    /// Focus id for the search input.
    pub search_id: widget::Id,
    /// A reset-to-defaults action awaiting confirmation (shows the dialog).
    pub pending_reset: Option<ResetScope>,
    /// The keyboard-shortcut action currently capturing a new binding on the Keyboard
    /// Shortcuts page, if any — the next key press becomes its shortcut.
    pub rebinding: Option<crate::shortcuts::Action>,
    /// Appearance (DRAGON-139): the custom-accent colour picker model (hex/RGB input,
    /// hue slider, recent colours). Re-seeded to the live accent when the sidebar opens.
    pub accent_picker: widget::ColorPickerModel,
    /// Appearance: whether the hand-rolled custom-accent colour-picker sidebar panel is
    /// open (the framework context drawer doesn't render on our secondary window, so the
    /// panel is rendered inline in `config_window_view`).
    pub accent_editor_open: bool,
    /// DRAGON-191: the window-capture border colour picker model (hex/RGB input, hue
    /// slider). Re-seeded to the target border's current colour when the sidebar opens.
    pub border_picker: widget::ColorPickerModel,
    /// DRAGON-191: which border the picker sidebar is editing, or `None` when closed.
    pub border_editor: Option<crate::app::BorderColorTarget>,
    /// macOS (DRAGON-130) / Windows (DRAGON-237): whether the "Start Capture" global-hotkey
    /// row is CAPTURING the next keypress (the button-recording flow, mirroring `rebinding`
    /// but for the daemon's OS hotkey rather than an in-app [`crate::shortcuts::Action`]).
    /// cfg-gated to the daemon-hotkey OSes so the Linux settings state stays byte-identical.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub capture_hotkey_rebinding: bool,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsState {
    pub fn new() -> Self {
        let mut nav = widget::segmented_button::SingleSelectModel::default();
        let general = nav
            .insert()
            .text("General")
            .icon(widget::icon::from_name("preferences-system-symbolic"))
            .data(ConfigTab::General)
            .id();
        let capture_modes = nav
            .insert()
            .text("Capture Modes")
            .icon(widget::icon::from_name("accessories-screenshot-symbolic"))
            .data(ConfigTab::CaptureModes)
            .id();
        nav.insert()
            .text("Audio & Video")
            .icon(widget::icon::from_name("applications-multimedia-symbolic"))
            .data(ConfigTab::AudioVideo);
        nav.insert()
            .text("Keyboard Shortcuts")
            .icon(widget::icon::from_name("input-keyboard-symbolic"))
            .data(ConfigTab::Shortcuts);
        // The icon here is a placeholder; `update_health_nav_icon` replaces it with a
        // severity-tinted glyph before the nav is ever shown.
        let health = nav
            .insert()
            .text("Health")
            .icon(widget::icon::from_name("emblem-ok-symbolic"))
            .data(ConfigTab::Health)
            .id();
        // The icon here is a placeholder; `update_about_nav_icon` replaces it with a
        // success-tinted download glyph when an update is available (DRAGON-175).
        let about = nav
            .insert()
            .text("About")
            .icon(widget::icon::from_name("help-about-symbolic"))
            .data(ConfigTab::About)
            .id();
        nav.activate(general);
        // The General page's in-page tab strip (DRAGON-138). Two short labels; the
        // window always opens on the first ("Settings").
        let mut general_tab = widget::segmented_button::SingleSelectModel::default();
        let general_tab_settings = general_tab
            .insert()
            .text("Settings")
            .icon(widget::icon::from_name("emblem-system-symbolic"))
            .data(GeneralTab::Settings)
            .id();
        general_tab
            .insert()
            .text("Appearance")
            .icon(widget::icon::from_name("applications-graphics-symbolic"))
            .data(GeneralTab::Appearance);
        general_tab.activate(general_tab_settings);
        // The Capture Modes page's in-page tab strip (DRAGON-140). Each tab carries
        // the icon of the former nav page it replaced; the window always opens on the
        // first ("Scanner").
        let mut capture_tab = widget::segmented_button::SingleSelectModel::default();
        let capture_tab_scanner = capture_tab
            .insert()
            .text("Scanner")
            .icon(widget::icon::from_name("document-properties-symbolic"))
            .data(CaptureTab::Scanner)
            .id();
        capture_tab
            .insert()
            .text("Screenshots")
            .icon(widget::icon::from_name("camera-photo-symbolic"))
            .data(CaptureTab::Screenshots);
        capture_tab
            .insert()
            .text("Screen Recordings")
            .icon(widget::icon::from_name("camera-video-symbolic"))
            .data(CaptureTab::Recordings);
        capture_tab.activate(capture_tab_scanner);
        // The Audio & Video page's in-page tab strip (DRAGON-141). Each tab carries
        // the icon of the former nav page it replaced; the window always opens on the
        // first ("Audio").
        let mut audio_video_tab = widget::segmented_button::SingleSelectModel::default();
        let audio_video_tab_audio = audio_video_tab
            .insert()
            .text("Audio")
            .icon(widget::icon::from_name("audio-input-microphone-symbolic"))
            .data(AudioVideoTab::Audio)
            .id();
        audio_video_tab
            .insert()
            .text("Video")
            .icon(widget::icon::from_name("video-display-symbolic"))
            .data(AudioVideoTab::Video);
        audio_video_tab.activate(audio_video_tab_audio);
        // The Keyboard Shortcuts page's in-page tab strip (DRAGON-142): the page's
        // sections split along the app's usage phases; the window always opens on the
        // first ("Capture").
        let mut shortcuts_tab = widget::segmented_button::SingleSelectModel::default();
        let shortcuts_tab_capture = shortcuts_tab
            .insert()
            .text("Capture")
            .icon(widget::icon::from_name("camera-photo-symbolic"))
            .data(ShortcutsTab::Capture)
            .id();
        shortcuts_tab
            .insert()
            .text("Recording")
            .icon(widget::icon::from_name("media-record-symbolic"))
            .data(ShortcutsTab::Recording);
        shortcuts_tab
            .insert()
            .text("Preview Editor")
            .icon(widget::icon::from_name("image-x-generic-symbolic"))
            .data(ShortcutsTab::Preview);
        shortcuts_tab.activate(shortcuts_tab_capture);
        Self {
            window: None,
            only: false,
            nav,
            general_tab,
            capture_tab,
            capture_modes,
            audio_video_tab,
            shortcuts_tab,
            health,
            about,
            nav_open: true,
            search_active: false,
            search: String::new(),
            search_id: widget::Id::unique(),
            pending_reset: None,
            rebinding: None,
            accent_picker: widget::ColorPickerModel::new("Hex", "RGB", None, None),
            accent_editor_open: false,
            border_picker: widget::ColorPickerModel::new("Hex", "RGB", None, None),
            border_editor: None,
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            capture_hotkey_rebinding: false,
        }
    }

    /// The currently-selected page.
    pub fn active(&self) -> ConfigTab {
        self.nav
            .active_data::<ConfigTab>()
            .copied()
            .unwrap_or(ConfigTab::General)
    }

    /// The active General in-page tab (DRAGON-138).
    pub fn active_general_tab(&self) -> GeneralTab {
        self.general_tab
            .active_data::<GeneralTab>()
            .copied()
            .unwrap_or(GeneralTab::Settings)
    }

    /// Return the General page to its first ("Settings") in-page tab — called when
    /// the window opens so it never reopens on Appearance (the selection is not
    /// persisted).
    #[cfg_attr(target_os = "macos", allow(dead_code))] // Linux in-process settings only (DRAGON-153)
    pub fn reset_general_tab(&mut self) {
        let first = self
            .general_tab
            .iter()
            .find(|&e| self.general_tab.data::<GeneralTab>(e) == Some(&GeneralTab::Settings));
        if let Some(e) = first {
            self.general_tab.activate(e);
        }
    }

    /// The active Capture Modes in-page tab (DRAGON-140).
    pub fn active_capture_tab(&self) -> CaptureTab {
        self.capture_tab
            .active_data::<CaptureTab>()
            .copied()
            .unwrap_or(CaptureTab::Scanner)
    }

    /// Return the Capture Modes page to its first ("Scanner") in-page tab — called
    /// when the window opens so it never reopens on another tab (the selection is
    /// not persisted). A deep link may override this AFTER the window opens.
    #[cfg_attr(target_os = "macos", allow(dead_code))] // Linux in-process settings only (DRAGON-153)
    pub fn reset_capture_tab(&mut self) {
        self.set_capture_tab(CaptureTab::Scanner);
    }

    /// Select a specific Capture Modes in-page tab (the missing-ffmpeg deep link
    /// jumps straight to "Screen Recordings").
    pub fn set_capture_tab(&mut self, tab: CaptureTab) {
        let target = self
            .capture_tab
            .iter()
            .find(|&e| self.capture_tab.data::<CaptureTab>(e) == Some(&tab));
        if let Some(e) = target {
            self.capture_tab.activate(e);
        }
    }

    /// The active Audio & Video in-page tab (DRAGON-141).
    pub fn active_audio_video_tab(&self) -> AudioVideoTab {
        self.audio_video_tab
            .active_data::<AudioVideoTab>()
            .copied()
            .unwrap_or(AudioVideoTab::Audio)
    }

    /// Return the Audio & Video page to its first ("Audio") in-page tab — called
    /// when the window opens so it never reopens on the Video tab (the selection is
    /// not persisted, and the live mic sensitivity meter is gated on the Audio tab).
    #[cfg_attr(target_os = "macos", allow(dead_code))] // Linux in-process settings only (DRAGON-153)
    pub fn reset_audio_video_tab(&mut self) {
        let first = self.audio_video_tab.iter().find(|&e| {
            self.audio_video_tab.data::<AudioVideoTab>(e) == Some(&AudioVideoTab::Audio)
        });
        if let Some(e) = first {
            self.audio_video_tab.activate(e);
        }
    }

    /// The active Keyboard Shortcuts in-page tab (DRAGON-142).
    pub fn active_shortcuts_tab(&self) -> ShortcutsTab {
        self.shortcuts_tab
            .active_data::<ShortcutsTab>()
            .copied()
            .unwrap_or(ShortcutsTab::Capture)
    }

    /// Return the Keyboard Shortcuts page to its first ("Capture") in-page tab —
    /// called when the window opens so it never reopens on another tab (the
    /// selection is not persisted).
    #[cfg_attr(target_os = "macos", allow(dead_code))] // Linux in-process settings only (DRAGON-153)
    pub fn reset_shortcuts_tab(&mut self) {
        let first = self.shortcuts_tab.iter().find(|&e| {
            self.shortcuts_tab.data::<ShortcutsTab>(e) == Some(&ShortcutsTab::Capture)
        });
        if let Some(e) = first {
            self.shortcuts_tab.activate(e);
        }
    }
}

/// Open a settings toplevel and a Task reporting its id once mapped. Matches the
/// cosmic framework's window defaults: client-side decorations + a transparent
/// surface (so the rounded content corners show through) + a resize border.
/// `saved` is the last-closed size (logical w, h); it's restored but clamped so it
/// never opens larger than the monitor, and never below the min size.
pub(super) fn open_config_window(
    saved: Option<(u32, u32)>,
) -> (window::Id, Task<cosmic::Action<Msg>>) {
    // Minimum settings-window size (DRAGON-268 follow-up): the window can never open or
    // resize smaller than this, on all platforms. Kept in sync with
    // `App::apply_nav_auto_collapse_on_spawn`'s spawn-width floor in update/window_chrome.rs.
    const MIN_W: f32 = 800.0;
    const MIN_H: f32 = 600.0;
    let (mut w, mut h) = saved
        .map(|(w, h)| (w as f32, h as f32))
        .unwrap_or((MIN_W, MIN_H));
    // Clamp to the largest monitor so the settings window never opens off-screen.
    // Wayland-side sizing (screencopy); macOS lets AppKit place/clamp the window.
    #[cfg(target_os = "linux")]
    if let Some((mw, mh)) = crate::screencopy::largest_output_size() {
        w = w.min(mw as f32);
        h = h.min(mh as f32);
    }
    w = w.max(MIN_W);
    h = h.max(MIN_H);
    // Frosted glass (DRAGON-217): enroll the whole surface in the compositor's
    // backdrop blur when the user has frosted windows on. Gated at the seam —
    // `glass_windows_enabled` is `false` off COSMIC / macOS (the reader returns
    // None), so the window opens un-enrolled and opaque exactly as before.
    // `config_window_view`'s chrome paints translucent (`theme::frost_color`) so
    // the blur shows through.
    //
    // Windows (DRAGON-267 A/B): the material comes from a DWM **Mica** backdrop
    // (`DWMWA_SYSTEMBACKDROP_TYPE`, applied post-show in the `ConfigWindowFloat`
    // handler), NOT from winit's `blur`. winit maps `blur:true` on Windows to a
    // legacy DWM blur-behind (an `SetWindowCompositionAttribute` accent policy)
    // that COMPETES with — and overrides — the system backdrop, so Mica would
    // never render. So we do NOT enroll winit blur on Windows; the window still
    // opens `transparent:true` below (the alpha-composited substrate Mica needs).
    #[cfg(not(windows))]
    let blur = crate::app::theme::glass_windows_enabled();
    #[cfg(windows)]
    let blur = false;
    let (id, task) = window::open(window::Settings {
        size: cosmic::iced::Size::new(w, h),
        blur,
        min_size: Some(cosmic::iced::Size::new(MIN_W, MIN_H)),
        resizable: true,
        resize_border: 8,
        // macOS (DRAGON-135): NATIVE decorations, with the titlebar hidden/transparent
        // below — the system traffic lights render over our CSD header (which drops
        // its own window buttons there), matching mac expectations. Also keeps the
        // `Titled | Resizable` style mask that dodges winit's borderless `is_zoomed`
        // crash (see `shell::preview_window`). Linux stays fully CSD.
        #[cfg(target_os = "macos")]
        decorations: true,
        #[cfg(not(target_os = "macos"))]
        decorations: false,
        // macOS (DRAGON-146 / DRAGON-268): OPAQUE by default so the WindowServer masks
        // the window at the native corner radius (20pt with the unified toolbar) and
        // reports it via SkyLight, so JankyBorders (and any border tool) hugs the
        // corner (the NSWindow's own opacity — winit's `transparent` flag — is the one
        // lever that changes the reported radius; the CAMetalLayer's opacity does not).
        // BUT window vibrancy (DRAGON-268) needs a NON-opaque window for the frosted
        // material behind the content to show, so when frosted windows are ON we open
        // transparent; this REVERSES DRAGON-146 (the corner/border-tool tradeoff is
        // accepted pending live mac testing, DRAGON-268 Blocker 1) only in the glass
        // case, keeping the default (glass off) opaque + native-corner-reporting. The
        // capture overlays are a separate path (always transparent).
        #[cfg(target_os = "macos")]
        transparent: crate::app::theme::glass_windows_enabled(),
        #[cfg(not(target_os = "macos"))]
        transparent: true,
        exit_on_close_request: false,
        // The app id matches our installed `.desktop`, so the compositor shows the app
        // icon for the settings window (dock, task switcher, etc.). `application_id` is
        // a Wayland-only field of iced's PlatformSpecific; macOS uses its own defaults.
        #[cfg(target_os = "linux")]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific {
            application_id: "dev.frosthaven.CosmicCaptureKit".to_string(),
            ..Default::default()
        },
        // macOS: no visible titlebar (transparent, no title text), content full-size
        // behind it — only the traffic lights remain, floating over our header.
        #[cfg(target_os = "macos")]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        },
        #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific::default(),
        // Windows (DRAGON-229, KOMOREBI.md §7): open HIDDEN so the user's tiling WM
        // (komorebi) sees no `Show` event at creation; the `ConfigWindowOpened` handler's
        // Windows arm marks it komorebi-ineligible (WS_EX_DLGMODALFRAME) and shows it
        // natively (`ConfigWindowFloat` poll → `float_and_show`), so komorebi's first
        // sight of it is already ineligible and it is never tiled. Not the process's first
        // window — the hidden bootstrap surface owns the event loop — so hiding it can't
        // stall iced. mac / Linux keep the default (visible): they open directly.
        #[cfg(windows)]
        visible: false,
        ..Default::default()
    });
    (id, task.map(|id| cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::ConfigWindowOpened(id)))))
}

impl App {
    /// Open the settings window (no-op if it is already open). The capture
    /// overlay (a `Layer::Overlay` surface) would stack above the settings
    /// toplevel, so hide it while settings is open — `Msg::WindowClosed` brings
    /// it back.
    #[cfg_attr(target_os = "macos", allow(clippy::needless_return))]
    pub(super) fn open_settings(&mut self) -> Task<cosmic::Action<Msg>> {
        // Already open in this instance — just focus it.
        if let Some(id) = self.settings.window {
            return window::gain_focus(id);
        }
        // macOS (DRAGON-153): never convert this capture child into the settings
        // pane — the child is a never-activated ACCESSORY app (the DRAGON-151
        // overlay model) and the activation policy is boot-time-only, so a converted
        // pane could not be Cmd+Tab'd or focused like a real window. Hand off to a
        // fresh `--settings` process instead (it boots with the REGULAR policy) and
        // end this capture session. If a pane is already open somewhere, the child's
        // own launch path focuses it and exits.
        #[cfg(target_os = "macos")]
        {
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).arg("--settings").spawn();
            }
            return self.teardown();
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Only one settings pane may exist across all instances. If another
            // instance already has it open, focus that window (via a detached helper
            // so the activation outlives us) and end this capture attempt.
            if !crate::instance::acquire_settings_lock() {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).arg("--focus-settings").spawn();
                }
                return self.teardown();
            }
            // This instance is becoming a settings window rather than a capture
            // overlay, so give up the capture single-instance lock — another capture
            // instance may now launch even with "allow multiple instances" off.
            crate::instance::release_capture_lock();
            // Refresh the Health nav icon (glyph + colour) so the expanded rail matches the
            // collapsed rail's live status the moment the window opens.
            self.update_health_nav_icon();
            // The in-page tab selections are not persisted — always open on the first tab
            // (General → "Settings", DRAGON-138; Capture Modes → "Scanner", DRAGON-140;
            // Audio & Video → "Audio", DRAGON-141; Shortcuts → "Capture", DRAGON-142). A
            // deep link may override the capture tab after this returns.
            self.settings.reset_general_tab();
            self.settings.reset_capture_tab();
            self.settings.reset_audio_video_tab();
            self.settings.reset_shortcuts_tab();
            // Refresh the About nav icon too, then kick off a background update
            // check (DRAGON-175) so the nav lights up + the About page fills in
            // without ever delaying the window opening.
            self.update_about_nav_icon();
            // Auto-collapse the nav rail by spawn width before minting (icon follows).
            self.apply_nav_auto_collapse_on_spawn();
            let (id, task) = open_config_window(self.settings_size);
            self.settings.window = Some(id);
            let check = self.update_settings(SettingsMsg::CheckForUpdates);
            Task::batch([task, self.hide_overlays(), check])
        }
    }

    pub(super) fn config_window_view(&self) -> Element<'_, Msg> {
        let focused = self.core.focused_window() == self.settings.window;
        let active = self.settings.active();
        let searching = self.settings.search_active && !self.settings.search.trim().is_empty();
        // Frosted glass (DRAGON-217): the window/nav backgrounds paint translucent so
        // the compositor blur enrolled on this surface shows through. Captured once for
        // the container-style closures below; a no-op (fully opaque, today's look) off
        // COSMIC / when frosted windows are off.
        let glass = self.glass;

        // --- CSD header bar: nav hamburger + search + window controls ---
        let toggle_icon = widget::icon::from_name(if self.settings.nav_open {
            "navbar-open-symbolic"
        } else {
            "navbar-closed-symbolic"
        })
        .icon()
        .size(16);
        // Nav toggle sized EXACTLY to the collapsed rail's column width (44px = 32px
        // icon button + 6px padding each side). A fixed-width button::icon left-aligns
        // its glyph, so use button::custom with a fill+center container holding a 16px
        // symbolic icon (matching the rail's glyph size) to both hit 44px and centre.
        #[cfg(not(target_os = "macos"))]
        let toggle_nav = crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::custom(
                widget::container(toggle_icon)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    // Nudge 1px right of centre: 2px left padding makes the centred icon's
                    // gaps 15px left / 13px right (1px more left, 1px less right).
                    .padding([0, 0, 0, 2]),
            )
            .class(cosmic::theme::Button::NavToggle)
            .selected(focused)
            .padding(0)
            .width(Length::Fixed(44.0))
            .height(Length::Fixed(36.0))
            .on_press(Msg::WindowChrome(WindowChromeMsg::ToggleConfigNav)),
        );
        // macOS (DRAGON-135): next to the traffic lights the rail-width NavToggle box
        // reads as dead space, so the toggle is a COMPACT box instead — the 16px glyph
        // with a MAC_HEADER_GLYPH_HALO halo, standard Icon class (the theme's medium
        // rounding) — pinned to the TOP of the header slot, where its centre lands on
        // the lights' centreline (tuned live on-device).
        #[cfg(target_os = "macos")]
        let toggle_nav = widget::container(
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::custom(
                    widget::container(toggle_icon)
                        .center_x(Length::Fill)
                        .center_y(Length::Fill),
                )
                .class(cosmic::theme::Button::Icon)
                .selected(focused)
                .padding(0)
                .width(Length::Fixed(MAC_HEADER_BTN))
                .height(Length::Fixed(MAC_HEADER_BTN))
                .on_press(Msg::WindowChrome(WindowChromeMsg::ToggleConfigNav)),
            ),
        )
        .height(Length::Fill)
        .align_y(cosmic::iced::Alignment::Start);
        let search: Element<'_, Msg> = if self.settings.search_active {
            widget::text_input::search_input("Search settings", &self.settings.search)
                .width(Length::Fixed(240.0))
                .id(self.settings.search_id.clone())
                .on_input(|a0| Msg::WindowChrome(WindowChromeMsg::ConfigSearchInput(a0)))
                .on_clear(Msg::WindowChrome(WindowChromeMsg::ConfigSearchClear))
                .into()
        } else {
            let btn = widget::button::icon(widget::icon::from_name("system-search-symbolic"))
                .on_press(Msg::WindowChrome(WindowChromeMsg::ConfigSearchActivate));
            // macOS: same compact top-pinned box as the nav toggle, for the same
            // traffic-light alignment (a symmetric halo, nothing hanging below).
            #[cfg(target_os = "macos")]
            {
                widget::container(crate::widgets::arrow_cursor::arrow_cursor(
                    btn.padding(MAC_HEADER_GLYPH_HALO),
                ))
                .height(Length::Fill)
                .align_y(cosmic::iced::Alignment::Start)
                .into()
            }
            #[cfg(not(target_os = "macos"))]
            {
                crate::widgets::arrow_cursor::arrow_cursor(btn.padding(8))
            }
        };
        let header = widget::header_bar()
            .title(WINDOW_TITLE)
            .focused(focused)
            .on_drag(Msg::WindowChrome(WindowChromeMsg::ConfigWindowDrag))
            .on_double_click(Msg::WindowChrome(WindowChromeMsg::ConfigWindowMaximize));
        // macOS (DRAGON-135): the native traffic lights (top-left, over our header —
        // the window opens with a transparent/hidden titlebar) replace the CSD window
        // buttons, so none are drawn and the leading controls inset clear of them.
        // Close routes through the native button → `WindowCloseRequested`;
        // minimize/zoom are native. Vertical alignment against the lights comes from
        // the toolbar style in `center_titlebar_buttons`, not padding here (growing
        // a child of the centre-aligned header row just clips it).
        //
        // DRAGON-268 follow-up (fullscreen header vanish): in NATIVE fullscreen the
        // traffic lights auto-hide (they only drop down on a mouse-to-top), so the
        // 72px inset reserved for them is dead space that pushes the nav toggle /
        // search needlessly right. Collapse the inset to 0 in fullscreen so the app's
        // own header controls sit flush at the leading edge where the lights were. The
        // native toolbar-detach in `match_window_fullscreen_backdrop` is what keeps the
        // buttons visible at all in fullscreen; this just seats them correctly.
        #[cfg(target_os = "macos")]
        let lead_inset = if self.settings_fullscreen {
            0.0
        } else {
            TRAFFIC_LIGHTS_INSET
        };
        #[cfg(target_os = "macos")]
        let header = header
            .start(widget::Space::new().width(Length::Fixed(lead_inset)))
            .start(toggle_nav)
            .start(search);
        // Windows (DRAGON-284): the native DWM caption buttons (min/max/close, top-right)
        // own close/minimize/maximize, so the CSD window buttons are OMITTED (no `.on_close`/
        // `.on_maximize`/`.on_minimize`). Close routes native ✕ → `WindowCloseRequested`;
        // maximize via the native button OR the header double-click (both → the native
        // `toggle_maximize`). A trailing `WIN_CAPTION_INSET` spacer reserves the cluster's
        // width so the header title/content never slides under the buttons. The native
        // buttons are installed post-show in the `ConfigWindowFloat` handler.
        #[cfg(windows)]
        let header = header
            .start(toggle_nav)
            .start(search)
            .end(widget::Space::new().width(Length::Fixed(WIN_CAPTION_INSET)));
        #[cfg(all(not(target_os = "macos"), not(windows)))]
        let header = header
            .start(toggle_nav)
            .start(search)
            .on_close(Msg::WindowChrome(WindowChromeMsg::CloseConfigWindow))
            .on_maximize(Msg::WindowChrome(WindowChromeMsg::ConfigWindowMaximize))
            .on_minimize(Msg::WindowChrome(WindowChromeMsg::ConfigWindowMinimize));

        // --- Page content (sections), or global search results ---
        // A page is a PINNED head (the page title + any in-page tab strip) stacked
        // over a SCROLLABLE body (the section groups + the per-page reset button), so
        // the title/tabs stay fixed at the top and only the content below scrolls
        // (DRAGON-268 follow-up). `head` is `None` for the global-search view (there
        // is no single title/tab strip there — the whole result list scrolls).
        //
        // Both the head and the scrolled body get the SAME centering + max-width +
        // horizontal padding, so a scrolled row lines up exactly under the pinned
        // title/tabs. Only the vertical padding differs (the head owns the top inset;
        // the body owns the bottom inset), so the scroll seam sits between them.
        let head: Option<Element<'_, Msg>>;
        let body_inner: Element<'_, Msg>;
        if searching {
            let q = self.settings.search.trim().to_lowercase();
            let mut col: Vec<Element<'_, Msg>> = Vec::new();
            for tab in [
                ConfigTab::General,
                ConfigTab::CaptureModes,
                ConfigTab::AudioVideo,
                ConfigTab::Shortcuts,
                ConfigTab::Health,
                ConfigTab::About,
            ] {
                let secs = self.rendered_sections(tab, Some(&q));
                if !secs.is_empty() {
                    col.push(widget::text::title4(Self::page_name(tab)).into());
                    col.extend(secs);
                }
            }
            if col.is_empty() {
                col.push(widget::text::body("No matching settings.").into());
            }
            head = None;
            body_inner = widget::settings::view_column(col).into();
        } else {
            // The pinned head: the page title, plus the in-page tab strip for the
            // tabbed pages. Built as its own `view_column` so its title/tab spacing
            // matches the section spacing below.
            // The title fills the width so the CENTERED helper's inner max-width box is
            // always the full 820 (not shrunk to the title's own width): on tabbed pages
            // the tab strip already stretches the head to 820, so a fill title is a no-op
            // there; on the non-tabbed Health/About pages the head is JUST the title, so
            // without this the box hugs the title and `center_x` centres it — this keeps
            // Health/About titles LEFT-aligned exactly like the tabbed pages (DRAGON-268).
            let mut head_col: Vec<Element<'_, Msg>> =
                vec![widget::text::title3(Self::page_name(active))
                    .width(Length::Fill)
                    .into()];
            let mut col: Vec<Element<'_, Msg>> = Vec::new();
            match active {
                ConfigTab::General => {
                    // The General page splits into two in-page tabs (DRAGON-138): the
                    // horizontal strip selects which section subset renders below it.
                    head_col.push(self.tab_strip(&self.settings.general_tab, |e| {
                        Msg::Settings(SettingsMsg::SetGeneralTab(e))
                    }));
                    let specs = match self.settings.active_general_tab() {
                        GeneralTab::Settings => self.general_settings_sections(),
                        GeneralTab::Appearance => self.general_appearance_sections(),
                    };
                    col.extend(self.render_specs(specs, None));
                }
                ConfigTab::CaptureModes => {
                    // Scanner / Screenshots / Screen Recordings, split into in-page tabs
                    // (DRAGON-140) — the strip selects which page's sections render below.
                    head_col.push(self.tab_strip(&self.settings.capture_tab, |e| {
                        Msg::Settings(SettingsMsg::SetCaptureTab(e))
                    }));
                    let specs = match self.settings.active_capture_tab() {
                        CaptureTab::Scanner => self.scanner_sections(),
                        CaptureTab::Screenshots => self.screenshots_sections(),
                        CaptureTab::Recordings => self.recordings_sections(),
                    };
                    col.extend(self.render_specs(specs, None));
                }
                ConfigTab::AudioVideo => {
                    // Audio / Video, split into in-page tabs (DRAGON-141) — the strip
                    // selects which page's sections render below.
                    head_col.push(self.tab_strip(&self.settings.audio_video_tab, |e| {
                        Msg::Settings(SettingsMsg::SetAudioVideoTab(e))
                    }));
                    let specs = match self.settings.active_audio_video_tab() {
                        AudioVideoTab::Audio => self.audio_sections(),
                        AudioVideoTab::Video => self.video_sections(),
                    };
                    col.extend(self.render_specs(specs, None));
                }
                ConfigTab::Shortcuts => {
                    // Capture / Recording / Preview, split into in-page tabs
                    // (DRAGON-142) — the strip selects which shortcut sections
                    // render below (`ShortcutsTab::for_group` on the section title).
                    head_col.push(self.tab_strip(&self.settings.shortcuts_tab, |e| {
                        Msg::Settings(SettingsMsg::SetShortcutsTab(e))
                    }));
                    let tab = self.settings.active_shortcuts_tab();
                    let specs = self
                        .keyboard_sections()
                        .into_iter()
                        .filter(|s| ShortcutsTab::for_group(s.title) == tab)
                        .collect();
                    col.extend(self.render_specs(specs, None));
                }
                _ => col.extend(self.rendered_sections(active, None)),
            }
            // A standalone "Reset to defaults" (this page) below the groups. The
            // About and Health pages are read-only, so they get no reset button.
            if active != ConfigTab::About && active != ConfigTab::Health {
                // A content-sized "Reset to defaults" chip at the column's leading edge
                // (NOT centred in the column, per the user). Its icon + label are centred
                // WITHIN the chip via the helper, which for a Shrink width just hugs the
                // content and keeps them vertically centred.
                col.push(
                    row::centered_button(
                        Some("edit-undo-symbolic"),
                        "Reset to defaults",
                        Length::Shrink,
                        row::standard_button_class(),
                        Some(Msg::WindowChrome(WindowChromeMsg::RequestReset(ResetScope::Page(active)))),
                    ),
                );
            }
            head = Some(widget::settings::view_column(head_col).into());
            body_inner = widget::settings::view_column(col).into();
        }
        // Center a page element with a max width + generous horizontal padding, like
        // cosmic-settings. It sits directly on the window background (no separate
        // content panel). `top`/`bottom` are supplied per element so the pinned head
        // owns the top inset and the scrolled body owns the bottom inset. An inner
        // `fn` (not a closure) keeps the element lifetime explicit — the invariant
        // `Container<'a>` return can't infer through an elided-lifetime closure.
        fn centered<'a>(el: Element<'a, Msg>, top: f32, bottom: f32) -> Element<'a, Msg> {
            widget::container(
                widget::container(el)
                    .max_width(820.0)
                    .padding(cosmic::iced::Padding {
                        top,
                        right: 24.0,
                        bottom,
                        left: 24.0,
                    }),
            )
            .center_x(Length::Fill)
            .width(Length::Fill)
            .into()
        }
        // The scrollable body fills the remaining vertical space below the pinned
        // head (`Length::Fill`), so ONLY the section content scrolls.
        let scroll_body = widget::scrollable(centered(body_inner, 8.0, 24.0))
            .height(Length::Fill)
            .width(Length::Fill);
        let content: Element<'_, Msg> = if let Some(head) = head {
            // Tabbed / titled page: the head (title + tabs) is PINNED at the top; the
            // body scrolls beneath it. The head owns the 8px top inset plus an 8px bottom
            // gap so the tab strip's underline clears the first scrolled row (an opaque
            // backdrop was tried and rejected: on a translucent window a second material
            // layer only darkens the chrome, and it did not occlude anyway).
            widget::column(vec![centered(head, 8.0, 8.0), scroll_body.into()])
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            // Global search: no single head, the whole result list scrolls.
            scroll_body.into()
        };

        // --- Nav rail: full panel when expanded, icon-only rail when collapsed ---
        let nav_panel: Element<'_, Msg> = if self.settings.nav_open {
            // Nav fills and scrolls if the window is too short; a "Factory reset"
            // button is pinned to the bottom of the rail (bottom-left of the window).
            // A `standard` button can't be made full-width (its width only sizes the
            // inner row, not the outer button), so build a custom one whose own width
            // is Fill — that stretches it to the nav pane.
            // Full-width; the helper centres the icon + label within it (a bare
            // leading row hangs the text at the button's left edge). DRAGON-268
            // follow-up: user wants this centred.
            let factory = row::centered_button(
                Some("edit-undo-symbolic"),
                "Factory reset",
                Length::Fill,
                row::standard_button_class(),
                Some(Msg::WindowChrome(WindowChromeMsg::RequestReset(ResetScope::Factory))),
            );
            // The nav rail's list (DRAGON-279): its container fill — libcosmic's opaque
            // `nav_bar_style` (`primary.base`) — is overridden to FULLY TRANSPARENT so the
            // window backdrop shows straight through the rail (the selected/hover pills are
            // the inner segmented button, unaffected). UNCONDITIONAL on every platform
            // (user decision 2026-07-19; see `theme::frost_component`).
            let nav_widget = widget::nav_bar(&self.settings.nav, |a0| Msg::WindowChrome(WindowChromeMsg::SetConfigTab(a0)));
            let nav_list: Element<'_, Msg> = nav_widget
                .into_container()
                .class(cosmic::theme::Container::custom(move |theme| {
                    let cosmic = theme.cosmic();
                    cosmic::iced::widget::container::Style {
                        icon_color: Some(cosmic.on_bg_color().into()),
                        text_color: Some(cosmic.on_bg_color().into()),
                        background: Some(Background::Color(cosmic::iced::Color::TRANSPARENT)),
                        border: Border {
                            width: 0.0,
                            color: cosmic::iced::Color::TRANSPARENT,
                            radius: cosmic.corner_radii.radius_s.into(),
                        },
                        ..Default::default()
                    }
                }))
                .into();
            widget::container(
                widget::column(vec![
                    widget::scrollable(nav_list)
                        .height(Length::Fill)
                        .width(Length::Fill)
                        .into(),
                    factory,
                ])
                .spacing(8.0)
                .width(Length::Fill)
                .height(Length::Fill),
            )
            // Widened slightly (was 190) to seat the longer "Screen Recordings" nav
            // label + the full-width "Factory reset" button without clipping.
            .width(Length::Fixed(210.0))
            .height(Length::Fill)
            .into()
        } else {
            // Icon-only rail. Match the expanded nav's vertical metrics (32px
            // buttons + a `space_xxs` top inset and spacing) so the icons keep the
            // same Y positions when toggling.
            let xxs = f32::from(cosmic::theme::active().cosmic().space_xxs());
            let mut rail: Vec<Element<'_, Msg>> = Vec::new();
            for entity in self.settings.nav.iter() {
                if let Some(tab) = self.settings.nav.data::<ConfigTab>(entity).copied() {
                    // The Health entry's icon reflects overall status: a severity glyph
                    // tinted green/amber/red. The About entry lights up with a
                    // success-tinted download glyph when an update is available
                    // (DRAGON-175). Every other entry uses its static icon.
                    let (icon_name, class) = if tab == ConfigTab::Health {
                        let sev = self.health_level();
                        (sev.icon_name(), health_rail_button_class(sev, tab == active))
                    } else if tab == ConfigTab::About && self.update_status.is_available() {
                        (ABOUT_UPDATE_ICON, about_rail_button_class(tab == active))
                    } else {
                        (Self::page_icon(tab), rail_button_class(tab == active))
                    };
                    rail.push(crate::widgets::arrow_cursor::arrow_cursor(
                        widget::button::icon(widget::icon::from_name(icon_name))
                            .class(class)
                            .tooltip(Self::page_name(tab))
                            .on_press(Msg::WindowChrome(WindowChromeMsg::SetConfigTab(entity)))
                            .width(Length::Fixed(32.0))
                            .height(Length::Fixed(32.0)),
                    ));
                }
            }
            let factory = widget::button::icon(widget::icon::from_name("edit-undo-symbolic"))
                .tooltip("Factory reset")
                .on_press(Msg::WindowChrome(WindowChromeMsg::RequestReset(ResetScope::Factory)))
                .width(Length::Fixed(32.0))
                .height(Length::Fixed(32.0));
            widget::container(
                widget::column(vec![
                    widget::scrollable(widget::column(rail).spacing(xxs))
                        .height(Length::Fill)
                        .into(),
                    crate::widgets::arrow_cursor::arrow_cursor(factory),
                ])
                .spacing(xxs),
            )
                .height(Length::Fill)
                .padding(cosmic::iced::Padding {
                    top: xxs,
                    right: 6.0,
                    bottom: xxs,
                    left: 6.0,
                })
                .class(cosmic::theme::Container::custom(move |theme| {
                    // DRAGON-279: the collapsed rail is FULLY transparent (matching the
                    // expanded rail), so the backdrop shows through regardless of the
                    // window's glass alpha. UNCONDITIONAL on every platform (user decision
                    // 2026-07-19; see `theme::frost_component`).
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(cosmic::iced::Color::TRANSPARENT)),
                        border: Border {
                            radius: theme::rounding(theme).s.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                }))
                .into()
        };

        // --- Body: nav rail + content (+ the accent-picker sidebar when open) ---
        // The framework context drawer doesn't render on this secondary window on our
        // libcosmic rev (only the main window is wrapped), so the custom-accent picker
        // is a hand-rolled right sidebar rendered inline here (DRAGON-139).
        let mut body_row = vec![nav_panel, content];
        if self.settings.accent_editor_open {
            body_row.push(self.accent_picker_panel());
        }
        if self.settings.border_editor.is_some() {
            body_row.push(self.border_picker_panel());
        }
        let body = widget::container(
            widget::row(body_row)
                .spacing(8.0)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .padding(cosmic::iced::Padding {
            top: 0.0,
            right: 8.0,
            bottom: 8.0,
            left: 8.0,
        })
        .width(Length::Fill)
        .height(Length::Fill);

        // The whole window is an opaque rounded background with a hairline border;
        // the transparent surface only shows through outside the rounded corners.
        // Mirrors the cosmic framework's outer window container.
        let stacked = widget::column(vec![header.into(), body.into()])
            .width(Length::Fill)
            .height(Length::Fill);
        // Frosted glass (DRAGON-217): the window background paints translucent (`glass`,
        // captured above) so the compositor blur enrolled on this surface shows through
        // the content area (the content sits directly on this background).
        let window: Element<'_, Msg> = widget::container(stacked)
            .padding(1)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(move |theme| {
                let cosmic = theme.cosmic();
                // Windows (DRAGON-276): DWM already rounds the window (DWMWCP_ROUND, DRAGON-274),
                // so rounding this outer container too paints a SECOND, differently-sized corner
                // inside DWM's — the "thick double corner". Fill SQUARE and let DWM do the single
                // rounding. Linux keeps the app-drawn radius (its window edge is the app's).
                #[cfg(target_os = "linux")]
                let radius = theme::rounding(theme).window();
                // macOS: the settings toplevel uses NATIVE decorations (DRAGON-135), so its
                // rounded window edge is drawn by AppKit at the fixed system corner radius, NOT
                // the app's roundness preference. Tracing that OS corner (not
                // `rounding().window()`, which under the "round" preset curves on a much larger
                // arc) is what keeps the 1px border a smooth curve instead of jagged segments.
                #[cfg(target_os = "macos")]
                let radius = [crate::platform::mac::window::native_window_corner_radius(); 4];
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(theme::frost_color(
                        cosmic.background.base.into(),
                        glass,
                    ))),
                    border: Border {
                        color: cosmic.bg_divider().into(),
                        width: 1.0,
                        #[cfg(windows)]
                        radius: 0.0.into(),
                        #[cfg(not(windows))]
                        radius: radius.into(),
                    },
                    ..Default::default()
                }
            }))
            .into();

        // Live microphone-test modal, stacked over the window while open. The capture itself
        // may also run without the modal (to feed the sensitivity bar), so gate on the flag.
        if self.mic_test_modal_open {
            return self.mic_test_modal(window);
        }

        // Launch-time update dialog (DRAGON-177), stacked over the window while shown.
        if self.update_dialog.is_some() {
            return self.update_dialog_modal(window);
        }

        // Reset confirmation modal, stacked over the window when pending.
        let Some(scope) = self.settings.pending_reset else {
            return window;
        };
        let (title, body) = match scope {
            ResetScope::Factory => (
                "Factory reset?",
                "This restores every setting on every page to its default. This cannot be undone.",
            ),
            ResetScope::Page(_) => (
                "Reset this page?",
                "This restores the settings on this page to their defaults. This cannot be undone.",
            ),
        };
        let backdrop: Element<'_, Msg> = widget::mouse_area(
            widget::container(widget::Space::new().width(Length::Fill).height(Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(|_t| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(theme::SCRIM)),
                        ..Default::default()
                    }
                })),
        )
        .on_press(Msg::WindowChrome(WindowChromeMsg::CancelReset))
        .interaction(cosmic::iced::mouse::Interaction::Idle)
        .into();
        let dialog = widget::dialog()
            .title(title)
            .body(body)
            .primary_action(crate::widgets::arrow_cursor::arrow_cursor(widget::button::destructive("Reset").on_press(Msg::WindowChrome(WindowChromeMsg::ConfirmReset))))
            .secondary_action(crate::widgets::arrow_cursor::arrow_cursor(widget::button::text("Cancel").on_press(Msg::WindowChrome(WindowChromeMsg::CancelReset))));
        let centered: Element<'_, Msg> = widget::container(dialog)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        cosmic::iced::widget::stack(vec![window, backdrop, centered]).into()
    }

    /// The launch-time update dialog (DRAGON-177), stacked over the settings window.
    /// The copy is EXACTLY the ticket's wording (no em/en-dashes); the "Don't remind
    /// me again" checkbox drives `update_dialog.dont_remind`, applied to the
    /// `notify_updates` setting when either button is clicked. "Update Now" runs the
    /// platform update flow; "Update Later" just dismisses for this session.
    fn update_dialog_modal<'a>(&'a self, window: Element<'a, Msg>) -> Element<'a, Msg> {
        // No dialog state ⇒ nothing to stack (the caller gates on `is_some`, but stay
        // defensive so this never panics on a stale call).
        let Some(dialog) = self.update_dialog.as_ref() else {
            return window;
        };
        // The "Don't remind me again" checkbox — two-way bound to the dialog state.
        // Wrapped so it shows the arrow, not the hand, on hover (house style).
        let remind = crate::widgets::arrow_cursor::arrow_cursor(
            widget::checkbox(dialog.dont_remind)
                .label("Don't remind me again")
                .on_toggle(|c| Msg::Settings(SettingsMsg::UpdateDialogRemindToggled(c))),
        );
        let card = widget::dialog()
            .title("Update available")
            .body(
                "There is a new update, would you like to update now? You can always \
                 update from the About page in settings.",
            )
            .control(remind)
            .primary_action(crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::suggested("Update Now")
                    .on_press(Msg::Settings(SettingsMsg::UpdateDialogNow)),
            ))
            .secondary_action(crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::text("Update Later")
                    .on_press(Msg::Settings(SettingsMsg::UpdateDialogLater)),
            ));
        // A click-absorbing scrim behind the card. Unlike the reset modal, an
        // outside click does NOT dismiss (the user must choose an explicit action so
        // the checkbox decision is deliberate), so the backdrop is inert.
        let backdrop: Element<'_, Msg> = widget::mouse_area(
            widget::container(widget::Space::new().width(Length::Fill).height(Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(|_t| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(theme::SCRIM)),
                        ..Default::default()
                    }
                })),
        )
        .on_press(Msg::WindowChrome(WindowChromeMsg::Ignore))
        .interaction(cosmic::iced::mouse::Interaction::Idle)
        .into();
        let centered: Element<'_, Msg> = widget::container(card)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        cosmic::iced::widget::stack(vec![window, backdrop, centered]).into()
    }

    /// Render a page's sections to elements, dropping items (and then empty
    /// sections) that don't match `filter` (a lowercased query), if any.
    fn rendered_sections(&self, tab: ConfigTab, filter: Option<&str>) -> Vec<Element<'_, Msg>> {
        self.render_specs(self.sections_for(tab), filter)
    }

    /// Render an explicit list of section specs to elements (the shared body of
    /// `rendered_sections`), dropping items — and then empty sections — that don't
    /// match `filter` (a lowercased query), if any. Split out (DRAGON-138) so the
    /// General page can render just one of its two in-page tabs' specs.
    fn render_specs<'a>(
        &'a self,
        specs: Vec<SectionSpec<'a>>,
        filter: Option<&str>,
    ) -> Vec<Element<'a, Msg>> {
        let mut out: Vec<Element<'_, Msg>> = Vec::new();
        for spec in specs {
            let mut rows: Vec<Element<'a, Msg>> = Vec::new();
            let mut any = false;
            // A query that matches the SECTION title keeps the whole section (all its
            // items), mirroring cosmic-settings' search (which checks section title +
            // descriptions). Without this, groups whose keyword lives only in the title —
            // e.g. "Overlay Opacity", whose items are "During …" with empty descs — were
            // unfindable once the Appearance in-page tab (DRAGON-138) hid them from the
            // default view. Also what makes the full-width note rows (accent swatches,
            // style previews — no item title/desc) reachable by their section title.
            let section_hit =
                filter.is_some_and(|q| spec.title.to_lowercase().contains(q));
            for it in spec.items {
                let keep = match filter {
                    Some(q) => {
                        section_hit
                            || it.title.to_lowercase().contains(q)
                            || it.desc.to_lowercase().contains(q)
                    }
                    None => true,
                };
                if keep {
                    if it.note {
                        // Full-width inline status line (icon + message); no controls.
                        let line: Element<'_, Msg> =
                            it.desc_el.unwrap_or_else(|| widget::text("").into());
                        rows.push(widget::settings::item_row(vec![
                            widget::container(line).width(Length::Fill).into(),
                        ]).into());
                        any = true;
                        continue;
                    }
                    // Every row reserves a fixed-width unit slot and reset slot
                    // (even when empty) so all controls share one right edge — the
                    // control sits left of the unit label, which sits left of the
                    // reset icon.
                    let suffix_slot: Element<'_, Msg> = match it.suffix {
                        Some(s) => widget::container(widget::text::caption(s))
                            .width(Length::Fixed(24.0))
                            .into(),
                        None => widget::space::Space::new()
                            .width(Length::Fixed(24.0))
                            .into(),
                    };
                    let reset_slot: Element<'_, Msg> = match it.reset {
                        Some((msg, changed)) => widget::container(reset_button(msg, changed))
                            .center_x(Length::Fixed(28.0))
                            .into(),
                        None => widget::space::Space::new()
                            .width(Length::Fixed(28.0))
                            .into(),
                    };
                    let control = widget::row(vec![it.control, suffix_slot, reset_slot])
                        .spacing(8.0)
                        .align_y(Alignment::Center);
                    // Build the row ourselves (vs `item::builder`) so the helper
                    // text uses our secondary tone; `item_row` is the same layout
                    // primitive the builder uses internally.
                    let label: Element<'_, Msg> = if let Some(sev) = it.gated {
                        // A control gated by a missing dependency: severity-tinted title,
                        // with any helper text in the same severity colour.
                        let title = severity_title(sev, it.title);
                        if it.desc.is_empty() {
                            widget::container(title).width(Length::Fill).into()
                        } else {
                            widget::column(vec![title, severity_caption(sev, it.desc)])
                                .spacing(2.0)
                                .width(Length::Fill)
                                .into()
                        }
                    } else if it.dim {
                        // Informational/unavailable rows (e.g. a disabled encoder-preset
                        // row): keep the title at the SAME body size + weight as an active
                        // row so disabling never shifts the layout (DRAGON-158). Normal
                        // text colour, not the secondary tone (which failed contrast).
                        widget::text::body(it.title)
                            .font(cosmic::font::bold())
                            .width(Length::Fill)
                            .into()
                    } else if let Some(de) = it.desc_el {
                        // Rich helper line (e.g. "Provided by <links>") under the title.
                        widget::column(vec![
                            widget::text::body(it.title).font(cosmic::font::bold()).into(),
                            de,
                        ])
                        .spacing(2.0)
                        .width(Length::Fill)
                        .into()
                    } else if it.desc.is_empty() {
                        widget::text::body(it.title)
                            .font(cosmic::font::bold())
                            .width(Length::Fill)
                            .into()
                    } else {
                        // Option name in bold; description in the normal text colour
                        // (the secondary tone failed contrast/accessibility).
                        let helper: Element<'_, Msg> = match it.severity {
                            Some(sev) => severity_caption(sev, it.desc),
                            None => widget::text::caption(it.desc).into(),
                        };
                        widget::column(vec![
                            widget::text::body(it.title).font(cosmic::font::bold()).into(),
                            helper,
                        ])
                        .spacing(2.0)
                        .width(Length::Fill)
                        .into()
                    };
                    rows.push(widget::settings::item_row(vec![label, control.into()]).into());
                    any = true;
                }
            }
            if any {
                out.push(self.section_card(spec.title, rows));
            }
        }
        out
    }

    /// Assemble a rendered section's rows into a titled group of PILL-MATERIAL row
    /// cards (DRAGON-279) with transparent 1px seams so the backdrop shows between
    /// rows.
    fn section_card<'a>(&self, title: &'a str, rows: Vec<Element<'a, Msg>>) -> Element<'a, Msg> {
        // Hand-build what libcosmic's `From<Section>` produces — a heading above the
        // rows — but with EACH ROW painting its own fill in the shared PILL MATERIAL
        // (DRAGON-279, user 2026-07-19: rows/buttons/nav pill are ONE material,
        // `theme::pill_fill` at `PILL_ALPHA`), joined by 1px genuinely-TRANSPARENT
        // seams so the window backdrop shows between rows (a same-fill gap inside one
        // painted card was invisible — the user wants visible separation, no drawn
        // line). Per-item presentation matches `ListColumn::into_element` exactly
        // (min-height-32 `content_row` + `space_xxs`/`space_m` padding); the heading
        // sits on the bare backdrop like Win11's group titles.
        let sp = cosmic::theme::spacing();
        let item_padding: cosmic::iced::Padding = [sp.space_xxs, sp.space_m].into();
        let n = rows.len();
        let wrapped: Vec<Element<'a, Msg>> = rows
            .into_iter()
            .enumerate()
            .map(|(i, content)| {
                // `ListColumn`'s `content_row`: the row content beside a 32px-tall spacer
                // (min row height), vertically centred, then the item padding.
                let row = widget::row(vec![
                    widget::container(content).width(Length::Fill).into(),
                    widget::space::vertical().height(Length::Fixed(32.0)).into(),
                ])
                .align_y(Alignment::Center)
                .padding(item_padding)
                .width(Length::Fill);
                // Group-aware rounding (user 2026-07-19): only the group's OUTER corners
                // curve — first row rounds its top pair, last row its bottom pair,
                // middle rows stay square — so the stack reads as one card sliced by
                // the transparent seams (the Win11 grouped-row shape).
                let (first, last) = (i == 0, i + 1 == n);
                widget::container(row)
                    .class(cosmic::theme::Container::custom(move |theme| {
                        let r = theme.cosmic().corner_radii.radius_s[0];
                        let (top, bottom) = (if first { r } else { 0.0 }, if last { r } else { 0.0 });
                        cosmic::iced::widget::container::Style {
                            background: Some(Background::Color(theme::pill_fill(
                                theme,
                                theme::PILL_ALPHA,
                            ))),
                            border: Border {
                                radius: [top, top, bottom, bottom].into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    }))
                    .into()
            })
            .collect();
        let list = widget::column(wrapped).width(Length::Fill).spacing(1.0);
        widget::column(vec![widget::text::heading(title).into(), list.into()])
            .spacing(8.0)
            .into()
    }

    /// Activate the nav entry for `tab` programmatically (flows that jump the
    /// user to a page, e.g. the update dialog's Update Now landing on About so
    /// the install progress is visible).
    pub(in crate::app) fn activate_config_tab(&mut self, tab: ConfigTab) {
        let entity = self
            .settings
            .nav
            .iter()
            .find(|e| self.settings.nav.data::<ConfigTab>(*e).copied() == Some(tab));
        if let Some(e) = entity {
            self.settings.nav.activate(e);
        }
    }

    fn page_name(tab: ConfigTab) -> &'static str {
        match tab {
            ConfigTab::General => "General",
            ConfigTab::CaptureModes => "Capture Modes",
            ConfigTab::AudioVideo => "Audio & Video",
            ConfigTab::Shortcuts => "Keyboard Shortcuts",
            ConfigTab::Health => "Health",
            ConfigTab::About => "About",
        }
    }

    fn page_icon(tab: ConfigTab) -> &'static str {
        match tab {
            ConfigTab::General => "preferences-system-symbolic",
            ConfigTab::CaptureModes => "accessories-screenshot-symbolic",
            ConfigTab::AudioVideo => "applications-multimedia-symbolic",
            ConfigTab::Shortcuts => "input-keyboard-symbolic",
            ConfigTab::Health => "utilities-system-monitor-symbolic",
            ConfigTab::About => "help-about-symbolic",
        }
    }

    /// A settings in-page tab strip (DRAGON-138/140): a native horizontal `tab_bar`
    /// over a `SingleSelectModel`, with a small gap between each tab's icon and its
    /// label (`button_spacing` — its default is 0, which jams them together).
    /// `on_activate` routes the selection to its settings-domain message. Shared by
    /// the General (Settings/Appearance) and Capture Modes (Scanner/Screenshots/
    /// Screen Recordings) pages.
    fn tab_strip<'a>(
        &self,
        model: &'a widget::segmented_button::SingleSelectModel,
        on_activate: fn(widget::segmented_button::Entity) -> Msg,
    ) -> Element<'a, Msg> {
        let gap = cosmic::theme::active().cosmic().space_xxs();
        crate::widgets::arrow_cursor::arrow_cursor(
            widget::tab_bar::horizontal(model)
                .button_alignment(Alignment::Center)
                .button_spacing(gap)
                .on_activate(on_activate),
        )
    }

    /// The hand-rolled custom-accent colour-picker sidebar (DRAGON-139) — a
    /// fixed-width panel holding libcosmic's full `ColorPickerModel` builder
    /// (hex/RGB input, hue slider, recent colours, Apply / Use-default / Cancel).
    /// Rendered inline in `config_window_view` because the framework context drawer
    /// doesn't paint on this secondary window on our rev. Mirrors cosmic-settings'
    /// `color_picker_context_view` panel shape.
    fn accent_picker_panel(&self) -> Element<'_, Msg> {
        let picker = self
            .settings
            .accent_picker
            .builder(|u| Msg::Settings(SettingsMsg::AccentPicker(u)))
            .reset_label("Use theme default")
            .save_label("Apply")
            .cancel_label("Cancel")
            .width(Length::Fixed(248.0))
            .height(Length::Fixed(158.0))
            .build("Recent colors", "Copy to clipboard", "Copied to clipboard");
        let inner = widget::column(vec![
            widget::text::title4("Custom Accent").into(),
            widget::text::caption("Set an exact accent colour by hex or RGB.").into(),
            widget::container(picker).center_x(Length::Fill).into(),
        ])
        .spacing(12.0)
        .width(Length::Fill);
        widget::container(inner)
            .width(Length::Fixed(280.0))
            .height(Length::Fill)
            .padding(16.0)
            .class(cosmic::theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(cosmic.primary.base.into())),
                    border: Border {
                        radius: theme::rounding(theme).s.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }))
            .into()
    }

    /// The window-capture border colour-picker sidebar (DRAGON-191): the same
    /// hand-rolled panel shape as [`Self::accent_picker_panel`], but driving the border
    /// picker model. The Active border's Reset means "follow the accent", the Inactive
    /// border's Reset restores its default colour (handled in `update_settings`).
    fn border_picker_panel(&self) -> Element<'_, Msg> {
        let (title, reset_label) = match self.settings.border_editor {
            Some(crate::app::BorderColorTarget::Inactive) => {
                ("Inactive Border Color", "Reset to default")
            }
            // Active (or, defensively, closed): Reset falls back to following the accent.
            _ => ("Active Border Color", "Follow accent color"),
        };
        let picker = self
            .settings
            .border_picker
            .builder(|u| Msg::Settings(SettingsMsg::BorderColorPicker(u)))
            .reset_label(reset_label)
            .save_label("Apply")
            .cancel_label("Cancel")
            .width(Length::Fixed(248.0))
            .height(Length::Fixed(158.0))
            .build("Recent colors", "Copy to clipboard", "Copied to clipboard");
        let inner = widget::column(vec![
            widget::text::title4(title).into(),
            widget::text::caption("Set an exact border colour by hex or RGB.").into(),
            widget::container(picker).center_x(Length::Fill).into(),
        ])
        .spacing(12.0)
        .width(Length::Fill);
        widget::container(inner)
            .width(Length::Fixed(280.0))
            .height(Length::Fill)
            .padding(16.0)
            .class(cosmic::theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(cosmic.primary.base.into())),
                    border: Border {
                        radius: theme::rounding(theme).s.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }))
            .into()
    }

    /// The section/item specs for a page (the single source of both the normal
    /// page view and the global search results).
    fn sections_for(&self, tab: ConfigTab) -> Vec<SectionSpec<'_>> {
        match tab {
            ConfigTab::General => self.general_sections(),
            // Concatenate the three former pages so the global search surfaces every
            // Capture Modes item under one header, regardless of the active in-page tab
            // (DRAGON-140; mirrors `general_sections`).
            ConfigTab::CaptureModes => {
                let mut secs = self.scanner_sections();
                secs.extend(self.screenshots_sections());
                secs.extend(self.recordings_sections());
                secs
            }
            // Concatenate the two former pages so the global search surfaces every
            // Audio & Video item under one header, regardless of the active in-page tab
            // (DRAGON-141; mirrors `general_sections`).
            ConfigTab::AudioVideo => {
                let mut secs = self.audio_sections();
                secs.extend(self.video_sections());
                secs
            }
            // All shortcut sections, regardless of the active in-page tab (DRAGON-142)
            // — the global search surfaces every binding under one header.
            ConfigTab::Shortcuts => self.keyboard_sections(),
            ConfigTab::Health => self.health_sections(),
            ConfigTab::About => self.about_sections(),
        }
    }
}

/// Style for a collapsed-rail icon button, matching the expanded nav's selected
/// look: a `neutral_5 @ alpha` background + accent icon when active.
fn rail_style(active: bool, hovered: bool, theme: &cosmic::Theme) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let mut s = cosmic::widget::button::Style::new();
    // The button token — matches libcosmic's own icon buttons.
    s.border_radius = theme::rounding(theme).xl.into();
    let bg_alpha = if active {
        0.2
    } else if hovered {
        0.1
    } else {
        0.0
    };
    if bg_alpha > 0.0 {
        let mut c: cosmic::iced::Color = cosmic.palette.neutral_5.into();
        c.a = bg_alpha;
        s.background = Some(Background::Color(c));
    }
    s.icon_color = Some(if active {
        theme::accent_text(theme)
    } else {
        cosmic.on_bg_color().into()
    });
    s
}

/// The button class for a collapsed-rail icon (active page = accent + highlight).
fn rail_button_class(active: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| rail_style(active, false, theme)),
        hovered: Box::new(move |_focused, theme| rail_style(active, true, theme)),
        pressed: Box::new(move |_focused, theme| rail_style(active, true, theme)),
        disabled: Box::new(move |theme| rail_style(active, false, theme)),
    }
}

/// Like `rail_button_class`, but tints the icon to a [`Severity`] colour so the
/// collapsed rail's Health entry shows green/amber/red at a glance.
fn health_rail_button_class(sev: Severity, active: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| health_rail_style(sev, active, false, theme)),
        hovered: Box::new(move |_focused, theme| health_rail_style(sev, active, true, theme)),
        pressed: Box::new(move |_focused, theme| health_rail_style(sev, active, true, theme)),
        disabled: Box::new(move |theme| health_rail_style(sev, active, false, theme)),
    }
}

/// `rail_style` with the icon recoloured to the severity colour (the rail's normal
/// background/active highlight is kept).
fn health_rail_style(
    sev: Severity,
    active: bool,
    hovered: bool,
    theme: &cosmic::Theme,
) -> cosmic::widget::button::Style {
    let mut s = rail_style(active, hovered, theme);
    s.icon_color = Some(sev.color(theme));
    s
}

/// Like `rail_button_class`, but tints the icon to the theme success colour so the
/// collapsed rail's About entry lights up green when an update is available
/// (DRAGON-175). The active-page highlight is kept.
fn about_rail_button_class(active: bool) -> cosmic::theme::Button {
    fn style(active: bool, hovered: bool, theme: &cosmic::Theme) -> cosmic::widget::button::Style {
        let mut s = rail_style(active, hovered, theme);
        s.icon_color = Some(theme::success(theme));
        s
    }
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| style(active, false, theme)),
        hovered: Box::new(move |_focused, theme| style(active, true, theme)),
        pressed: Box::new(move |_focused, theme| style(active, true, theme)),
        disabled: Box::new(move |theme| style(active, false, theme)),
    }
}

