//! Portable recording-control UI model shared by every control surface (DRAGON-172,
//! prep for DRAGON-173).
//!
//! ## Why this exists
//!
//! The in-recording controls appear in several places that MUST stay identical:
//!
//! * the macOS resident daemon's menu-bar item (`crate::daemon`),
//! * the macOS per-recording own status item (`crate::tray` = `platform/mac/tray.rs`),
//! * the Linux recording tray (`crate::tray`),
//! * and — coming in DRAGON-173 — a Linux resident tray process with the same full
//!   menu + three-state icon.
//!
//! Each backend renders differently (objc2 `NSMenu` selectors, `ksni` closures), so the
//! rendering can't be shared — but the DECISIONS can and must be, or the behaviour
//! drifts. This module is the ONE dependency-free, portable source for:
//!
//! * the three-state menu-bar / tray icon geometry (corner brackets; + centre dot while
//!   recording; + centre pause bars while paused) and the [`IconState`] choice,
//! * the Pause/Resume label,
//! * and the WHOLE tray menu MODEL (`tray_menu`, DRAGON-574) — the ordered list of rows
//!   for BOTH states (idle and recording), their labels, icons, submenus, radio groups
//!   and per-state disabled flags — as data each backend maps onto its widgets.
//!
//! It is NOT `cfg`-gated; the wire protocol + socket plumbing that carry the recording
//! state between the daemon and a child stay in `crate::daemon_ipc` (its pure parts
//! reuse this module, its socket is platform-gated).

/// The daemon / tray icon state, decided PURELY from the reported recording state.
/// Not recording: the plain corner brackets (idle). Recording: brackets plus a solid
/// centre dot. Recording but paused: brackets plus centre pause bars. A tiny enum so
/// the decision is unit-testable without any rasterizer or AppKit and the drawing code
/// has one switch.
//
// The three-state icon model + SVGs (this enum through `icon_svg`) are consumed by the
// Linux and macOS tray/daemon status items only; Windows ships no tray/daemon, so they're
// dead in the Windows bin build (the pure tests below still exercise them). DRAGON-229.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconState {
    /// Corner brackets only (no recording in progress).
    Idle,
    /// Corner brackets plus a solid centre dot (a recording is in progress).
    Recording,
    /// Corner brackets plus centre pause bars (a recording is in progress but paused).
    Paused,
}

/// The icon state for a `(recording, paused)` pair (see [`IconState`]). A paused
/// recording is still "in progress", so it maps to [`IconState::Paused`], not `Idle`.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))] // Linux/mac tray/daemon only (DRAGON-229)
pub fn icon_state(recording: bool, paused: bool) -> IconState {
    match (recording, paused) {
        (false, _) => IconState::Idle,
        (true, false) => IconState::Recording,
        (true, true) => IconState::Paused,
    }
}

/// The Pause/Resume menu-item label for a paused/live recording (no em/en-dashes,
/// project rule). One source shared by every renderer.
pub fn pause_label(paused: bool) -> &'static str {
    if paused {
        "Resume Recording"
    } else {
        "Pause Recording"
    }
}

// ── Three-state icon (corner brackets; + centre dot; + centre pause bars) ─────
//
// The viewfinder corner brackets draw at all times (the idle state). While a recording
// is in progress a solid CENTRE DOT is added inside them; while that recording is paused
// the dot becomes CENTRE PAUSE BARS — so all three states read at a glance. The bracket
// geometry is IDENTICAL between states so the icon never shifts; only the centre glyph
// changes. macOS renders these as template images (only the alpha matters — AppKit tints
// them to the menu bar); the Linux tray tints them with the accent colour. The fill in
// the SVG is opaque black so a template render keeps full alpha.

/// The idle icon: the viewfinder corner brackets only (one path, four subpaths; each
/// corner is the rounded-rect's 4.5-radius arc plus a 1px straight stub on both sides,
/// so the curved corners survive with the edges cut away).
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))] // Linux/mac tray/daemon only (DRAGON-229)
pub const ICON_BORDER: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M3.2 8.7 L3.2 7.7 A4.5 4.5 0 0 1 7.7 3.2 L8.7 3.2 M15.3 3.2 L16.3 3.2 A4.5 4.5 0 0 1 20.8 7.7 L20.8 8.7 M20.8 15.3 L20.8 16.3 A4.5 4.5 0 0 1 16.3 20.8 L15.3 20.8 M8.7 20.8 L7.7 20.8 A4.5 4.5 0 0 1 3.2 16.3 L3.2 15.3" fill="none" stroke="#000" stroke-width="2.2" stroke-linecap="round"/></svg>"##;

/// The recording icon: the same corner brackets plus a solid centre dot.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))] // Linux/mac tray/daemon only (DRAGON-229)
pub const ICON_BORDER_DOT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M3.2 8.7 L3.2 7.7 A4.5 4.5 0 0 1 7.7 3.2 L8.7 3.2 M15.3 3.2 L16.3 3.2 A4.5 4.5 0 0 1 20.8 7.7 L20.8 8.7 M20.8 15.3 L20.8 16.3 A4.5 4.5 0 0 1 16.3 20.8 L15.3 20.8 M8.7 20.8 L7.7 20.8 A4.5 4.5 0 0 1 3.2 16.3 L3.2 15.3" fill="none" stroke="#000" stroke-width="2.2" stroke-linecap="round"/><circle cx="12" cy="12" r="5" fill="#000"/></svg>"##;

/// The paused icon: the same corner brackets plus centre pause bars (sized to the same
/// visual weight as the recording dot).
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))] // Linux/mac tray/daemon only (DRAGON-229)
pub const ICON_BORDER_PAUSE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M3.2 8.7 L3.2 7.7 A4.5 4.5 0 0 1 7.7 3.2 L8.7 3.2 M15.3 3.2 L16.3 3.2 A4.5 4.5 0 0 1 20.8 7.7 L20.8 8.7 M20.8 15.3 L20.8 16.3 A4.5 4.5 0 0 1 16.3 20.8 L15.3 20.8 M8.7 20.8 L7.7 20.8 A4.5 4.5 0 0 1 3.2 16.3 L3.2 15.3" fill="none" stroke="#000" stroke-width="2.2" stroke-linecap="round"/><rect x="8.1" y="7.2" width="2.7" height="9.6" rx="1.35" fill="#000"/><rect x="13.2" y="7.2" width="2.7" height="9.6" rx="1.35" fill="#000"/></svg>"##;

/// The SVG source for a given icon state (see [`IconState`]). Pure so the three-state
/// choice is unit-testable without any rasterizer or AppKit.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))] // Linux/mac tray/daemon only (DRAGON-229)
pub fn icon_svg(state: IconState) -> &'static str {
    match state {
        IconState::Idle => ICON_BORDER,
        IconState::Recording => ICON_BORDER_DOT,
        IconState::Paused => ICON_BORDER_PAUSE,
    }
}

/// The RED used for the centre glyph of an in-progress recording, paused or not. Apple's
/// system red, which reads as "recording" on every backdrop and is the same value the
/// recording chrome uses elsewhere.
#[cfg_attr(target_os = "macos", allow(dead_code))] // tinted trays only; mac uses template images
pub const RECORDING_RED: &str = "#ff3b30";

/// [`RECORDING_RED`] as channel bytes, for the rasterizers that tint by RGB values
/// rather than by string substitution: the Linux countdown tray's digit faces
/// (DRAGON-563) and, through [`menu_icon_tint`], the "Cancel & Delete Recording"
/// menu icon on every platform (the destructive control; the owner's recolour round
/// narrowed the once-uniform red family to it alone). Pinned to the hex form by a
/// test so the two spellings can never drift.
pub const RECORDING_RED_RGB: [u8; 3] = [0xff, 0x3b, 0x30];

/// The SUCCESS GREEN the "Finish & Save Recording" menu icon rasterizes in, as
/// channel bytes: the app's ONE canonical success colour
/// (`crate::app::theme::SUCCESS`, the same value the upload meter turns on
/// completion), spelled as bytes here because the menu rasterizers tint by RGB and
/// the tray daemons deliberately carry no iced stack. Pinned to the theme constant
/// by a test (`success_green_bytes_are_the_theme_success_colour`) so the two
/// spellings can never drift.
pub const SUCCESS_GREEN_RGB: [u8; 3] = [0x5c, 0xcc, 0x73];

/// The countdown tray item's one menu entry (DRAGON-563): cancel the pre-capture
/// countdown. The `Cancel <thing>` shape the upload counter's "Cancel upload" set
/// (`cloud::upload::tray::CANCEL_LABEL`). ONE source for the Linux ksni item, the mac
/// menu-bar item and the Windows notification-area icon. Pure data, pinned by a test;
/// no em/en-dashes.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))] // countdown tray platforms (DRAGON-563)
pub const COUNTDOWN_CANCEL_LABEL: &str = "Cancel countdown";

/// **Pure**, unit-tested: the countdown tray item's title/tooltip for `remaining`
/// seconds (DRAGON-563). One builder shared by all three platform items so a panel that
/// shows either says the same thing, the rule the upload counter's `tooltip` set. No
/// em/en-dashes.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))] // countdown tray platforms (DRAGON-563)
pub fn countdown_tooltip(remaining: u8) -> String {
    format!("Capture starts in {remaining}s")
}

/// [`icon_svg`] recoloured for a TINTED tray, in the two roles the glyph actually has.
/// Pure, unit-tested, and shared by every tinting surface so they cannot drift.
///
/// * The viewfinder BRACKETS are always the app's resolved ACCENT. They are chrome: they mean
///   the same thing in every state, so they look the same in every state.
/// * The CENTRE GLYPH is [`RECORDING_RED`] whenever a recording exists, which is the dot while
///   running AND the bars while paused. A paused recording is still a recording: it holds the
///   capture connection, the file is open, and there is something to come back to. Shape (bars
///   vs dot) is what distinguishes the two, which is what the shared geometry was for.
///
/// One pair of replacements covers both glyphs because the templates are built for it:
/// `stroke` is the brackets and `fill` is the centre in every one of them.
///
/// **macOS deliberately does not use this.** Its menu-bar item renders these as TEMPLATE
/// images, where only the alpha matters because AppKit tints them to the menu bar; it has no
/// colour of its own to set, which is why this is dead there and says so.
///
/// This lived in `platform/windows/daemon.rs` until DRAGON-541, when the Linux trays were
/// found to be tinting the WHOLE glyph with the accent — brackets and centre alike — so a
/// recording never read red on Linux and the state that most needs to stand out was the one
/// that blended into its own chrome. Moving the rule into the shared tree rather than copying
/// it into the Linux plugin is what keeps the two from diverging again.
#[cfg_attr(target_os = "macos", allow(dead_code))] // tinted trays only; mac uses template images
pub fn tray_icon_svg(state: IconState, accent: [u8; 3]) -> String {
    let base = icon_svg(state);
    let hex = format!("#{:02x}{:02x}{:02x}", accent[0], accent[1], accent[2]);
    match state {
        // Nothing in the centre to colour: the idle icon is brackets only.
        IconState::Idle => base.replace("#000", &hex),
        IconState::Recording | IconState::Paused => base
            .replace(r##"stroke="#000""##, &format!(r##"stroke="{hex}""##))
            .replace(r##"fill="#000""##, &format!(r##"fill="{RECORDING_RED}""##)),
    }
}

// ── In-recording menu model ──────────────────────────────────────────────────
//
// The ordered list of in-recording controls, as DATA. Each control surface (macOS
// daemon NSMenu, macOS own status item, Linux ksni tray, and the future Linux resident
// tray) maps this onto its own widget/selector plumbing — the labels, order, checkmarks,
// and which action each fires are decided HERE so they can never drift between surfaces.

/// One in-recording control action. The discriminant is what the surface wires to its
/// click handler (each maps 1:1 onto `tray::TrayEvent` / `daemon_ipc::Command`).
//
// The menu MODEL below is consumed by the Linux `ksni` tray today and, in DRAGON-173, by
// the Linux resident process; macOS builds it via per-selector NSMenu items, so on macOS
// these types have no consumer yet. Allow the dead-code lint there rather than gate the
// whole model per-platform (it is portable by design).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingAction {
    /// Pause the recording, or resume it when paused.
    TogglePause,
    /// Toggle the microphone's live arm. Since DRAGON-558 this is fired by an "Audio
    /// Recording" radio pick's toggle diff while a recording is in progress (see
    /// [`audio_toggles_are_live`] / [`audio_pick_live_actions`]), not by a recording-menu
    /// item of its own.
    ToggleMic,
    /// Toggle the system audio's live arm (same routing as [`RecordingAction::ToggleMic`]).
    ToggleSystemAudio,
    /// Finish (stop + save) the recording.
    Stop,
    /// Cancel the recording and discard its file.
    Cancel,
}

/// How a menu item presents: a plain activatable item, a checkmark item (with its
/// checked state), or a separator (no action).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItemKind {
    /// A plain activatable item firing [`RecordingItem::action`].
    Standard,
    /// A checkmark item; `bool` is whether it is currently checked.
    //
    // NO model emits this since DRAGON-558 moved the audio toggles (the only checkmark
    // items) into the "Audio Recording" radio submenu, so it is dead on every platform.
    // It stays: it is menu VOCABULARY every renderer still carries a match arm for (two of
    // those renderers, mac daemon + mac child tray, are compile-verifiable only on a Mac),
    // and a checkmark row is the likeliest kind to return. Deleting it would buy nothing
    // and cost blind arm-shedding edits in the closed platform files.
    #[allow(dead_code)]
    Checkmark(bool),
    /// A visual separator (no label, no action).
    //
    // NO model emits this since DRAGON-574 moved every separator to the [`TrayItem`]
    // level (the control group never carried one anyway). It stays for the same reason
    // `Checkmark` does: it is menu VOCABULARY every renderer still carries a match arm
    // for, two of those renderers compile-verifiable only on a Mac.
    #[allow(dead_code)]
    Separator,
}

