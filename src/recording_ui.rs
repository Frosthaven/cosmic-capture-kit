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
//! * and the in-recording menu MODEL (`recording_menu`) — the ordered list of items,
//!   their labels, checkmarks, and roles — as data each backend maps onto its widgets.
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
    /// Toggle the microphone arm (checkmark reflects armed).
    ToggleMic,
    /// Toggle the system-audio arm (checkmark reflects armed).
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
    Checkmark(bool),
    /// A visual separator (no label, no action).
    Separator,
}

/// One item in the in-recording menu MODEL: its display label, how it presents, and the
/// action it fires. Separators carry an empty label and `RecordingAction::Stop` as an
/// inert placeholder (never wired — the kind is `Separator`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingItem {
    /// The menu label (empty for a separator). Never contains em/en-dashes.
    pub label: &'static str,
    /// How the item presents (standard / checkmark+state / separator).
    pub kind: MenuItemKind,
    /// The action the item fires when activated (unused for a separator).
    pub action: RecordingAction,
}

/// The in-recording control menu MODEL, in display order, for the given state. ONE
/// source for every surface (DRAGON-173 order): Toggle Microphone (check), Toggle System
/// Audio (check), a separator, Pause/Resume Recording, Finish & Save Recording, Cancel &
/// Delete Recording. EVERY control surface renders the SAME six entries and then, below a
/// separator, the "Capture Menu" submenu (see [`capture_menu`]) — the mac daemon, the Linux
/// resident, the Linux recording tray, and the mac child's own NSStatusItem are all
/// identical while recording. No per-surface parameterization: the model emits one
/// structure and every renderer renders all of it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn recording_menu(paused: bool, mic: bool, system: bool) -> Vec<RecordingItem> {
    vec![
        RecordingItem {
            label: "Toggle Microphone",
            kind: MenuItemKind::Checkmark(mic),
            action: RecordingAction::ToggleMic,
        },
        RecordingItem {
            label: "Toggle System Audio",
            kind: MenuItemKind::Checkmark(system),
            action: RecordingAction::ToggleSystemAudio,
        },
        RecordingItem {
            label: "",
            kind: MenuItemKind::Separator,
            action: RecordingAction::Stop,
        },
        RecordingItem {
            label: pause_label(paused),
            kind: MenuItemKind::Standard,
            action: RecordingAction::TogglePause,
        },
        RecordingItem {
            label: "Finish & Save Recording",
            kind: MenuItemKind::Standard,
            action: RecordingAction::Stop,
        },
        RecordingItem {
            label: "Cancel & Delete Recording",
            kind: MenuItemKind::Standard,
            action: RecordingAction::Cancel,
        },
    ]
}

// ── Capture-launcher menu model (the "Capture Menu" group) ────────────────────
//
// The capture launchers + Settings + Quit are the menu the daemon / resident show BEFORE a
// recording starts (flat), and — while a recording is in progress (DRAGON-173) — inside a
// "Capture Menu" SUBMENU rendered with the platform's native submenu arrow. EVERY
// in-recording control surface renders this submenu (the daemon, the resident, the Linux
// recording tray, AND the mac child's own NSStatusItem) so the in-recording menu is uniform
// everywhere. All uses draw from the ONE model below so the labels + order can never drift
// between the flat idle menu, the recording-time submenu, macOS, and Linux. On a
// child-owned recording surface the launchers spawn a fresh one-shot process (like the
// daemon does) and Quit renders permanently disabled (a child surface exists only while
// recording, where Quit is disabled anyway).

/// The submenu label the daemon / resident use to nest the capture group while recording.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const CAPTURE_MENU_LABEL: &str = "Capture Menu";

/// One capture-group action. Each renderer maps this onto its own spawn flag / selector
/// (the launchers spawn a capture child; Settings opens the settings window; Quit tears the
/// resident down). Quit's ENABLED state is the renderer's call (disabled while recording).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureAction {
    /// Scanner (forces Region + scan).
    Scan,
    /// Region capture.
    Region,
    /// Window capture.
    Window,
    /// Monitor capture.
    Monitor,
    /// Open the settings window.
    Settings,
    /// Quit the daemon / resident (disabled while recording).
    Quit,
}