/// One item in the in-recording menu MODEL: its display label, how it presents, and the
/// action it fires. A separator (none in the model today; renderers still handle the kind)
/// carries an empty label and `RecordingAction::Stop` as an inert placeholder (never
/// wired — the kind is `Separator`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingItem {
    /// The menu label (empty for a separator). Never contains em/en-dashes.
    pub label: &'static str,
    /// How the item presents (standard / checkmark+state / separator).
    pub kind: MenuItemKind,
    /// The action the item fires when activated (unused for a separator).
    pub action: RecordingAction,
    /// The control's icon. Its colour follows the owner's three-way rule
    /// ([`menu_icon_fixed_tint`]): Finish success-green, Discard recording-red,
    /// Pause / Resume the ordinary accent, identical on every surface. `None` only
    /// for a separator.
    pub icon: Option<MenuIcon>,
}

/// The in-recording control menu MODEL, in display order, for the given state. ONE
/// source for every surface: Pause/Resume Recording, Finish & Save Recording, Cancel &
/// Delete Recording. This is the CONTROLS block of the in-recording dropdown, sitting
/// right under the leading Audio Recording submenu; the full top-level shape (audio
/// submenu, then the controls, then the shared launcher group with its per-state
/// disabled flags) is [`tray_menu`] (DRAGON-574) — the mac daemon, the Linux
/// resident, the Linux recording tray, the mac child's own NSStatusItem and the Windows
/// daemon are all identical while recording. No per-surface parameterization: the model
/// emits one structure and every renderer renders all of it.
///
/// The two audio toggles lived HERE from DRAGON-173 until DRAGON-558 ("Toggle Microphone"
/// / "Toggle System Audio", checkmarked, above a separator). The owner moved them into
/// the always-present "Audio Recording: <state>" radio submenu (in the launcher group
/// while idle, leading the whole menu while recording; see
/// [`tray_menu`] and [`AudioArmState`]) so the control exists while IDLE too, arming
/// the persisted defaults a new recording reads at start; while recording a radio pick
/// still becomes [`RecordingAction::ToggleMic`] / [`RecordingAction::ToggleSystemAudio`]
/// (the routing decision is [`audio_toggles_are_live`], the diff is
/// [`audio_pick_live_actions`]). Do not re-add audio items here: that would put the same
/// control in two places of one menu.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn recording_menu(paused: bool) -> Vec<RecordingItem> {
    vec![
        RecordingItem {
            label: pause_label(paused),
            kind: MenuItemKind::Standard,
            action: RecordingAction::TogglePause,
            // The icon flips with the label: `play` is the universal resume glyph, so
            // a paused recording offers a play mark, not a second pause mark.
            icon: Some(if paused { MenuIcon::Resume } else { MenuIcon::Pause }),
        },
        RecordingItem {
            label: "Finish & Save Recording",
            kind: MenuItemKind::Standard,
            action: RecordingAction::Stop,
            icon: Some(MenuIcon::Finish),
        },
        RecordingItem {
            label: "Cancel & Delete Recording",
            kind: MenuItemKind::Standard,
            action: RecordingAction::Cancel,
            icon: Some(MenuIcon::Discard),
        },
    ]
}

// ── The ONE tray menu model (DRAGON-574) ─────────────────────────────────────
//
// The owner's restructure. ONE portable function, [`tray_menu`], emits the WHOLE tray
// dropdown for both states, and every control surface (the mac daemon, the Linux
// resident, the Linux recording tray, the mac child's own NSStatusItem, the Windows
// daemon) walks it top to bottom. The launchers moved into nested "Capture" / "Record"
// submenus, the pre-capture countdown gained its own "Countdown Timer: NN" radio submenu
// (writing the SAME persisted `delay_idx` every capture launch reads), and the
// while-recording menu is the SAME launcher group with per-state disabled flags plus the
// Audio Recording submenu and the three controls on top — there is no separate
// recording-time "Capture Menu" submenu any more.
//
// This deliberately supersedes the DRAGON-558/559 shapes (`capture_menu`,
// `recording_capture_menu`, `RECORDING_MENU_SECTIONS`, `CAPTURE_MENU_LABEL`, the
// AudioArms row-anchor and the separator markers that carried it). Those existed because
// the idle menu was FLAT and the recording menu nested a slimmed copy of it; with one
// model emitting both states, the anchors and the filter had nothing left to mark. macOS
// keys its "Manage Permissions" insertion point on the Settings ACTION row now (directly
// below Settings, as before).

/// Optional per-item ICON identity (DRAGON-574). Names a shipped glyph, never pixels:
/// each platform rasterizes + tints [`MenuIcon::svg`] through its own machinery (ksni
/// `icon_data` PNGs on Linux, per-item bitmaps on Windows, template `NSImage`s on macOS),
/// and a host that does not render menu icons simply ignores them. RADIO rows carry NO
/// icon on purpose: the toggle indicator and the icon fight for the same slot on several
/// hosts.
///
/// Every variant reuses an asset the app already ships for the SAME meaning (owner's
/// mapping): the settings tabs' glyphs for Capture / Record / Audio, the General page's
/// gear for Settings, the overlay toolbar's target glyphs for Region / Window / Monitor,
/// the scanner kind's glyph for Scanner, and Lucide's `timer` (vendored for this feature)
/// for the Countdown Timer. `key` (owner-picked, vendored the same way) is for the
/// macOS-only Manage Permissions item.
///
/// The RECORDING-CONTROL variants carry the owner's three-way colour rule
/// ([`menu_icon_fixed_tint`]): Finish rasterizes in the app's success green
/// ([`SUCCESS_GREEN_RGB`]), Discard in [`RECORDING_RED_RGB`], and Pause / Resume are
/// ordinary accent-tinted rows (macOS renders the two fixed-tint icons as
/// NON-template images so the colour survives AppKit's appearance tinting, and
/// Pause / Resume as templates like every other row). `pause`, `play` and `save`
/// were already shipped by the preview/player UI; `trash` is vendored for this
/// feature like `timer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuIcon {
    /// Lucide `view` — the scanner kind's glyph (`document-properties-symbolic`), the
    /// same asset the toolbar's scan segment and the settings Scanner tab use.
    Scanner,
    /// Lucide `pipette` — the colour picker tool (DRAGON-582), the glyph the owner
    /// named. Vendored for this feature like `timer` and `trash`; see
    /// `res/icons/ATTRIBUTION.md`.
    ColorPicker,
    /// Lucide `camera` — the settings window's Screenshots tab glyph
    /// (`camera-photo-symbolic`).
    Capture,
    /// Lucide `video` — the settings window's Screen Recordings tab glyph
    /// (`camera-video-symbolic`).
    Record,
    /// Lucide `timer` — owner-picked for the Countdown Timer submenu (vendored by
    /// DRAGON-574; see `res/icons/ATTRIBUTION.md`).
    Countdown,
    /// Lucide `audio-lines` — the settings window's Audio tab glyph
    /// (`audio-x-generic-symbolic`).
    Audio,
    /// Lucide `settings` — the gear the settings window's General page uses
    /// (`preferences-system-symbolic`).
    Settings,
    /// Lucide `x` — the app's close glyph (`window-close-symbolic`).
    Quit,
    /// Lucide `crop` — the overlay toolbar's Region target glyph
    /// (`screenshot-selection-symbolic`). Shared by Capture Region AND Record Region:
    /// the target is the icon's meaning, the verb comes from the submenu.
    Region,
    /// Lucide `app-window` — the toolbar's Window target glyph
    /// (`screenshot-window-symbolic`).
    Window,
    /// Lucide `monitor` — the toolbar's Monitor target glyph
    /// (`screenshot-screen-symbolic`).
    Monitor,
    /// Lucide `pause` — the "Pause Recording" control (an ordinary accent-tinted row;
    /// the same glyph the preview player's pause button uses).
    Pause,
    /// Lucide `play` — the "Resume Recording" control (ordinary tint, like Pause).
    /// Play is the universal resume glyph, so the paused state does not re-show a
    /// pause mark; the icon flips with the label ([`recording_menu`]).
    Resume,
    /// Lucide `save` — the "Finish & Save Recording" control (fixed
    /// [`SUCCESS_GREEN_RGB`], the app's success colour).
    Finish,
    /// Lucide `trash` — the "Cancel & Delete Recording" control (fixed
    /// [`RECORDING_RED_RGB`], the destructive one; vendored from upstream for this
    /// feature, like `timer`).
    Discard,
    /// Lucide `key`, the macOS-only "Manage Permissions" tray item (DRAGON-412);
    /// vendored from upstream for this feature, like `timer` and `trash`. Ordinary
    /// accent-tinted row, not a fixed-tint one.
    ///
    /// Dead off macOS: unlike every other variant here, this one is never built
    /// through the portable `tray_menu` model (TCC has no Linux/Windows
    /// equivalent, so it is woven directly into the mac daemon's own menu build
    /// in `platform/mac/daemon.rs`; see that call site's comment), so nothing
    /// constructs it on those platforms.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Permissions,
}

impl MenuIcon {
    /// The vendored Lucide SVG source for this icon (24-unit viewBox, `currentColor`
    /// strokes — the whole set's house format). Embedded here rather than through
    /// `widgets::icons` because the tray daemons deliberately carry no iced stack.
    pub fn svg(self) -> &'static str {
        match self {
            MenuIcon::Scanner => include_str!("../res/icons/lucide/view.svg"),
            MenuIcon::ColorPicker => include_str!("../res/icons/lucide/pipette.svg"),
            MenuIcon::Capture => include_str!("../res/icons/lucide/camera.svg"),
            MenuIcon::Record => include_str!("../res/icons/lucide/video.svg"),
            MenuIcon::Countdown => include_str!("../res/icons/lucide/timer.svg"),
            MenuIcon::Audio => include_str!("../res/icons/lucide/audio-lines.svg"),
            MenuIcon::Settings => include_str!("../res/icons/lucide/settings.svg"),
            MenuIcon::Quit => include_str!("../res/icons/lucide/x.svg"),
            MenuIcon::Region => include_str!("../res/icons/lucide/crop.svg"),
            MenuIcon::Window => include_str!("../res/icons/lucide/app-window.svg"),
            MenuIcon::Monitor => include_str!("../res/icons/lucide/monitor.svg"),
            MenuIcon::Pause => include_str!("../res/icons/lucide/pause.svg"),
            MenuIcon::Resume => include_str!("../res/icons/lucide/play.svg"),
            MenuIcon::Finish => include_str!("../res/icons/lucide/save.svg"),
            MenuIcon::Discard => include_str!("../res/icons/lucide/trash.svg"),
            MenuIcon::Permissions => include_str!("../res/icons/lucide/key.svg"),
        }
    }
}

/// **Pure**, unit-tested: the FIXED tint a menu icon always rasterizes in regardless
/// of the surface accent, or `None` for an ordinary row (accent-tinted; rendered as
/// a black TEMPLATE image on macOS, where AppKit owns the tint). The owner's
/// three-way rule, which replaced the uniform RECORDING-RED control family:
///
/// * "Finish & Save Recording" ([`MenuIcon::Finish`]) is [`SUCCESS_GREEN_RGB`], the
///   app's one success colour: finishing is the productive keep-my-file action.
/// * "Cancel & Delete Recording" ([`MenuIcon::Discard`]) stays
///   [`RECORDING_RED_RGB`]: it is the destructive control.
/// * Pause and its Resume flip are ORDINARY rows again: they act on the live
///   recording but destroy nothing, so they read like every other menu icon.
///
/// On macOS a fixed-tint icon must ALSO skip the template mark, or AppKit's
/// appearance tinting discards the colour; the mac walkers branch on
/// `menu_icon_fixed_tint(icon).is_some()` for exactly that (Pause / Resume are back
/// on the template path there).
pub fn menu_icon_fixed_tint(icon: MenuIcon) -> Option<[u8; 3]> {
    match icon {
        MenuIcon::Finish => Some(SUCCESS_GREEN_RGB),
        MenuIcon::Discard => Some(RECORDING_RED_RGB),
        _ => None,
    }
}

/// **Pure**, unit-tested: the tint a menu icon rasterizes with, given the surface's
/// accent. ONE rule for the Linux PNGs, the Windows menu bitmaps and the mac images
/// (mac passes black as its "accent" and marks an accent-tinted result a template):
/// a fixed-tint control keeps its own colour ([`menu_icon_fixed_tint`], the
/// three-way rule), everything else is the accent. Deciding this here rather than in
/// each walker is what keeps a control from rendering green on two platforms and
/// accent on the third.
pub fn menu_icon_tint(icon: MenuIcon, accent: [u8; 3]) -> [u8; 3] {
    menu_icon_fixed_tint(icon).unwrap_or(accent)
}

/// **Pure**, unit-tested: a [`MenuIcon`]'s SVG with its `currentColor` strokes replaced
/// by a concrete colour, ready for any rasterizer. The tinting surfaces (Linux ksni
/// pixmaps, Windows menu bitmaps) pass their accent; macOS passes black and marks the
/// image a TEMPLATE, where only the alpha matters. One substitution shared by all three
/// so a half-tinted icon cannot happen on just one platform.
pub fn menu_icon_svg_tinted(icon: MenuIcon, rgb: [u8; 3]) -> String {
    let hex = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
    icon.svg().replace("currentColor", &hex)
}

/// One capture-group action. Each renderer maps this onto its own spawn flag / selector
/// (the launchers spawn a capture child; Settings opens the settings window; Quit tears
/// the resident down). Enabled state comes from the [`TrayItem`] that carries it, never
/// from the renderer's own reading of the recording state.
//
// `AudioArms` left this enum in DRAGON-574: it was never an action, only a row anchor
// the old flat model used to mark where the audio submenu rendered. The submenu is a
// first-class [`TrayItem::Radio`] now, so the anchor had nothing left to mark.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureAction {
    /// The colour picker tool (DRAGON-582). Not a capture: it spawns a child that shows
    /// the dimmed magnifier overlay and opens the result window. It sits FIRST in the
    /// menu, before Scanner, which is the owner's placement.
    ColorPicker,
    /// Scanner (forces Region + scan).
    Scan,
    /// Region capture.
    Region,
    /// Window capture.
    Window,
    /// Monitor capture.
    Monitor,
    /// Region recording (DRAGON-559): the capture twin with the video kind. Spawns the
    /// SAME child as "Capture Region" plus `--video`, so a fallback (portal-frozen)
    /// session routes through the portal exactly like its capture twin.
    RecordRegion,
    /// Window recording (DRAGON-559; see [`CaptureAction::RecordRegion`]).
    RecordWindow,
    /// Monitor recording (DRAGON-559; see [`CaptureAction::RecordRegion`]).
    RecordMonitor,
    /// Open the settings window.
    Settings,
    /// Quit the daemon / resident (disabled while recording).
    Quit,
}

/// One activatable row of the tray menu: its label, the action it fires, and its
/// optional icon. Separators and submenus are [`TrayItem`] variants of their own since
/// DRAGON-574, so this is always a plain clickable row. Never contains em/en-dashes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureItem {
    /// The display label. Renderers may substitute their platform string for
    /// [`CaptureAction::Quit`] ("Quit Cosmic Capture Kit Tray" on the resident/daemon
    /// surfaces, "Quit Cosmic Capture Kit" on a child-owned icon) and for
    /// [`CaptureAction::Settings`] (macOS renders its native ellipsis).
    pub label: &'static str,
    /// The action the row fires when activated.
    pub action: CaptureAction,
    /// The row's icon, `None` where the owner's spec assigns none.
    pub icon: Option<MenuIcon>,
}

/// Which LAUNCHER submenu a [`TrayItem::Launchers`] row opens (DRAGON-574).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LauncherSubmenu {
    /// "Capture" — the three still-capture launchers. Fully enabled while recording
    /// (stills during a recording are fine).
    Capture,
    /// "Record" — the three recording launchers. Disabled while recording: a session
    /// records ONE child at a time (the resident's control socket serves one recording;
    /// a second would supersede the first's controls).
    Record,
}

impl LauncherSubmenu {
    /// The submenu row's label (the owner's verbatim wording).
    pub fn label(self) -> &'static str {
        match self {
            LauncherSubmenu::Capture => "Capture",
            LauncherSubmenu::Record => "Record",
        }
    }

    /// The submenu row's icon (the settings tabs' glyphs, owner's mapping).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // consumed natively off-Linux
    pub fn icon(self) -> MenuIcon {
        match self {
            LauncherSubmenu::Capture => MenuIcon::Capture,
            LauncherSubmenu::Record => MenuIcon::Record,
        }
    }
}

/// Which RADIO submenu a [`TrayItem::Radio`] row opens (DRAGON-574). The rows come from
/// the submenu's own builder ([`countdown_items`] / [`audio_arm_items`]) and its title
/// from the matching title builder ([`countdown_submenu_title`] /
/// [`audio_submenu_title`]), both fed with the surface's CURRENT state — that is why the
/// model carries only the kind, not the rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioSubmenu {
    /// "Countdown Timer: NN" — the four presets, writing the persisted `delay_idx`.
    /// Disabled while recording.
    Countdown,
    /// "Audio Recording: <state>" — the four complete arm states. Enabled while
    /// recording (a pick applies to the recording LIVE, see
    /// [`audio_toggles_are_live`]).
    Audio,
}

impl RadioSubmenu {
    /// The submenu row's icon (`timer` / the Audio settings tab's glyph).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // consumed natively off-Linux
    pub fn icon(self) -> MenuIcon {
        match self {
            RadioSubmenu::Countdown => MenuIcon::Countdown,
            RadioSubmenu::Audio => MenuIcon::Audio,
        }
    }
}

/// One row of the tray dropdown, as portable data (DRAGON-574). Renderers walk the
/// [`visible_tray_menu`] list and map each variant onto the widget they already have;
/// the `enabled` flags are part of the MODEL, so no surface re-derives them from the
/// recording state on its own. Since the hide round a false flag means the row is
/// OMITTED from the rendered menu, not grayed; [`visible_tray_menu`] carries the why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayItem {
    /// An in-recording control row ([`recording_menu`]'s vocabulary). Present only
    /// while recording.
    Control(RecordingItem),
    /// A plain activatable row; `enabled: false` HIDES the row (see
    /// [`visible_tray_menu`]).
    Action {
        /// The row (label / action / icon).
        item: CaptureItem,
        /// Whether the row is usable in this state (false = hidden when rendered).
        enabled: bool,
    },
    /// A nested submenu of launcher rows; `enabled: false` HIDES the whole submenu
    /// (see [`visible_tray_menu`]). Renderers may still disable rows defensively; a
    /// disabled parent never reaches them any more.
    Launchers {
        /// Which submenu (label + icon come from it).
        menu: LauncherSubmenu,
        /// Whether the submenu is usable in this state (false = hidden when rendered).
        enabled: bool,
        /// The submenu's rows, in display order.
        items: Vec<CaptureItem>,
    },
    /// A nested RADIO submenu (rows from its own builder; see [`RadioSubmenu`]).
    Radio {
        /// Which submenu (icon + row/title builders come from it).
        menu: RadioSubmenu,
        /// Whether the submenu is usable in this state (false = hidden when rendered).
        enabled: bool,
    },
    /// A visual separator.
    Separator,
}

/// The three Capture launcher rows (also the shape test's fixture). Split out of
/// [`tray_menu`] so the icon mapping is written once.
fn capture_launcher_items() -> Vec<CaptureItem> {
    vec![
        CaptureItem {
            label: "Capture Region",
            action: CaptureAction::Region,
            icon: Some(MenuIcon::Region),
        },
        CaptureItem {
            label: "Capture Window",
            action: CaptureAction::Window,
            icon: Some(MenuIcon::Window),
        },
        CaptureItem {
            label: "Capture Monitor",
            action: CaptureAction::Monitor,
            icon: Some(MenuIcon::Monitor),
        },
    ]
}

/// The three Record launcher rows. The icons are the SAME target glyphs as the capture
/// trio's (owner's rule: the target is the icon's meaning, the verb comes from the
/// submenu).
fn record_launcher_items() -> Vec<CaptureItem> {
    vec![
        CaptureItem {
            label: "Record Region",
            action: CaptureAction::RecordRegion,
            icon: Some(MenuIcon::Region),
        },
        CaptureItem {
            label: "Record Window",
            action: CaptureAction::RecordWindow,
            icon: Some(MenuIcon::Window),
        },
        CaptureItem {
            label: "Record Monitor",
            action: CaptureAction::RecordMonitor,
            icon: Some(MenuIcon::Monitor),
        },
    ]
}

/// **Pure**, unit-tested: THE tray dropdown, both states, one function (DRAGON-574, the
/// owner's restructure).
///
/// Idle (`recording == false`): Color Picker, Scanner, the "Capture" submenu, the "Record" submenu,
/// the "Countdown Timer: NN" radio submenu, the "Audio Recording: <state>" radio
/// submenu, a separator, "Settings...", Quit — everything enabled.
///
/// While recording: the "Audio Recording" radio submenu leads the WHOLE menu (the
/// owner's recolour-round amendment; a pick applies to the recording LIVE, so the
/// most-adjusted control sits on top), then the three controls ([`recording_menu`],
/// Pause/Resume leading) and a separator, then the SAME group with the per-state
/// flags the owner specified: Color Picker, Scanner and Capture stay enabled (stills while
/// recording are fine), Record and the Countdown Timer are flagged OFF (one
/// recording at a time; a countdown pick has nothing to apply to), Settings stays
/// enabled, Quit is flagged OFF (quitting would orphan the recording child's control
/// surface). The audio submenu is MOVED to the top while recording, not copied: the
/// launcher group emits its row only while idle, so the same control never appears
/// twice in one menu.
///
/// This is the full STATE model: a false flag documents WHY a row is unusable in that
/// state. What a surface actually renders is [`visible_tray_menu`], which OMITS the
/// flagged-off rows (the hide round; the why lives there).
///
/// The radio submenus' titles and rows are the renderer's to fill from its CURRENT
/// state ([`countdown_submenu_title`] / [`audio_submenu_title`]); everything else —
/// order, labels, icons, enabled flags — is decided HERE so no surface can drift.
pub fn tray_menu(recording: bool, paused: bool) -> Vec<TrayItem> {
    let mut items: Vec<TrayItem> = Vec::new();
    if recording {
        // The owner's recolour-round amendment: the Audio Recording submenu moves to
        // the VERY TOP while recording, above the controls. Moved, not copied: the
        // launcher group below emits its audio row only while idle.
        items.push(TrayItem::Radio { menu: RadioSubmenu::Audio, enabled: true });
        for control in recording_menu(paused) {
            items.push(TrayItem::Control(control));
        }
        items.push(TrayItem::Separator);
    }
    // DRAGON-582: the colour picker leads the launcher group, BEFORE Scanner (the
    // owner's placement). Enabled while recording like Scanner is: picking a colour
    // takes no capture connection and cannot disturb a running recording.
    items.push(TrayItem::Action {
        item: CaptureItem {
            label: "Color Picker",
            action: CaptureAction::ColorPicker,
            icon: Some(MenuIcon::ColorPicker),
        },
        enabled: true,
    });
    items.push(TrayItem::Action {
        item: CaptureItem {
            label: "Scanner",
            action: CaptureAction::Scan,
            icon: Some(MenuIcon::Scanner),
        },
        enabled: true,
    });
    items.push(TrayItem::Launchers {
        menu: LauncherSubmenu::Capture,
        enabled: true,
        items: capture_launcher_items(),
    });
    items.push(TrayItem::Launchers {
        menu: LauncherSubmenu::Record,
        enabled: !recording,
        items: record_launcher_items(),
    });
    items.push(TrayItem::Radio { menu: RadioSubmenu::Countdown, enabled: !recording });
    if !recording {
        // Idle keeps the audio submenu here, in the group; while recording it
        // already leads the menu (above), so emitting it here too would put the
        // same control in two places of one dropdown.
        items.push(TrayItem::Radio { menu: RadioSubmenu::Audio, enabled: true });
    }
    items.push(TrayItem::Separator);
    items.push(TrayItem::Action {
        item: CaptureItem {
            label: "Settings...",
            action: CaptureAction::Settings,
            icon: Some(MenuIcon::Settings),
        },
        enabled: true,
    });
    items.push(TrayItem::Action {
        item: CaptureItem { label: "Quit", action: CaptureAction::Quit, icon: Some(MenuIcon::Quit) },
        enabled: !recording,
    });
    items
}

/// **Pure**, unit-tested: [`tray_menu`] AS RENDERED — every row whose flag is false is
/// OMITTED. This is the list every control surface walks (the hide round).
///
/// WHY hide instead of gray or subdue: the COSMIC applet renders a dbusmenu row with
/// `enabled=false` exactly like an enabled one (no graying), and dbusmenu has no
/// text-colour property, so SUBDUED text cannot be guaranteed on Linux at all. The
/// owner's rule is "subdued if possible, else hide, and the choice must behave the
/// same on all platforms". Linux forces hide, so hide is the uniform answer
/// everywhere; mac and Windows mirror it by omission simply by walking this list.
///
/// While recording the dropdown is therefore: the Audio Recording submenu,
/// Pause/Resume, Finish & Save, Cancel & Delete, a separator, Color Picker, Scanner,
/// the Capture submenu, a separator, Settings... — no Record, no Countdown Timer, no Quit. Idle
/// hides nothing. The full model ([`tray_menu`]) keeps expressing state through its
/// flags, so the reasons stay documented and a future host that CAN subdue uniformly
/// could return to rendering the full list.
pub fn visible_tray_menu(recording: bool, paused: bool) -> Vec<TrayItem> {
    tray_menu(recording, paused)
        .into_iter()
        .filter(|entry| match entry {
            TrayItem::Control(_) | TrayItem::Separator => true,
            TrayItem::Action { enabled, .. }
            | TrayItem::Launchers { enabled, .. }
            | TrayItem::Radio { enabled, .. } => *enabled,
        })
        .collect()
}