/// One item in the capture-group MODEL: its label, how it presents (standard / separator),
/// and the action it fires. Separators carry an empty label and an inert placeholder action.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureItem {
    /// The menu label (empty for a separator). Never contains em/en-dashes.
    pub label: &'static str,
    /// How the item presents (standard / separator; the capture group has no checkmarks).
    pub kind: MenuItemKind,
    /// The action the item fires when activated (unused for a separator).
    pub action: CaptureAction,
}

/// The capture-group menu MODEL, in display order: Scanner, Capture Region, Capture Window,
/// Capture Monitor, a separator, Settings, Quit. ONE source for the daemon / resident idle
/// menu (rendered flat) AND the recording-time "Capture Menu" submenu (rendered nested) so
/// the two can never drift. The renderer decides Quit's enabled state (disabled while
/// recording) and maps each [`CaptureAction`] onto its spawn flag / selector.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn capture_menu() -> Vec<CaptureItem> {
    vec![
        CaptureItem { label: "Scanner", kind: MenuItemKind::Standard, action: CaptureAction::Scan },
        CaptureItem {
            label: "Capture Region",
            kind: MenuItemKind::Standard,
            action: CaptureAction::Region,
        },
        CaptureItem {
            label: "Capture Window",
            kind: MenuItemKind::Standard,
            action: CaptureAction::Window,
        },
        CaptureItem {
            label: "Capture Monitor",
            kind: MenuItemKind::Standard,
            action: CaptureAction::Monitor,
        },
        CaptureItem { label: "", kind: MenuItemKind::Separator, action: CaptureAction::Settings },
        CaptureItem {
            label: "Settings",
            kind: MenuItemKind::Standard,
            action: CaptureAction::Settings,
        },
        CaptureItem { label: "Quit", kind: MenuItemKind::Standard, action: CaptureAction::Quit },
    ]
}

impl CaptureAction {
    /// The single CLI flag a launcher / Settings action passes to a spawned one-shot child,
    /// or `None` for [`CaptureAction::Quit`] (which is never spawned — it tears the
    /// resident/daemon down, and on a child-owned surface it renders permanently disabled).
    /// ONE mapping shared by every surface's spawn path so the daemon, resident, and the
    /// child-owned recording trays launch byte-identical children.
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    pub fn spawn_flag(self) -> Option<&'static str> {
        match self {
            CaptureAction::Scan => Some("--scan"),
            CaptureAction::Region => Some("--region"),
            CaptureAction::Window => Some("--window"),
            CaptureAction::Monitor => Some("--monitor"),
            CaptureAction::Settings => Some("--settings"),
            CaptureAction::Quit => None,
        }
    }
}

/// Spawn the full app as a DETACHED one-shot child with `flag`. Detached (`setsid` on unix;
/// CREATE_NO_WINDOW + no-wait on Windows) so the child owns its own session — it cannot be
/// reaped as our child (no SIGCHLD churn) and the launcher process survives the child
/// exiting/crashing (and vice versa). Best-effort: a spawn failure just logs. This is the ONE
/// spawn path every recording control surface uses — the mac daemon, the Linux resident, the
/// Windows tray daemon (DRAGON-237), and the child-owned recording trays (`crate::tray`), so
/// a "Capture Menu" launch is byte-identical everywhere. Present on mac + Linux + Windows
/// (the platforms with a resident/tray surface); other platforms never call it.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub fn spawn_capture_child(flag: &str) {
    spawn_capture_child_with_env(flag, &[]);
}

/// Windows body of [`spawn_capture_child_with_env`] (DRAGON-237): the detach mechanism is
/// platform logic, so it lives under `platform/windows/` behind this thin dispatch (strict
/// split). Same contract as the unix body — a detached one-shot child, best-effort.
#[cfg(windows)]
pub fn spawn_capture_child_with_env(flag: &str, envs: &[(&str, &str)]) {
    crate::platform::windows::process::spawn_detached_child(flag, envs);
}