// ── The "Countdown Timer" radio submenu (DRAGON-574) ─────────────────────────
//
// One submenu, four RADIO presets, writing the SAME persisted field every capture launch
// already honors: `delay_idx` (`state/schema.rs`), the index into the overlay's delay
// chips (No delay / 3s / 5s / 10s). The overlay, the `--countdown` CLI mapping
// (`app::countdown_index`) and this submenu therefore agree by construction; a test in
// `app/mod.rs` pins the preset tables to each other. The title carries the current value
// zero-padded ("Countdown Timer: 05"; "00" means off) so it reads without opening. While
// recording the submenu is DISABLED (the tray_menu flag); a pick while idle persists
// through [`persist_countdown_idx`] on the resident/daemon surfaces, or through the
// app's own `PickDelay` handler on a child-owned surface (the app owns the live chip).

/// The four countdown presets, in display order, in SECONDS. Index IS the persisted
/// `delay_idx`. Must stay equal to the overlay's `DELAYS` table (pinned by
/// `app::tests`).
pub const COUNTDOWN_PRESET_SECS: [u64; 4] = [0, 3, 5, 10];

/// **Pure**, unit-tested: a preset's radio-row label, zero-padded to two digits (the
/// owner's spec: 00 / 03 / 05 / 10).
pub fn countdown_label(secs: u64) -> String {
    format!("{secs:02}")
}

/// **Pure**, unit-tested: the submenu's TITLE for the persisted index — "Countdown
/// Timer: 05"; "00" means no countdown. A stray index clamps to the last preset, the
/// same clamp the app's own `PickDelay` applies.
pub fn countdown_submenu_title(delay_idx: usize) -> String {
    let secs = COUNTDOWN_PRESET_SECS[delay_idx.min(COUNTDOWN_PRESET_SECS.len() - 1)];
    format!("Countdown Timer: {}", countdown_label(secs))
}

/// One radio row of the "Countdown Timer" submenu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CountdownItem {
    /// The row label ([`countdown_label`]).
    pub label: String,
    /// The `delay_idx` activating this row persists.
    pub idx: usize,
    /// Whether this row is the currently-persisted one (the radio mark).
    pub selected: bool,
}

/// **Pure**, unit-tested: the four radio rows with `current_idx` marked (clamped like
/// [`countdown_submenu_title`]). ONE source for every renderer.
pub fn countdown_items(current_idx: usize) -> Vec<CountdownItem> {
    let current = current_idx.min(COUNTDOWN_PRESET_SECS.len() - 1);
    COUNTDOWN_PRESET_SECS
        .iter()
        .enumerate()
        .map(|(idx, &secs)| CountdownItem {
            label: countdown_label(secs),
            idx,
            selected: idx == current,
        })
        .collect()
}

/// **Pure**, unit-tested: the preset index at a display-order position (the shape
/// `ksni`'s radio-select callback hands back), `None` for an out-of-range index so a
/// stray index can never persist a countdown. The display order IS the index space, so
/// this is a bounds check, kept as a function for symmetry with
/// [`audio_arm_choice_at`].
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // only ksni reports picks as an index
pub fn countdown_choice_at(index: usize) -> Option<usize> {
    (index < COUNTDOWN_PRESET_SECS.len()).then_some(index)
}

/// Write a countdown radio pick's preset index — the resident/daemon idle route
/// (DRAGON-574), the countdown twin of [`persist_audio_arms`]: load-modify-save through
/// the normal settings write path, setting `delay_idx`, so every later capture launch
/// (tray, hotkey, bare CLI) counts down from the new value. Returns the (clamped) index
/// written; the caller refreshes its submenu title / radio mark from it. A child-owned
/// surface does NOT call this: it sends the pick to the app, whose `PickDelay` handler
/// persists AND updates the live overlay chip.
pub fn persist_countdown_idx(idx: usize) -> usize {
    let idx = idx.min(COUNTDOWN_PRESET_SECS.len() - 1);
    let mut p = crate::state::load();
    p.delay_idx = idx;
    crate::state::save(&p);
    idx
}

// ── The "Audio Recording" radio submenu (DRAGON-558, labels DRAGON-574) ───────
//
// One submenu, four RADIO rows, one decision. The submenu is ALWAYS in the tray menu
// (a [`TrayItem::Radio`] row of [`tray_menu`], enabled in BOTH states); its TITLE
// carries the current arm state so the state reads without opening it, and ONE radio
// click sets the COMPLETE state (mic AND system in one pick — the owner's stay-open menu
// is not portably possible, so a single-click complete pick is the agreed alternative).
// What a pick DOES depends on whether a recording exists; that decision lives here —
// portable, pure, unit-tested — so no per-platform tray hand-rolls its own
// `if recording`, and the syscall halves (the config write, the IPC relay) stay in the
// platform code that owns them. DRAGON-574 renamed the two single-channel rows: "Mic
// Only" / "System Only" (was "Microphone only" / "System only"), and the title word for
// those states follows.

/// The complete audio-arm state as ONE choice (DRAGON-558): which recording channels are
/// armed. The "Audio Recording" submenu renders these as its four radio rows, and one pick
/// sets the whole pair, so there is no click sequence for a closing menu to interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioArmState {
    /// Microphone and system audio both armed.
    Both,
    /// Microphone only.
    MicrophoneOnly,
    /// System audio only.
    SystemOnly,
    /// Neither channel armed.
    None,
}

impl AudioArmState {
    /// **Pure**, unit-tested: the state for a `(mic, system)` arm pair.
    pub fn from_arms(mic: bool, system: bool) -> Self {
        match (mic, system) {
            (true, true) => AudioArmState::Both,
            (true, false) => AudioArmState::MicrophoneOnly,
            (false, true) => AudioArmState::SystemOnly,
            (false, false) => AudioArmState::None,
        }
    }

    /// **Pure**, unit-tested: the `(mic, system)` arm pair this state means. Exact inverse
    /// of [`AudioArmState::from_arms`], pinned by a round-trip test.
    pub fn arms(self) -> (bool, bool) {
        match self {
            AudioArmState::Both => (true, true),
            AudioArmState::MicrophoneOnly => (true, false),
            AudioArmState::SystemOnly => (false, true),
            AudioArmState::None => (false, false),
        }
    }

    /// **Pure**, unit-tested: the state word the submenu TITLE carries (the owner's
    /// DRAGON-574 wording: Mic + System / Mic Only / System Only / None; the DRAGON-558
    /// third live test renamed "Both" to say WHAT it combines, and DRAGON-574 aligned the
    /// single-channel words with the renamed radio rows). No em/en-dashes.
    pub fn word(self) -> &'static str {
        match self {
            AudioArmState::Both => "Mic + System",
            AudioArmState::MicrophoneOnly => "Mic Only",
            AudioArmState::SystemOnly => "System Only",
            AudioArmState::None => "None",
        }
    }

    /// **Pure**, unit-tested: the radio-row label. Identical to [`AudioArmState::word`]
    /// since DRAGON-574 unified the row and title wording ("Mic Only" / "System Only"
    /// replaced "Microphone only" / "System only"); kept as its own name because the
    /// renderers spell out which surface they are building.
    pub fn radio_label(self) -> &'static str {
        self.word()
    }

    /// **Pure**, unit-tested: parse the `--audio <channels>` CLI value (DRAGON-559) into
    /// an arm state, `None` for anything unrecognised so a typo can never silently arm (or
    /// silently NOT disarm) a channel — the caller rejects the launch instead.
    ///
    /// The vocabulary is the DRAGON-558 one (both / microphone / system / none) plus the
    /// obvious spellings, case-insensitive: `both` | `all` | `mic+system` | `system+mic`;
    /// `microphone` | `mic`; `system` | `sys`; `none` | `off`.
    pub fn parse_flag(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "both" | "all" | "mic+system" | "system+mic" => Some(AudioArmState::Both),
            "microphone" | "mic" => Some(AudioArmState::MicrophoneOnly),
            "system" | "sys" => Some(AudioArmState::SystemOnly),
            "none" | "off" => Some(AudioArmState::None),
            _ => None,
        }
    }
}

/// **Pure**, unit-tested: the `(mic, system)` arms a capture launch starts with
/// (DRAGON-559). A given `--audio` state wins outright — a per-LAUNCH override that the
/// caller must never write back to the settings store — and an absent flag keeps the
/// persisted pair unchanged. The one place the override decision lives, so the app cannot
/// half-apply a flag (one channel from the flag, one from the store).
pub fn launch_audio_arms(flag: Option<AudioArmState>, persisted: (bool, bool)) -> (bool, bool) {
    match flag {
        Some(state) => state.arms(),
        None => persisted,
    }
}

/// The four radio choices in DISPLAY ORDER: Mic + System, Mic Only, System Only,
/// None. The
/// one order every renderer uses, and the index space `ksni`'s radio-select callback
/// reports back into (see [`audio_arm_choice_at`]).
pub const AUDIO_ARM_ORDER: [AudioArmState; 4] = [
    AudioArmState::Both,
    AudioArmState::MicrophoneOnly,
    AudioArmState::SystemOnly,
    AudioArmState::None,
];

/// **Pure**, unit-tested: the choice at a display-order index (the shape `ksni`'s
/// `RadioGroup` select callback hands back), `None` for an out-of-range index so a stray
/// index can never mis-arm.
// Linux-only in the bin build: only ksni reports a pick as an INDEX (macOS wires one
// selector per row, Windows one command id per row), so the two Linux trays are the only
// callers. The body stays portable because the index space is the portable AUDIO_ARM_ORDER.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn audio_arm_choice_at(index: usize) -> Option<AudioArmState> {
    AUDIO_ARM_ORDER.get(index).copied()
}

/// **Pure**, unit-tested: the submenu's TITLE for the current state — "Audio Recording:
/// Mic + System" / "Mic Only" / "System Only" / "None". The title is the
/// read-without-opening surface:
/// every renderer re-titles the submenu whenever the state changes.
pub fn audio_submenu_title(state: AudioArmState) -> String {
    format!("Audio Recording: {}", state.word())
}

/// One radio row of the "Audio Recording" submenu: its label, the COMPLETE state a click
/// sets, and whether it carries the radio mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioArmItem {
    /// The row label ([`AudioArmState::radio_label`]).
    pub label: &'static str,
    /// The complete arm state activating this row sets.
    pub choice: AudioArmState,
    /// Whether this row is the currently-active one (the radio mark).
    pub selected: bool,
}

/// **Pure**, unit-tested: the four radio rows in [`AUDIO_ARM_ORDER`], with `current`
/// marked. ONE source for every renderer, so the rows, their order and the mark can never
/// drift between the Linux trays, the mac menu bar and the Windows popup.
pub fn audio_arm_items(current: AudioArmState) -> Vec<AudioArmItem> {
    AUDIO_ARM_ORDER
        .iter()
        .map(|&choice| AudioArmItem {
            label: choice.radio_label(),
            choice,
            selected: choice == current,
        })
        .collect()
}

/// **Pure**, unit-tested: whether a radio pick acts on the LIVE recording.
///
/// True while a recording is in progress (paused included — the child, its capture
/// connection and its file all still exist): the pick is applied as live toggles
/// ([`audio_pick_live_actions`]), which become gain automation in the recording,
/// reversible until finalize; the recording child's own toggle handler persists each flip,
/// so the saved default follows. False while idle: the pick writes the PERSISTED arms
/// (`record_mic` / `record_system_audio` in the settings store) that a NEW recording reads
/// at start ([`persist_audio_arms`]).
///
/// The resident/daemon surfaces route on this. The CHILD-owned trays (`crate::tray`) do
/// not need it: they always send the app the same toggle tray events, because the app
/// process owns both behaviours already (its handler flips + persists the arm while idle
/// and drives the gain automation while recording).
pub fn audio_toggles_are_live(recording: bool) -> bool {
    recording
}

/// **Pure**, unit-tested: the `(mic, system)` pair the submenu renders FROM — its title
/// state and radio mark. The LIVE channel arms while a recording is in progress, else the
/// PERSISTED (`armed`) defaults — the same split as [`audio_toggles_are_live`], in one
/// place so a surface can never route a pick live while titling from the persisted state
/// (or the reverse). The caller supplies `armed` however it keeps it fresh: the Linux
/// resident caches it on its config-mtime tick, macOS and Windows read the store at menu
/// build.
pub fn audio_arm_source(
    recording: bool,
    live: (bool, bool),
    armed: (bool, bool),
) -> (bool, bool) {
    if audio_toggles_are_live(recording) {
        live
    } else {
        armed
    }
}