/// [`spawn_capture_child`] with extra environment variables for the child (e.g.
/// `CCK_SETTINGS_TAB=about` so a post-update settings child opens on About).
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn spawn_capture_child_with_env(flag: &str, envs: &[(&str, &str)]) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("spawn_capture_child: current_exe failed, cannot spawn {flag}: {e}");
            return;
        }
    };
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(flag);
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
        }
        Err(e) => log::warn!("spawn capture child {flag} failed: {e}"),
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
        // The DRAGON-173 order: Toggle Microphone (check), Toggle System Audio (check), a
        // separator, Pause/Resume Recording, Finish & Save, Cancel & Delete.
        let items = recording_menu(false, true, false);
        let labels: Vec<&str> = items.iter().map(|i| i.label).collect();
        assert_eq!(
            labels,
            vec![
                "Toggle Microphone",
                "Toggle System Audio",
                "", // separator
                "Pause Recording",
                "Finish & Save Recording",
                "Cancel & Delete Recording",
            ]
        );
        // Checkmarks track the passed state (mic on, system off).
        assert_eq!(items[0].kind, MenuItemKind::Checkmark(true));
        assert_eq!(items[1].kind, MenuItemKind::Checkmark(false));
        assert_eq!(items[2].kind, MenuItemKind::Separator);
        // Actions map 1:1.
        assert_eq!(items[0].action, RecordingAction::ToggleMic);
        assert_eq!(items[1].action, RecordingAction::ToggleSystemAudio);
        assert_eq!(items[3].action, RecordingAction::TogglePause);
        assert_eq!(items[4].action, RecordingAction::Stop);
        assert_eq!(items[5].action, RecordingAction::Cancel);
    }

    #[test]
    fn recording_menu_pause_label_follows_state() {
        // The Pause/Resume item is now at index 3 (after the two toggles + separator).
        assert_eq!(recording_menu(true, false, false)[3].label, "Resume Recording");
        assert_eq!(recording_menu(false, false, false)[3].label, "Pause Recording");
    }

    #[test]
    fn capture_menu_order_and_labels() {
        // The capture group (rendered flat when idle, nested under "Capture Menu" while
        // recording): Scanner, Capture Region, Capture Window, Capture Monitor, a separator,
        // Settings, Quit.
        let items = capture_menu();
        let labels: Vec<&str> = items.iter().map(|i| i.label).collect();
        assert_eq!(
            labels,
            vec![
                "Scanner",
                "Capture Region",
                "Capture Window",
                "Capture Monitor",
                "", // separator
                "Settings",
                "Quit",
            ]
        );
        assert_eq!(items[0].action, CaptureAction::Scan);
        assert_eq!(items[1].action, CaptureAction::Region);
        assert_eq!(items[2].action, CaptureAction::Window);
        assert_eq!(items[3].action, CaptureAction::Monitor);
        assert_eq!(items[4].kind, MenuItemKind::Separator);
        assert_eq!(items[5].action, CaptureAction::Settings);
        assert_eq!(items[6].action, CaptureAction::Quit);
        assert_eq!(CAPTURE_MENU_LABEL, "Capture Menu");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn capture_action_spawn_flags_match_the_cli_launch_flags() {
        // Every launcher / Settings action maps to the CLI flag the resident spawns a
        // one-shot child with; Quit never spawns (it tears the resident down).
        assert_eq!(CaptureAction::Scan.spawn_flag(), Some("--scan"));
        assert_eq!(CaptureAction::Region.spawn_flag(), Some("--region"));
        assert_eq!(CaptureAction::Window.spawn_flag(), Some("--window"));
        assert_eq!(CaptureAction::Monitor.spawn_flag(), Some("--monitor"));
        assert_eq!(CaptureAction::Settings.spawn_flag(), Some("--settings"));
        assert_eq!(CaptureAction::Quit.spawn_flag(), None);
    }

    #[test]
    fn no_menu_label_contains_a_dash() {
        for i in recording_menu(false, true, true) {
            assert!(
                !i.label.contains('\u{2014}') && !i.label.contains('\u{2013}'),
                "dash in {:?}",
                i.label
            );
        }
        for i in capture_menu() {
            assert!(
                !i.label.contains('\u{2014}') && !i.label.contains('\u{2013}'),
                "dash in {:?}",
                i.label
            );
        }
        assert!(!CAPTURE_MENU_LABEL.contains('\u{2014}') && !CAPTURE_MENU_LABEL.contains('\u{2013}'));
    }
}