/// **Pure**, unit-tested: the live toggle actions that carry the `current` `(mic, system)`
/// arms to `choice`. Zero, one or two of [`RecordingAction::ToggleMic`] /
/// [`RecordingAction::ToggleSystemAudio`], because the live lane speaks TOGGLES (each one
/// is a `TrackGain` automation event in the recording): a channel already in its target
/// state gets no action, so a pick can never double-flip one. Picking the current state is
/// a no-op by construction.
pub fn audio_pick_live_actions(
    current: (bool, bool),
    choice: AudioArmState,
) -> Vec<RecordingAction> {
    let target = choice.arms();
    let mut actions = Vec::new();
    if current.0 != target.0 {
        actions.push(RecordingAction::ToggleMic);
    }
    if current.1 != target.1 {
        actions.push(RecordingAction::ToggleSystemAudio);
    }
    actions
}

/// Write a radio pick's COMPLETE arm state — the idle route (DRAGON-558). The
/// resident/daemon surfaces call this from a pick when no recording exists:
/// load-modify-save through the normal settings write path (`crate::state`), setting both
/// `record_mic` and `record_system_audio` in one save, so a new capture child reads the
/// new arms at start. Returns the `(mic, system)` pair written; the caller refreshes its
/// submenu title / radio mark from it (or from its own config-change watch).
pub fn persist_audio_arms(choice: AudioArmState) -> (bool, bool) {
    let (mic, system) = choice.arms();
    let mut p = crate::state::load();
    p.record_mic = mic;
    p.record_system_audio = system;
    crate::state::save(&p);
    (mic, system)
}

impl CaptureAction {
    /// The CLI argv a launcher / Settings action passes to a spawned one-shot child, or
    /// `None` for the one action that never spawns: [`CaptureAction::Quit`] (it tears the
    /// resident/daemon down, and on a child-owned surface it ends the session). ONE
    /// mapping shared by every surface's spawn path so the daemon, resident, and the
    /// child-owned recording trays launch byte-identical children. The radio submenus
    /// never spawn: they are [`TrayItem::Radio`] rows, not actions, so they cannot reach
    /// any `spawn_args().unwrap_or(…)` launcher arm.
    ///
    /// A LIST since DRAGON-559 (it was `spawn_flag`, one flag): the record entries spawn
    /// their capture twin's mode flag PLUS `--video`, on purpose. CLI.md's convention is
    /// composition (`--monitor --video` records a monitor), the parser already reads the
    /// pair, and a dedicated `--record-<mode>` flag would be a second spelling of an argv
    /// that already exists. The record children deliberately carry NO `--audio` flag (an
    /// absent flag keeps the persisted arms, which are exactly what the tray's Audio
    /// Recording submenu manages) and NO `--countdown` flag (an absent flag reads the
    /// persisted `delay_idx`, which is exactly what the Countdown Timer submenu writes).
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    pub fn spawn_args(self) -> Option<&'static [&'static str]> {
        match self {
            CaptureAction::ColorPicker => Some(&["--color-picker"]),
            CaptureAction::Scan => Some(&["--scan"]),
            CaptureAction::Region => Some(&["--region"]),
            CaptureAction::Window => Some(&["--window"]),
            CaptureAction::Monitor => Some(&["--monitor"]),
            CaptureAction::RecordRegion => Some(&["--region", "--video"]),
            CaptureAction::RecordWindow => Some(&["--window", "--video"]),
            CaptureAction::RecordMonitor => Some(&["--monitor", "--video"]),
            CaptureAction::Settings => Some(&["--settings"]),
            CaptureAction::Quit => None,
        }
    }
}

/// How long a tray-menu LAUNCHER waits before its child spawns, **on macOS only**
/// (DRAGON-574, narrowed to macOS by DRAGON-600). There the daemon owns the `NSMenu`, so
/// AppKit has already dismissed it by the time the action fires and this is a short settle
/// for the close animation, not a guess about another process. Which entries wait is
/// [`spawn_waits_for_menu_dismiss`]; radio picks and Quit spawn nothing, so nothing is
/// delayed for them. Windows does not use it at all: `TrackPopupMenu(TPM_RETURNCMD)` only
/// returns after the menu is dismissed, which is a real menu-closed signal.
///
/// **Linux deliberately does NOT use this, and a pre-spawn wait there is structurally
/// impossible.** See [`MENU_LAUNCH_ENV`] for the mechanism that replaced it. DRAGON-600
/// went looking for the signal Windows has, and found four walls:
///
/// 1. The protocol HAS the event. `com.canonical.dbusmenu` defines
///    `Event(id, "closed", …)`, and COSMIC's `cosmic-applet-status-area` even implements
///    the sender (`components/status_menu.rs`, `pub fn closed`). But its one and only
///    caller is the "user clicked the same tray icon a second time" toggle in
///    `components/app.rs`. Activating a ROW never reaches it, and the upstream click
///    handler says so in as many words: `if is_submenu { … } else { // TODO: Close menu? }`.
///    A live `dbus-monitor` capture of two complete menu cycles saw `opened`,
///    `AboutToShow(0)`, `GetLayout` and `clicked`, and never once `closed`.
/// 2. `ksni` would swallow it anyway. `Service::event` matches `"clicked"` and then
///    `_ => ()`, in 0.3.5 AND in 0.3.6, the newest release; `AboutToShow` never reaches
///    the tray either; the `Tray` trait has no menu-opened / menu-closed hook to
///    implement; and `Handle` exposes no connection, so the event cannot be intercepted
///    from outside the crate. Upgrading ksni buys nothing here.
/// 3. We cannot dismiss the dropdown ourselves. It is the HOST's surface. Every method on
///    the dbusmenu interface we serve is inbound, and the only two signals we may emit,
///    `ItemsPropertiesUpdated` and `LayoutUpdated`, make the host REBUILD the popup rather
///    than close it. ksni's one teardown, `Handle::shutdown`, takes the tray icon with it.
///    This is the whole asymmetry with Windows and macOS: there WE own the menu.
/// 4. And then the owner supplied the fact that settles it: on COSMIC no tray row closes
///    the popup until the LAUNCHED PROCESS appears, for any application, not just ours.
///    The dismissal is caused by the launch. A wait placed BEFORE the spawn is therefore
///    waiting for something its own waiting prevents, at any value.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // macOS owns its NSMenu; kept for the tests
pub const MENU_DISMISS_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// The env marker a tray-menu launcher puts on its child, on Linux (DRAGON-600). Set only
/// for a row that [`spawn_waits_for_menu_dismiss`] answers true for, so the ONE rule is
/// unchanged and only the place it is CONSULTED moved.
///
/// The child reads it once at launch and, if it is going to grab the frozen flats, HOLDS
/// that grab until its own overlay has taken keyboard focus, which is the event that
/// dismisses the host's dropdown. See `app::MenuFlatsHold`. A launch with no menu on
/// screen (PrintScreen, a hotkey, SIGUSR1) never carries the marker and pays nothing.
///
/// WHY a marker rather than a wait: the popup outlives the click and dies only when the
/// launched process takes focus, so the daemon cannot observe the dismissal at all. The
/// child can, because the child is what causes it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // Linux is the only host-owned menu
pub const MENU_LAUNCH_ENV: &str = "CCK_TRAY_MENU_LAUNCH";

/// **Pure**, unit-tested: whether activating `action` from a tray menu has a dropdown to
/// get out of the way. THE one rule, and DRAGON-600 changed only where it is consulted:
/// macOS still spends [`MENU_DISMISS_DELAY`] before the spawn, while Linux tags the child
/// with [`MENU_LAUNCH_ENV`] and lets the child do the waiting.
///
/// DERIVED, since DRAGON-598, from the one structural fact a new entry cannot be added
/// without: [`CaptureAction::spawn_args`]. An entry that spawns a child from a menu IS a
/// launching entry, so a row joins this set the moment it gets its argv, and there is no
/// second list anybody has to remember. [`CaptureAction::Quit`] stays out for free, since
/// it has no argv at all: it tears the resident down rather than launching anything. The
/// radio submenus never reach here (they are [`TrayItem::Radio`] rows, not actions).
///
/// What this replaced was a hand-kept exclusion, `!matches!(action, Settings | Quit)`, and
/// the reason for the change is worth keeping. The behaviour was right, but the RULE was
/// only readable as a double negative, and its own doc had already gone stale: it still
/// enumerated "Scanner and the six launcher rows" after DRAGON-582 added a seventh, the
/// colour picker. A rule you have to re-derive from a negative match, against a doc that
/// no longer lists everything, is one a future entry can silently fall out of.
///
/// Settings is the one behaviour change: it opens a window and grabs no pixels, so it
/// never needed the wait. It joined anyway, because buying it is what makes the set
/// impossible to forget.
#[cfg_attr(windows, allow(dead_code))] // Windows has a native menu-closed signal
pub fn spawn_waits_for_menu_dismiss(action: CaptureAction) -> bool {
    action.spawn_args().is_some()
}

/// Linux: start a tray-menu launcher's child IMMEDIATELY, tagged with
/// [`MENU_LAUNCH_ENV`] (DRAGON-600). The tag is the whole mechanism, and delay is its
/// opposite: on COSMIC the dropdown is dismissed BY the launched process taking keyboard
/// focus, so every millisecond spent not-launching is a millisecond the menu stays up.
/// The child holds its own frozen-flats grab until its overlay has that focus.
///
/// The log line marks the ACTIVATION instant. The next timestamp anyone cares about is
/// the child's flats grab, and it used to have to be inferred by subtracting a constant
/// from the spawn line, which quietly assumed the very thing under investigation.
#[cfg(target_os = "linux")]
pub fn spawn_capture_child_args_from_menu(
    args: &'static [&'static str],
    mut envs: Vec<(String, String)>,
) {
    log::debug!("menu launcher activated: spawning now, the child owns the dismiss wait");
    envs.push((MENU_LAUNCH_ENV.to_string(), "1".to_string()));
    let env_refs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    spawn_capture_child_args(args, &env_refs);
}

/// macOS: [`spawn_capture_child_args`] AFTER [`MENU_DISMISS_DELAY`], on a detached helper
/// thread so the menu loop never blocks (DRAGON-574). `envs` is owned because the spawn
/// outlives the menu callback that resolved it; anything press-time (the trigger display)
/// must be resolved BEFORE calling this, so only the spawn waits, never the state read.
/// Best-effort like the underlying spawn.
///
/// macOS keeps the pre-spawn shape because AppKit dismisses the `NSMenu` before it sends
/// the action, so unlike Linux the thing being waited for has already happened.
#[cfg(target_os = "macos")]
pub fn spawn_capture_child_args_deferred(
    args: &'static [&'static str],
    envs: Vec<(String, String)>,
) {
    let _ = std::thread::Builder::new().name("cck-menu-spawn".into()).spawn(move || {
        std::thread::sleep(MENU_DISMISS_DELAY);
        let env_refs: Vec<(&str, &str)> =
            envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        spawn_capture_child_args(args, &env_refs);
    });
}

/// Spawn the full app as a DETACHED one-shot child with `flag`. Detached (`setsid` on unix;
/// CREATE_NO_WINDOW + no-wait on Windows) so the child owns its own session — it cannot be
/// reaped as our child (no SIGCHLD churn) and the launcher process survives the child
/// exiting/crashing (and vice versa). Best-effort: a spawn failure just logs. This is the ONE
/// spawn path every recording control surface uses — the mac daemon, the Linux resident, the
/// Windows tray daemon (DRAGON-237), and the child-owned recording trays (`crate::tray`), so
/// a "Capture Menu" launch is byte-identical everywhere. Present on mac + Linux + Windows
/// (the platforms with a resident/tray surface); other platforms never call it.
///
/// Returns whether a child process was actually created (DRAGON-438). Windows needs that
/// answer because its tray daemon ACKNOWLEDGES the spawn to a waiting launcher, and an ack
/// for a spawn that never happened is just the silent failure moved one step along. Every
/// other caller ignores it, and mac/Linux behaviour is unchanged — the unix body already
/// knew this and only logged it.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub fn spawn_capture_child(flag: &str) -> bool {
    spawn_capture_child_with_env(flag, &[])
}

/// Windows body of [`spawn_capture_child_with_env`] (DRAGON-237): the detach mechanism is
/// platform logic, so it lives under `platform/windows/` behind this thin dispatch (strict
/// split). Same contract as the unix body — a detached one-shot child, best-effort.
#[cfg(windows)]
pub fn spawn_capture_child_with_env(flag: &str, envs: &[(&str, &str)]) -> bool {
    spawn_capture_child_args(&[flag], envs)
}

/// DRAGON-428: [`spawn_capture_child_with_env`] for a child that needs MORE THAN ONE flag.
/// The daemon's "(no editor)" capture hotkeys pass `--active-window --no-editor`, and argv
/// is the daemon's only channel to the child, so the spawn seam has to carry a list.
///
/// Additive: the single-flag entry points delegate here with a one-element slice, so every
/// existing caller keeps its exact behaviour — including the DRAGON-438 spawn-happened
/// answer, which is what the tray daemon's capture ack is allowed to depend on.
#[cfg(windows)]
pub fn spawn_capture_child_args(args: &[&str], envs: &[(&str, &str)]) -> bool {
    crate::platform::windows::process::spawn_detached_child(args, envs)
}

/// [`spawn_capture_child`] with extra environment variables for the child (e.g.
/// `CCK_SETTINGS_TAB=about` so a post-update settings child opens on About).
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn spawn_capture_child_with_env(flag: &str, envs: &[(&str, &str)]) -> bool {
    spawn_capture_child_args(&[flag], envs)
}

/// DRAGON-428: [`spawn_capture_child_with_env`] for a child that needs MORE THAN ONE flag.
/// The daemon's "(no editor)" capture hotkeys pass `--active-window --no-editor`, and argv
/// is the daemon's only channel to the child, so the spawn seam has to carry a list.
///
/// Additive: the single-flag entry points delegate here with a one-element slice, so every
/// existing caller keeps its exact behaviour — including the DRAGON-438 spawn-happened
/// answer the return value carries.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn spawn_capture_child_args(args: &[&str], envs: &[(&str, &str)]) -> bool {
    let flag = args.first().copied().unwrap_or("");
    // `self_exe`, not `current_exe` (DRAGON-510): a capture child is detached and unreaped
    // and long outlives the daemon call that started it, so under an AppImage it must be
    // launched from the `.AppImage` file rather than from a mount that can disappear.
    let exe = match crate::util::self_exe() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("spawn_capture_child: self_exe failed, cannot spawn {flag}: {e}");
            return false;
        }
    };
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    // Own session: detach from the launcher's controlling terminal / process group so the
    // child is fully independent (no reap, no signal coupling either way).
    // SAFETY: `setsid` in the forked child before exec is async-signal-safe and touches no
    // shared state — the textbook detach.
    unsafe {
        use std::os::unix::process::CommandExt as _;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(mut child) => {
            log::info!("spawned capture child {flag} (pid {})", child.id());
            // Reap without blocking (DRAGON-184): the one-shot app exits before its
            // children finish (init adopts them), but the LONG-LIVED residents
            // (Linux tray / mac menu-bar daemon) are real parents — never waiting
            // left one <defunct> zombie per capture. A tiny detached waiter costs a
            // thread for the child's lifetime; portable std, no SIGCHLD fiddling.
            let _ = std::thread::Builder::new()
                .name("cck-child-reaper".into())
                .spawn(move || {
                    let _ = child.wait();
                });
            true
        }
        Err(e) => {
            log::warn!("spawn capture child {flag} failed: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_state_maps_recording_and_paused() {
        assert_eq!(icon_state(false, false), IconState::Idle);
        assert_eq!(icon_state(false, true), IconState::Idle); // not recording wins
        assert_eq!(icon_state(true, false), IconState::Recording);
        assert_eq!(icon_state(true, true), IconState::Paused);
    }

    /// The two colour ROLES of a tinted tray icon. Moved here from `platform/windows/daemon.rs`
    /// by DRAGON-541, which is the point of the ticket as much as the fix is: this is a pure
    /// string test, and living in a `cfg(windows)` module meant it only ran on a Windows build,
    /// while the rule it pins was one the Linux trays had never followed.
    #[test]
    fn a_tinted_tray_icon_has_accent_brackets_and_a_red_centre() {
        // A known accent pins the recolouring deterministically.
        let accent = [0x00, 0x78, 0xd4]; // #0078d4 (the Windows-11 default blue)
        let idle = tray_icon_svg(IconState::Idle, accent);
        let rec = tray_icon_svg(IconState::Recording, accent);
        let paused = tray_icon_svg(IconState::Paused, accent);

        // Nothing keeps the black template colour in any state.
        for (name, svg) in [("idle", &idle), ("recording", &rec), ("paused", &paused)] {
            assert!(!svg.contains("#000"), "{name} left an untinted black");
        }

        // Idle is brackets only, so it is entirely accent and carries no red.
        assert!(idle.contains("#0078d4"), "idle tinted with the accent");
        assert!(!idle.contains(RECORDING_RED), "idle has no centre glyph to redden");

        // Both ACTIVE states: accent brackets, red centre. PAUSED matching RECORDING is the
        // point — it used to tint its bars with the accent, which made the state that most
        // needs to stand out blend into its own brackets. On Linux, until DRAGON-541, BOTH of
        // them did that.
        for (name, svg) in [("recording", &rec), ("paused", &paused)] {
            assert!(svg.contains(r##"stroke="#0078d4""##), "{name} brackets are accent-tinted");
            assert!(
                svg.contains(&format!(r##"fill="{RECORDING_RED}""##)),
                "{name} centre glyph is the recording red, not the accent"
            );
        }

        // All three still read differently, which is the whole reason for three icons:
        // recording and paused share their colours, so only the GEOMETRY (dot vs bars)
        // separates them, and that must survive.
        assert_ne!(idle, rec);
        assert_ne!(rec, paused);
        assert_ne!(idle, paused);
    }

    /// The recolouring is a pair of string substitutions over hand-written templates, so it
    /// only works while every template spells its two roles the way it expects. Pin that
    /// contract on the templates themselves: an edit that renames a `fill` or drops a `stroke`
    /// would otherwise silently produce a half-tinted icon that no other test would notice.
    #[test]
    fn every_template_spells_its_two_roles_the_way_the_recolouring_expects() {
        for (name, svg) in [
            ("idle", ICON_BORDER),
            ("recording", ICON_BORDER_DOT),
            ("paused", ICON_BORDER_PAUSE),
        ] {
            assert!(svg.contains(r##"stroke="#000""##), "{name} must stroke its brackets in #000");
        }
        // Only the two ACTIVE templates have a centre glyph to fill. The idle one says
        // `fill="none"` on its bracket path, which must not be mistaken for one.
        assert!(!ICON_BORDER.contains(r##"fill="#000""##), "idle has no filled centre glyph");
        for (name, svg) in [("recording", ICON_BORDER_DOT), ("paused", ICON_BORDER_PAUSE)] {
            assert!(svg.contains(r##"fill="#000""##), "{name} must fill its centre glyph in #000");
        }
    }

    #[test]
    fn pause_label_swaps_and_is_dash_free() {
        assert_eq!(pause_label(false), "Pause Recording");
        assert_eq!(pause_label(true), "Resume Recording");
        for l in [pause_label(false), pause_label(true)] {
            assert!(!l.contains('\u{2014}') && !l.contains('\u{2013}'), "dash in {l:?}");
        }
    }

    #[test]
    fn icon_svg_shares_corner_brackets_and_swaps_only_the_centre_glyph() {
        // The exact corner-bracket path all three states must share (pins the three
        // inline SVGs to ONE geometry — drift in any fails here).
        const CORNERS: &str = "M3.2 8.7 L3.2 7.7 A4.5 4.5 0 0 1 7.7 3.2 L8.7 3.2 M15.3 3.2 L16.3 3.2 A4.5 4.5 0 0 1 20.8 7.7 L20.8 8.7 M20.8 15.3 L20.8 16.3 A4.5 4.5 0 0 1 16.3 20.8 L15.3 20.8 M8.7 20.8 L7.7 20.8 A4.5 4.5 0 0 1 3.2 16.3 L3.2 15.3";
        let idle = icon_svg(IconState::Idle);
        let rec = icon_svg(IconState::Recording);
        let paused = icon_svg(IconState::Paused);
        assert_ne!(idle, rec, "idle and recording must render differently");
        assert_ne!(rec, paused, "recording and paused must render differently");
        assert!(
            idle.contains(CORNERS) && rec.contains(CORNERS) && paused.contains(CORNERS),
            "all three carry the four corner brackets"
        );
        assert!(!idle.contains("<circle") && !idle.contains("<rect"), "idle is corners only");
        assert!(rec.contains("<circle"), "recording adds the centre dot");
        assert!(!paused.contains("<circle"), "paused drops the dot");
        assert_eq!(paused.matches("<rect").count(), 2, "paused adds the two pause bars");
    }

    #[test]
    fn recording_menu_order_labels_and_checks() {
        // The DRAGON-558 order: Pause/Resume Recording, Finish & Save, Cancel & Delete.
        // The two audio toggles left this menu (the "Audio Recording" radio submenu owns
        // them now), so the recording group carries no checkmarks and no separator.
        let items = recording_menu(false);
        let labels: Vec<&str> = items.iter().map(|i| i.label).collect();
        assert_eq!(
            labels,
            vec!["Pause Recording", "Finish & Save Recording", "Cancel & Delete Recording"]
        );
        // Actions map 1:1, all plain items.
        for item in &items {
            assert_eq!(item.kind, MenuItemKind::Standard);
        }
        assert_eq!(items[0].action, RecordingAction::TogglePause);
        assert_eq!(items[1].action, RecordingAction::Stop);
        assert_eq!(items[2].action, RecordingAction::Cancel);
        // The audio LIVE actions did not leave the vocabulary: the capture group's audio
        // items still fire them while recording (see audio_toggle_route_tests).
        assert!(
            !items.iter().any(|i| matches!(
                i.action,
                RecordingAction::ToggleMic | RecordingAction::ToggleSystemAudio
            )),
            "the recording menu itself carries no audio items since DRAGON-558"
        );
    }

    #[test]
    fn recording_menu_pause_label_follows_state() {
        // The Pause/Resume item leads the group since DRAGON-558.
        assert_eq!(recording_menu(true)[0].label, "Resume Recording");
        assert_eq!(recording_menu(false)[0].label, "Pause Recording");
    }

    /// A compact fingerprint of one [`TrayItem`] for shape pins: `(label, enabled)`,
    /// where a submenu's label is its base label and a separator's is "".
    fn shape(item: &TrayItem) -> (String, bool) {
        match item {
            TrayItem::Control(c) => (c.label.to_string(), true),
            TrayItem::Action { item, enabled } => (item.label.to_string(), *enabled),
            TrayItem::Launchers { menu, enabled, .. } => (menu.label().to_string(), *enabled),
            TrayItem::Radio { menu, enabled } => {
                let label = match menu {
                    RadioSubmenu::Countdown => "Countdown Timer",
                    RadioSubmenu::Audio => "Audio Recording",
                };
                (label.to_string(), *enabled)
            }
            TrayItem::Separator => (String::new(), true),
        }
    }

    /// The owner's IDLE shape (DRAGON-574, plus DRAGON-582's Color Picker at the
    /// head): Color Picker, Scanner, the Capture and Record submenus, the
    /// Countdown Timer and Audio Recording radio submenus, a separator, Settings...,
    /// Quit — everything enabled.
    #[test]
    fn idle_tray_menu_is_the_owners_shape() {
        let items = tray_menu(false, false);
        let got: Vec<(String, bool)> = items.iter().map(shape).collect();
        assert_eq!(
            got,
            vec![
                ("Color Picker".to_string(), true),
                ("Scanner".to_string(), true),
                ("Capture".to_string(), true),
                ("Record".to_string(), true),
                ("Countdown Timer".to_string(), true),
                ("Audio Recording".to_string(), true),
                (String::new(), true), // separator
                ("Settings...".to_string(), true),
                ("Quit".to_string(), true),
            ]
        );
        // No Control rows while idle.
        assert!(!items.iter().any(|i| matches!(i, TrayItem::Control(_))));
    }

    /// The owner's WHILE-RECORDING shape of the full STATE model (DRAGON-574 plus the
    /// recolour-round amendment): the Audio Recording submenu leads the WHOLE menu,
    /// then the three controls and a separator, then the same group with Record,
    /// Countdown Timer and Quit flagged OFF (rendered as HIDDEN since the hide round;
    /// see [`visible_tray_menu`]) while Scanner, Capture and Settings stay on. The
    /// audio submenu is MOVED to the top, not copied: the group emits its row only
    /// while idle.
    #[test]
    fn recording_tray_menu_is_the_owners_shape() {
        let items = tray_menu(true, false);
        let got: Vec<(String, bool)> = items.iter().map(shape).collect();
        assert_eq!(
            got,
            vec![
                ("Audio Recording".to_string(), true), // leads the menu; live arms
                ("Pause Recording".to_string(), true),
                ("Finish & Save Recording".to_string(), true),
                ("Cancel & Delete Recording".to_string(), true),
                (String::new(), true), // separator
                ("Color Picker".to_string(), true),
                ("Scanner".to_string(), true),
                ("Capture".to_string(), true),
                ("Record".to_string(), false),    // flagged off: one recording at a time
                ("Countdown Timer".to_string(), false), // flagged off while recording
                (String::new(), true), // separator
                ("Settings...".to_string(), true),
                ("Quit".to_string(), false), // flagged off while recording
            ]
        );
        // Paused flips only the pause control's label (now the second row, under the
        // leading audio submenu).
        let paused = tray_menu(true, true);
        assert_eq!(paused[0], items[0]);
        assert_eq!(shape(&paused[1]).0, "Resume Recording");
        assert_eq!(&paused[2..], &items[2..]);
    }

    /// The RENDERED while-recording dropdown (the owner's hide rule): the flagged-off
    /// rows are OMITTED, not grayed, because the COSMIC applet does not gray dbusmenu
    /// `enabled=false` and dbusmenu has no text-colour property, so subdued text
    /// cannot be guaranteed on Linux; hide is the one uniform answer. No Record, no
    /// Countdown Timer, no Quit.
    #[test]
    fn the_rendered_recording_menu_omits_the_flagged_off_rows() {
        let got: Vec<(String, bool)> =
            visible_tray_menu(true, false).iter().map(shape).collect();
        assert_eq!(
            got,
            vec![
                ("Audio Recording".to_string(), true), // leads the whole menu
                ("Pause Recording".to_string(), true),
                ("Finish & Save Recording".to_string(), true),
                ("Cancel & Delete Recording".to_string(), true),
                (String::new(), true), // separator
                ("Color Picker".to_string(), true),
                ("Scanner".to_string(), true),
                ("Capture".to_string(), true),
                (String::new(), true), // separator
                ("Settings...".to_string(), true),
            ]
        );
    }

    /// Idle hides nothing: the rendered list IS the full model, so the hide rule can
    /// never eat an idle row by accident.
    #[test]
    fn the_rendered_idle_menu_hides_nothing() {
        assert_eq!(visible_tray_menu(false, false), tray_menu(false, false));
    }

    /// The two launcher submenus: the capture trio and the record trio, full labels,
    /// with the toolbar's TARGET glyphs — the SAME icon for Capture Region and Record
    /// Region (the target is the icon's meaning, the verb comes from the submenu).
    #[test]
    fn launcher_submenus_carry_the_trios_with_the_target_icons() {
        for (recording, expect_enabled) in [(false, true), (true, false)] {
            let items = tray_menu(recording, false);
            let capture = items.iter().find_map(|i| match i {
                TrayItem::Launchers { menu: LauncherSubmenu::Capture, enabled, items } => {
                    Some((*enabled, items.clone()))
                }
                _ => None,
            });
            let record = items.iter().find_map(|i| match i {
                TrayItem::Launchers { menu: LauncherSubmenu::Record, enabled, items } => {
                    Some((*enabled, items.clone()))
                }
                _ => None,
            });
            let (cap_enabled, cap_items) = capture.expect("a Capture submenu in every state");
            let (rec_enabled, rec_items) = record.expect("a Record submenu in every state");
            assert!(cap_enabled, "Capture stays enabled in both states");
            assert_eq!(rec_enabled, expect_enabled, "Record is disabled exactly while recording");
            assert_eq!(
                cap_items
                    .iter()
                    .map(|i| (i.label, i.action, i.icon))
                    .collect::<Vec<_>>(),
                vec![
                    ("Capture Region", CaptureAction::Region, Some(MenuIcon::Region)),
                    ("Capture Window", CaptureAction::Window, Some(MenuIcon::Window)),
                    ("Capture Monitor", CaptureAction::Monitor, Some(MenuIcon::Monitor)),
                ]
            );
            assert_eq!(
                rec_items
                    .iter()
                    .map(|i| (i.label, i.action, i.icon))
                    .collect::<Vec<_>>(),
                vec![
                    ("Record Region", CaptureAction::RecordRegion, Some(MenuIcon::Region)),
                    ("Record Window", CaptureAction::RecordWindow, Some(MenuIcon::Window)),
                    ("Record Monitor", CaptureAction::RecordMonitor, Some(MenuIcon::Monitor)),
                ]
            );
        }
    }

    /// The owner's icon mapping for the top-level rows: every named entry carries the
    /// shipped asset it was assigned; the submenu parents' icons come from their kinds.
    #[test]
    fn top_level_rows_carry_the_owners_icons() {
        let items = tray_menu(false, false);
        for item in &items {
            match item {
                TrayItem::Action { item, .. } => {
                    let expected = match item.action {
                        CaptureAction::ColorPicker => MenuIcon::ColorPicker,
                        CaptureAction::Scan => MenuIcon::Scanner,
                        CaptureAction::Settings => MenuIcon::Settings,
                        CaptureAction::Quit => MenuIcon::Quit,
                        other => panic!("unexpected top-level action {other:?}"),
                    };
                    assert_eq!(item.icon, Some(expected), "{:?}", item.label);
                }
                TrayItem::Launchers { menu, .. } => {
                    let expected = match menu {
                        LauncherSubmenu::Capture => MenuIcon::Capture,
                        LauncherSubmenu::Record => MenuIcon::Record,
                    };
                    assert_eq!(menu.icon(), expected);
                }
                TrayItem::Radio { menu, .. } => {
                    let expected = match menu {
                        RadioSubmenu::Countdown => MenuIcon::Countdown,
                        RadioSubmenu::Audio => MenuIcon::Audio,
                    };
                    assert_eq!(menu.icon(), expected);
                }
                TrayItem::Control(_) | TrayItem::Separator => {}
            }
        }
        // The radio ROWS carry no icon (CountdownItem / AudioArmItem have no icon
        // field): the toggle indicator and an icon fight for the same slot on several
        // hosts, so the guarantee is structural. The in-recording CONTROLS carry the
        // three-way tints; `the_recording_controls_carry_the_three_way_tints` pins them.
    }

    /// The in-recording controls carry their glyphs (pause / play flip, save, trash)
    /// and the owner's THREE-WAY tint rule: Pause and Resume are ordinary
    /// accent-tinted rows, Finish is the app's success green, Discard stays
    /// [`RECORDING_RED_RGB`]; every other icon keeps the accent it was given.
    #[test]
    fn the_recording_controls_carry_the_three_way_tints() {
        let items = recording_menu(false);
        assert_eq!(
            items.iter().map(|i| i.icon).collect::<Vec<_>>(),
            vec![Some(MenuIcon::Pause), Some(MenuIcon::Finish), Some(MenuIcon::Discard)]
        );
        // Paused flips the pause control's icon to the play (resume) glyph and
        // nothing else.
        let paused = recording_menu(true);
        assert_eq!(paused[0].icon, Some(MenuIcon::Resume));
        assert_eq!(&paused[1..], &items[1..]);
        let accent = [0x12, 0x34, 0x56];
        // Pause and its Resume flip: STANDARD rows (no fixed tint; template on mac).
        for icon in [MenuIcon::Pause, MenuIcon::Resume] {
            assert_eq!(menu_icon_fixed_tint(icon), None, "{icon:?} is an ordinary row");
            assert_eq!(menu_icon_tint(icon, accent), accent);
        }
        // Finish & Save: the app's success green, whatever the accent.
        assert_eq!(menu_icon_fixed_tint(MenuIcon::Finish), Some(SUCCESS_GREEN_RGB));
        assert_eq!(menu_icon_tint(MenuIcon::Finish, accent), SUCCESS_GREEN_RGB);
        // Cancel & Delete: stays the recording red, whatever the accent.
        assert_eq!(menu_icon_fixed_tint(MenuIcon::Discard), Some(RECORDING_RED_RGB));
        assert_eq!(menu_icon_tint(MenuIcon::Discard, accent), RECORDING_RED_RGB);
        // The three fixed states are mutually distinct, so the mapping cannot
        // silently collapse.
        assert_ne!(SUCCESS_GREEN_RGB, RECORDING_RED_RGB);
        for icon in [
            MenuIcon::Scanner,
            MenuIcon::Capture,
            MenuIcon::Record,
            MenuIcon::Countdown,
            MenuIcon::Audio,
            MenuIcon::Settings,
            MenuIcon::Quit,
            MenuIcon::Region,
            MenuIcon::Window,
            MenuIcon::Monitor,
        ] {
            assert_eq!(menu_icon_fixed_tint(icon), None, "{icon:?} is not a fixed-tint glyph");
            assert_eq!(menu_icon_tint(icon, accent), accent);
        }
    }

    /// [`SUCCESS_GREEN_RGB`] IS the app's canonical success colour
    /// (`app::theme::SUCCESS`, the value the upload meter turns on completion),
    /// byte for byte, so the two spellings can never drift.
    #[test]
    fn success_green_bytes_are_the_theme_success_colour() {
        let c = crate::app::theme::SUCCESS;
        let to_byte = |v: f32| (v * 255.0).round() as u8;
        assert_eq!(SUCCESS_GREEN_RGB, [to_byte(c.r), to_byte(c.g), to_byte(c.b)]);
    }

    /// Every menu icon embeds a real Lucide SVG in the house format, and the shared
    /// tint helper replaces every `currentColor` so no rasterizer sees an unresolved
    /// paint.
    #[test]
    fn every_menu_icon_embeds_and_tints() {
        let all = [
            MenuIcon::Scanner,
            MenuIcon::Capture,
            MenuIcon::Record,
            MenuIcon::Countdown,
            MenuIcon::Audio,
            MenuIcon::Settings,
            MenuIcon::Quit,
            MenuIcon::Region,
            MenuIcon::Window,
            MenuIcon::Monitor,
            MenuIcon::Pause,
            MenuIcon::Resume,
            MenuIcon::Finish,
            MenuIcon::Discard,
        ];
        for icon in all {
            let svg = icon.svg();
            assert!(svg.starts_with("<svg"), "{icon:?} is not an SVG");
            assert!(svg.contains("currentColor"), "{icon:?} lost its currentColor strokes");
            assert!(svg.contains(r#"viewBox="0 0 24 24""#), "{icon:?} left the 24-unit box");
            let tinted = menu_icon_svg_tinted(icon, [0x12, 0xab, 0xef]);
            assert!(!tinted.contains("currentColor"), "{icon:?} tint left a currentColor");
            assert!(tinted.contains("#12abef"), "{icon:?} tint missing the colour");
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn capture_action_spawn_args_match_the_cli_launch_flags() {
        // Every launcher / Settings action maps to the CLI argv the resident spawns a
        // one-shot child with; Quit never spawns (it tears the resident down). The
        // record entries (DRAGON-559) are their capture twins plus `--video` — the
        // CLI.md composition, not a new mode flag — and carry NO `--audio` and NO
        // `--countdown` flag, so a tray-launched recording reads the persisted arms and
        // countdown the tray's own submenus manage.
        assert_eq!(CaptureAction::ColorPicker.spawn_args(), Some(&["--color-picker"][..]));
        assert_eq!(CaptureAction::Scan.spawn_args(), Some(&["--scan"][..]));
        assert_eq!(CaptureAction::Region.spawn_args(), Some(&["--region"][..]));
        assert_eq!(CaptureAction::Window.spawn_args(), Some(&["--window"][..]));
        assert_eq!(CaptureAction::Monitor.spawn_args(), Some(&["--monitor"][..]));
        assert_eq!(
            CaptureAction::RecordRegion.spawn_args(),
            Some(&["--region", "--video"][..])
        );
        assert_eq!(
            CaptureAction::RecordWindow.spawn_args(),
            Some(&["--window", "--video"][..])
        );
        assert_eq!(
            CaptureAction::RecordMonitor.spawn_args(),
            Some(&["--monitor", "--video"][..])
        );
        assert_eq!(CaptureAction::Settings.spawn_args(), Some(&["--settings"][..]));
        assert_eq!(CaptureAction::Quit.spawn_args(), None);
    }

    #[test]
    fn no_menu_label_contains_a_dash() {
        let no_dash = |s: &str| !s.contains('\u{2014}') && !s.contains('\u{2013}');
        for i in recording_menu(false) {
            assert!(no_dash(i.label), "dash in {:?}", i.label);
        }
        for recording in [false, true] {
            for item in tray_menu(recording, false) {
                let (label, _) = shape(&item);
                assert!(no_dash(&label), "dash in {label:?}");
                if let TrayItem::Launchers { items, .. } = &item {
                    for i in items {
                        assert!(no_dash(i.label), "dash in {:?}", i.label);
                    }
                }
            }
        }
        for state in AUDIO_ARM_ORDER {
            assert!(no_dash(state.word()), "dash in {:?}", state.word());
            assert!(no_dash(state.radio_label()), "dash in {:?}", state.radio_label());
            assert!(no_dash(&audio_submenu_title(state)));
        }
        for idx in 0..COUNTDOWN_PRESET_SECS.len() {
            assert!(no_dash(&countdown_submenu_title(idx)));
        }
        for row in countdown_items(0) {
            assert!(no_dash(&row.label), "dash in {:?}", row.label);
        }
    }
}

#[cfg(test)]
mod countdown_submenu_tests {
    use super::*;

    /// The owner's spec: four presets, zero-padded two-digit labels, index = the
    /// persisted `delay_idx`.
    #[test]
    fn the_presets_and_labels_are_zero_padded() {
        assert_eq!(COUNTDOWN_PRESET_SECS, [0, 3, 5, 10]);
        let rows = countdown_items(2);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["00", "03", "05", "10"]);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row.idx, i, "the row persists its own display index");
        }
        let marked: Vec<usize> = rows.iter().filter(|r| r.selected).map(|r| r.idx).collect();
        assert_eq!(marked, vec![2], "exactly the current index carries the mark");
    }

    /// The TITLE is the read-without-opening surface: "Countdown Timer: NN", zero
    /// padded, "00" meaning off; a stray index clamps like the app's own `PickDelay`.
    #[test]
    fn the_title_carries_the_current_value_zero_padded() {
        assert_eq!(countdown_submenu_title(0), "Countdown Timer: 00");
        assert_eq!(countdown_submenu_title(1), "Countdown Timer: 03");
        assert_eq!(countdown_submenu_title(2), "Countdown Timer: 05");
        assert_eq!(countdown_submenu_title(3), "Countdown Timer: 10");
        assert_eq!(countdown_submenu_title(99), "Countdown Timer: 10", "stray idx clamps");
        // A stray current index also clamps in the row mark.
        let marked: Vec<usize> =
            countdown_items(99).iter().filter(|r| r.selected).map(|r| r.idx).collect();
        assert_eq!(marked, vec![3]);
    }

    /// The index space ksni's radio callback reports back into matches the display
    /// order, and a stray index persists nothing.
    #[test]
    fn choice_at_index_is_a_bounds_check() {
        for idx in 0..COUNTDOWN_PRESET_SECS.len() {
            assert_eq!(countdown_choice_at(idx), Some(idx));
        }
        assert_eq!(countdown_choice_at(4), None);
        assert_eq!(countdown_choice_at(usize::MAX), None);
    }
}

/// DRAGON-574's frozen-frame rule, and DRAGON-598's guarantee that no entry can fall out
/// of it: a menu row that spawns a child waits out the dropdown first. DRAGON-600 kept the
/// rule and pinned the WAIT as a bound rather than a mechanism, because on COSMIC there is
/// no menu-closed signal to wait on; see [`MENU_DISMISS_DELAY`].
#[cfg(test)]
mod menu_dismiss_tests {
    use super::*;

    /// THE property, walked over the model in both states: every row that can spawn a
    /// child waits, and the only row that does not wait is the one with nothing to spawn.
    ///
    /// This is why a new entry cannot repeat DRAGON-582's near miss. The old test asserted
    /// the predicate against a copy of its own body (`!matches!(…, Settings | Quit)`), so
    /// it agreed with any answer the predicate gave; this one asserts it against
    /// [`CaptureAction::spawn_args`], which is the independent fact.
    #[test]
    fn every_menu_row_that_spawns_a_child_waits_for_the_dismiss() {
        let mut seen = 0;
        for recording in [false, true] {
            for entry in tray_menu(recording, false) {
                let rows: Vec<CaptureItem> = match entry {
                    TrayItem::Action { item, .. } => vec![item],
                    TrayItem::Launchers { items, .. } => items,
                    _ => Vec::new(),
                };
                for row in rows {
                    seen += 1;
                    assert_eq!(
                        spawn_waits_for_menu_dismiss(row.action),
                        row.action.spawn_args().is_some(),
                        "{:?} ({}): waiting and spawning must be the same question",
                        row.action,
                        row.label
                    );
                }
            }
        }
        assert!(seen >= 18, "the walk covered only {seen} rows, so it proves little");
    }

    /// Named one by one, because a table walk cannot catch a row that is missing from the
    /// menu entirely. The colour picker is FIRST: it is the entry DRAGON-598 was raised
    /// for, and it grabs a full-screen snapshot it then shows the user at 12x, which is
    /// the surface that makes a stale dropdown legible.
    #[test]
    fn every_launching_entry_is_named_and_waits() {
        for action in [
            CaptureAction::ColorPicker,
            CaptureAction::Scan,
            CaptureAction::Region,
            CaptureAction::Window,
            CaptureAction::Monitor,
            CaptureAction::RecordRegion,
            CaptureAction::RecordWindow,
            CaptureAction::RecordMonitor,
            CaptureAction::Settings,
        ] {
            assert!(spawn_waits_for_menu_dismiss(action), "{action:?} launches, so it waits");
        }
        // Quit is the only exception, and it earns it structurally: no argv, no child.
        assert_eq!(CaptureAction::Quit.spawn_args(), None);
        assert!(!spawn_waits_for_menu_dismiss(CaptureAction::Quit));
    }

    /// macOS keeps a pre-spawn settle, and it is pinned so a tidy cannot quietly change
    /// it. It is legitimate THERE because AppKit dismisses the `NSMenu` before it sends
    /// the action, so the thing being waited out has already happened.
    #[test]
    fn the_mac_pre_spawn_settle_is_pinned() {
        assert_eq!(MENU_DISMISS_DELAY.as_millis(), 200);
    }

    /// The Linux marker's NAME is part of the contract between two processes: the resident
    /// sets it, the capture child reads it, and they are the same binary but not the same
    /// build in a mixed-artifact session (AppImage tray, Flatpak child). A rename on one
    /// side alone silently turns the hold off and the dropdown comes back.
    #[test]
    fn the_menu_launch_marker_name_is_pinned() {
        assert_eq!(MENU_LAUNCH_ENV, "CCK_TRAY_MENU_LAUNCH");
        assert!(MENU_LAUNCH_ENV.starts_with("CCK_"), "every env of ours carries the prefix");
    }
}

#[cfg(test)]
mod audio_flag_parse_tests {
    use super::*;

    /// The DRAGON-558 vocabulary plus its obvious spellings, case-insensitive: each
    /// accepted token maps to exactly the state its word means.
    #[test]
    fn the_accepted_spellings_map_to_their_states() {
        for (token, expected) in [
            ("both", AudioArmState::Both),
            ("all", AudioArmState::Both),
            ("mic+system", AudioArmState::Both),
            ("system+mic", AudioArmState::Both),
            ("microphone", AudioArmState::MicrophoneOnly),
            ("mic", AudioArmState::MicrophoneOnly),
            ("system", AudioArmState::SystemOnly),
            ("sys", AudioArmState::SystemOnly),
            ("none", AudioArmState::None),
            ("off", AudioArmState::None),
        ] {
            assert_eq!(AudioArmState::parse_flag(token), Some(expected), "{token:?}");
        }
    }

    /// Case does not matter (a shell user types what they type).
    #[test]
    fn parsing_is_case_insensitive() {
        assert_eq!(AudioArmState::parse_flag("Both"), Some(AudioArmState::Both));
        assert_eq!(AudioArmState::parse_flag("MIC"), Some(AudioArmState::MicrophoneOnly));
        assert_eq!(AudioArmState::parse_flag("System"), Some(AudioArmState::SystemOnly));
        assert_eq!(AudioArmState::parse_flag("NONE"), Some(AudioArmState::None));
    }

    /// Anything else is a reject, never a guess: a typo silently arming a channel (or
    /// silently NOT disarming one, for a user who asked for `none`) is the failure this
    /// parser exists to prevent. The caller turns `None` into a refused launch.
    #[test]
    fn unrecognised_values_are_rejected() {
        for token in [
            "", " ", "bth", "microphone+system", "mic-system", "mic ", " none", "true",
            "1", "--audio", "no", "sytem",
        ] {
            assert_eq!(AudioArmState::parse_flag(token), None, "{token:?} must not parse");
        }
    }
}

#[cfg(test)]
mod launch_arm_override_tests {
    use super::*;

    /// A given `--audio` state beats the persisted pair outright, whatever the pair was:
    /// the override is complete, never a per-channel merge.
    #[test]
    fn a_flag_beats_the_persisted_arms() {
        for persisted in [(false, false), (true, false), (false, true), (true, true)] {
            for choice in AUDIO_ARM_ORDER {
                assert_eq!(
                    launch_audio_arms(Some(choice), persisted),
                    choice.arms(),
                    "{choice:?} must win over {persisted:?}"
                );
            }
        }
    }

    /// An absent flag keeps the persisted arms exactly — the launch behaves as every
    /// launch did before the flag existed.
    #[test]
    fn an_absent_flag_keeps_the_persisted_arms() {
        for persisted in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(launch_audio_arms(None, persisted), persisted);
        }
    }
}

#[cfg(test)]
mod audio_arm_state_tests {
    use super::*;

    /// `from_arms` and `arms` are exact inverses over all four pairs, so a pick can never
    /// change meaning on the way through the model.
    #[test]
    fn arm_pairs_round_trip_through_the_state() {
        for mic in [false, true] {
            for system in [false, true] {
                let state = AudioArmState::from_arms(mic, system);
                assert_eq!(state.arms(), (mic, system), "{state:?} round-trips");
            }
        }
    }

    /// The submenu TITLE is the read-without-opening surface; pin the owner's exact
    /// wording for all four states.
    #[test]
    fn submenu_titles_carry_the_owners_state_words() {
        // The DRAGON-574 wording: the title word matches the renamed radio rows.
        assert_eq!(audio_submenu_title(AudioArmState::Both), "Audio Recording: Mic + System");
        assert_eq!(
            audio_submenu_title(AudioArmState::MicrophoneOnly),
            "Audio Recording: Mic Only"
        );
        assert_eq!(audio_submenu_title(AudioArmState::SystemOnly), "Audio Recording: System Only");
        assert_eq!(audio_submenu_title(AudioArmState::None), "Audio Recording: None");
    }

    /// The four radio rows: the owner's order and labels, with exactly the current state
    /// marked. Every renderer draws from this one list.
    #[test]
    fn radio_rows_are_the_four_choices_with_the_current_one_marked() {
        let rows = audio_arm_items(AudioArmState::SystemOnly);
        let labels: Vec<&str> = rows.iter().map(|r| r.label).collect();
        assert_eq!(labels, vec!["Mic + System", "Mic Only", "System Only", "None"]);
        let marked: Vec<AudioArmState> =
            rows.iter().filter(|r| r.selected).map(|r| r.choice).collect();
        assert_eq!(marked, vec![AudioArmState::SystemOnly], "exactly one row carries the mark");
        // Each row sets the complete state it is labelled with.
        for (row, expected) in rows.iter().zip(AUDIO_ARM_ORDER) {
            assert_eq!(row.choice, expected);
        }
    }

    /// The index space `ksni`'s radio callback reports back into matches the display
    /// order, and a stray index arms nothing.
    #[test]
    fn choice_at_index_follows_display_order_and_rejects_strays() {
        for (idx, expected) in AUDIO_ARM_ORDER.into_iter().enumerate() {
            assert_eq!(audio_arm_choice_at(idx), Some(expected));
        }
        assert_eq!(audio_arm_choice_at(4), None);
        assert_eq!(audio_arm_choice_at(usize::MAX), None);
    }
}

#[cfg(test)]
mod audio_toggle_route_tests {
    use super::*;

    /// One submenu, two meanings, ONE decision: live (apply to the recording) exactly
    /// while a recording is in progress, paused included; persisted-arm write only while
    /// idle.
    #[test]
    fn audio_picks_are_live_exactly_while_a_recording_exists() {
        assert!(audio_toggles_are_live(true));
        assert!(!audio_toggles_are_live(false));
    }

    /// The submenu titles/marks from the same source the pick routes to: live arms while
    /// recording, persisted arms while idle — never a mix.
    #[test]
    fn the_render_source_follows_the_same_split_as_the_pick_route() {
        let live = (true, false);
        let armed = (false, true);
        assert_eq!(audio_arm_source(true, live, armed), live);
        assert_eq!(audio_arm_source(false, live, armed), armed);
    }

    /// A live pick is the TOGGLE DIFF from the current arms to the chosen state: only the
    /// channels that differ get an action, so nothing double-flips and picking the current
    /// state is a no-op.
    #[test]
    fn live_pick_actions_are_the_exact_toggle_diff() {
        use RecordingAction::{ToggleMic, ToggleSystemAudio};
        // From (mic on, system off):
        let current = (true, false);
        assert_eq!(audio_pick_live_actions(current, AudioArmState::MicrophoneOnly), vec![]);
        assert_eq!(
            audio_pick_live_actions(current, AudioArmState::Both),
            vec![ToggleSystemAudio]
        );
        assert_eq!(audio_pick_live_actions(current, AudioArmState::None), vec![ToggleMic]);
        assert_eq!(
            audio_pick_live_actions(current, AudioArmState::SystemOnly),
            vec![ToggleMic, ToggleSystemAudio]
        );
        // Exhaustively: applying the diff as toggles always lands on the chosen state.
        for mic in [false, true] {
            for system in [false, true] {
                for choice in AUDIO_ARM_ORDER {
                    let (mut m, mut s) = (mic, system);
                    for action in audio_pick_live_actions((mic, system), choice) {
                        match action {
                            ToggleMic => m = !m,
                            ToggleSystemAudio => s = !s,
                            other => panic!("a pick never emits {other:?}"),
                        }
                    }
                    assert_eq!(
                        (m, s),
                        choice.arms(),
                        "toggles from ({mic},{system}) must land on {choice:?}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod countdown_tray_model_tests {
    use super::*;

    /// The byte form and the hex form of the recording red are two spellings of ONE
    /// colour; a drift between them would tint the countdown digits a different red than
    /// the recording glyph, which is exactly what DRAGON-563 forbids.
    #[test]
    fn the_rgb_bytes_are_the_recording_red_hex() {
        let hex = format!(
            "#{:02x}{:02x}{:02x}",
            RECORDING_RED_RGB[0], RECORDING_RED_RGB[1], RECORDING_RED_RGB[2]
        );
        assert_eq!(hex, RECORDING_RED);
    }

    /// The cancel entry and the tooltip: the owner's wording, dash-free, and the tooltip
    /// carries the remaining seconds it was asked about.
    #[test]
    fn the_cancel_label_and_tooltip_carry_the_owners_wording() {
        let no_dash = |s: &str| !s.contains('\u{2014}') && !s.contains('\u{2013}');
        assert_eq!(COUNTDOWN_CANCEL_LABEL, "Cancel countdown");
        assert!(no_dash(COUNTDOWN_CANCEL_LABEL));
        assert_eq!(countdown_tooltip(5), "Capture starts in 5s");
        assert_eq!(countdown_tooltip(10), "Capture starts in 10s");
        assert!(no_dash(&countdown_tooltip(3)));
    }
}
