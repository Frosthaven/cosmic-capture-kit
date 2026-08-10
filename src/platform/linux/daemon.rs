//! Resident system-tray RESIDENT process (Linux, DRAGON-173).
//!
//! The Linux counterpart of the macOS menu-bar DAEMON (`crate::daemon`): a TINY
//! process — NO iced/cosmic/wgpu stack — that owns ONE `ksni`
//! StatusNotifierItem and spawns the FULL app as one-shot capture children. It is
//! the full-parity port of the mac resident experience: the same `resident` setting
//! turns it on, the same three-state bracket icon, the same in-recording menu group,
//! and the same child<->resident IPC (`crate::daemon_ipc`).
//!
//! ## Why a separate process, same binary
//!
//! `main()` early-branches here (BEFORE any GUI init) when: Linux, a bare launch (no
//! capture-mode / settings / preview / worker flags), and the persisted `resident`
//! setting is on. The resident never touches [`crate::app::run`], so none of the
//! iced/wgpu dependency graph is initialized — same technique the mac daemon uses to
//! keep its idle footprint tiny.
//!
//! ## What it owns
//!
//! * ONE `ksni` StatusNotifierItem showing the shared three-state viewfinder icon
//!   (corner brackets; + centre dot while a child records; + centre pause bars while
//!   paused, all from [`crate::recording_ui`]) and a menu built from the ONE portable
//!   [`crate::recording_ui::visible_tray_menu`] model (DRAGON-574 + the hide round),
//!   so the surfaces can never
//!   drift. Idle: Scanner, the "Capture" and "Record" submenus (Region / Window /
//!   Monitor trios), the "Countdown Timer: NN" radio submenu (persisting `delay_idx`,
//!   the same field every capture launch reads), the "Audio Recording: <state>" radio
//!   submenu (the DRAGON-558 persistent audio-arm control), separator, Settings...,
//!   Quit. While a child records: the three controls lead (Pause/Resume Recording,
//!   Finish & Save Recording, Cancel & Delete Recording), then the same group with the
//!   flagged-off rows HIDDEN — no Record, no Countdown Timer, no Quit; Scanner /
//!   Capture / Audio Recording (live arms) / Settings remain.
//! * The recording-control IPC socket ([`crate::daemon_ipc::socket_path`]): a recording
//!   child connects at start, streams its [`RecordingState`], and receives the menu's
//!   [`Command`]s back — identical wire protocol to the mac daemon.
//!
//! Unlike the mac daemon there is NO global hotkey here: on Linux the PrintScreen
//! capture key is a COSMIC custom shortcut the user owns (it spawns `target/release`
//! directly), so the resident only offers the menu launchers. A second bare `resident`
//! launch still asks the running resident to capture (SIGUSR1), matching mac.
//!
//! ## Lifecycle coordination (see also [`crate::instance`])
//!
//! * The resident takes the shared resident single-instance lock at startup (its own
//!   lock file; the capture children it spawns take no lock at all since DRAGON-351)
//!   and installs the
//!   SIGUSR1 handler FIRST THING, so a second bare launch that finds the lock held can
//!   SIGUSR1 us → we spawn the default capture child → the second process exits.
//! * `SetResident(true)` in the settings UI spawns this resident detached (the tray item
//!   appears at once); `SetResident(false)` SIGTERMs the lock-holder (us) so the item
//!   disappears immediately (a SIGTERM handler shuts the ksni item down and exits).
//! * A capture child crashing or exiting NEVER affects the resident — children are
//!   spawned fully detached (their own session), so there is no SIGCHLD to reap.
//!
//! Everything here is `#[cfg(target_os = "linux")]`; other platforms never compile it.

#![cfg(target_os = "linux")]

use crate::daemon_ipc::{icon_state, Command, IconState, RecordingState};
use crate::recording_ui::{
    visible_tray_menu, CaptureAction, MenuItemKind, RadioSubmenu, RecordingAction, TrayItem,
};
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The CLI flag the SIGUSR1 "capture NOW" default spawns (Region). The menu launchers spawn
/// via [`crate::recording_ui::CaptureAction::spawn_args`] (the ONE shared mapping); this
/// constant names only the second-launch default so both agree on it.
const REGION_FLAG: &str = "--region";

/// Spawn the full app as a DETACHED one-shot child with `flag`. A thin wrapper over the ONE
/// portable spawn path ([`crate::recording_ui::spawn_capture_child`]) so the resident, the
/// mac daemon, and the child-owned recording trays launch byte-identical detached children.
/// Best-effort: a spawn failure just logs (the tray keeps working).
fn spawn_child(flag: &str) {
    crate::recording_ui::spawn_capture_child(flag);
}

// ── SIGUSR1: a second bare launch asks us to capture NOW ─────────────────────
//
// A second bare `resident` launch finds the resident lock held (by us) and SIGUSR1s
// our recorded pid instead of duplicating. The handler is async-signal-safe (one atomic
// store); the servicing thread drains it and spawns the default capture child. Installed
// FIRST THING in `run` so it's armed before we record our pid.

static USR1_PENDING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigusr1(_sig: libc::c_int) {
    // Async-signal-safe: a single atomic store, nothing more.
    USR1_PENDING.store(true, Ordering::SeqCst);
}

/// Install the SIGUSR1 handler. Called once, first thing in [`run`].
fn install_sigusr1() {
    // Coerce to a typed fn POINTER first, then to the handler int — a direct
    // fn-item→int cast trips clippy::fn_to_numeric_cast.
    let handler: extern "C" fn(libc::c_int) = on_sigusr1;
    // SAFETY: installing a plain, async-signal-safe C handler for SIGUSR1.
    unsafe {
        libc::signal(libc::SIGUSR1, handler as usize as libc::sighandler_t);
    }
}

// ── SIGTERM: SetResident(false) asks us to quit ──────────────────────────────
//
// `SetResident(false)` (and a capture-hotkey / autostart change) SIGTERMs the resident
// lock-holder (us) so the tray item disappears at once (`crate::instance::signal_daemon_quit`).
// AppKit auto-handles SIGTERM on mac; here we install our own async-signal-safe handler
// that flags a quit, drained by the trigger thread which shuts the ksni item down and
// exits cleanly (dropping the handle removes the item; a bare default SIGTERM would kill
// us before the D-Bus item is torn down, leaving a ghost until the host times out).

static TERM_PENDING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigterm(_sig: libc::c_int) {
    // Async-signal-safe: a single atomic store, nothing more.
    TERM_PENDING.store(true, Ordering::SeqCst);
}

/// Install the SIGTERM handler. Called once, alongside [`install_sigusr1`].
fn install_sigterm() {
    let handler: extern "C" fn(libc::c_int) = on_sigterm;
    // SAFETY: installing a plain, async-signal-safe C handler for SIGTERM.
    unsafe {
        libc::signal(libc::SIGTERM, handler as usize as libc::sighandler_t);
    }
}

// ── The resident tray item ───────────────────────────────────────────────────

/// Shared IPC state the socket thread writes and the tray render reads: the last
/// recording state a connected child reported, and the write half of the child's
/// connection so menu clicks can send commands back. Cloneable `Arc` handles held by
/// the socket thread and the tray.
#[derive(Clone, Default)]
struct IpcShared {
    /// Last recording state the connected child reported (idle when no child).
    state: Arc<Mutex<RecordingState>>,
    /// Write half of the connected child's socket, `None` when no child is attached. A
    /// menu click writes a `Command` line here; a closed pipe drops the send.
    child_tx: Arc<Mutex<Option<UnixStream>>>,
}

impl IpcShared {
    /// Send a command to the connected child (best-effort). A missing / broken
    /// connection silently drops it — the child will report fresh state or disconnect.
    fn send(&self, cmd: Command) {
        if let Ok(mut guard) = self.child_tx.lock()
            && let Some(stream) = guard.as_mut()
        {
            if stream.write_all(cmd.encode().as_bytes()).is_err() {
                // The child is gone; drop the write half so we stop trying.
                *guard = None;
            } else {
                let _ = stream.flush();
            }
        }
    }

    /// A snapshot of the last reported recording state (idle if the lock is poisoned).
    fn snapshot(&self) -> RecordingState {
        self.state.lock().map(|g| *g).unwrap_or_default()
    }
}

/// The resident StatusNotifierItem: the shared IPC state (recording snapshot + the
/// child command channel) and the app's accent colour (the icon tint, matching the
/// recording tray + toolbar buttons), plus a per-`IconState` render cache so a host
/// re-querying `icon_pixmap` on every panel redraw doesn't re-run the usvg parse.
/// One cached icon render: the (state, tint) it was rendered for, plus the pixmaps.
/// Keyed on the accent too (DRAGON-179): the trigger loop live-updates the tray's
/// accent when the user changes the app's Theme override, and the cached render
/// must not survive that.
type IconCacheEntry = ((IconState, [u8; 3]), Vec<ksni::Icon>);

struct ResidentTray {
    ipc: IpcShared,
    accent: [u8; 3],
    icon_cache: std::cell::RefCell<Option<IconCacheEntry>>,
    /// The persisted audio arms (`record_mic` / `record_system_audio`), the state the
    /// "Audio Recording: <state>" submenu titles and marks itself from while IDLE
    /// (DRAGON-558). CACHED here rather than read per menu build, for the same reason the
    /// accent is: a config read does not belong on the ksni thread inside a menu callback.
    /// Kept fresh two ways: the trigger loop's config-mtime tick re-reads them (so a
    /// settings window's change lands within ~1s), and an idle radio pick updates them
    /// directly with the pair it just persisted.
    armed_mic: bool,
    armed_system: bool,
    /// The persisted countdown preset index (`delay_idx`), the state the "Countdown
    /// Timer: NN" submenu titles and marks itself from (DRAGON-574). Cached and kept
    /// fresh exactly like the audio arms above (mtime tick + the idle pick's own write).
    delay_idx: usize,
}

impl ResidentTray {
    /// The three-state icon for the current recording snapshot (shared with the mac
    /// daemon via [`crate::recording_ui`]): Idle brackets when no child records, + a
    /// centre dot while recording, + centre pause bars while paused.
    fn icon_state(&self) -> IconState {
        let st = self.ipc.snapshot();
        icon_state(&st)
    }
}

impl ksni::Tray for ResidentTray {
    /// A left-click opens the menu (all launchers + in-recording controls live there)
    /// instead of firing a single action, so every control is one click away.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "dev.thedragon.CosmicCaptureKit.Resident".to_string()
    }

    fn title(&self) -> String {
        "Cosmic Capture Kit".to_string()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let key = (self.icon_state(), self.accent);
        if let Some((cached_key, icons)) = &*self.icon_cache.borrow()
            && *cached_key == key
        {
            return icons.clone();
        }
        let icons =
            crate::tray::render_state_icon(key.0, self.accent).map(|i| vec![i]).unwrap_or_default();
        *self.icon_cache.borrow_mut() = Some((key, icons.clone()));
        icons
    }

    /// The resident menu: the ONE portable [`visible_tray_menu`] model (DRAGON-574 +
    /// the hide round), both
    /// states, so labels + order + visibility match the mac daemon, the Windows daemon
    /// and the Linux recording tray exactly (see [`resident_menu_shapes`] for the
    /// pinned shape). Idle: Scanner, the Capture / Record submenus, the Countdown
    /// Timer and Audio Recording radio submenus, Settings..., Quit. While a child
    /// records: the three controls lead, then the same group with Record / Countdown /
    /// Quit HIDDEN.
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let st = self.ipc.snapshot();
        // The pair the Audio Recording submenu titles/marks itself from: the live child
        // arms while recording, the cached persisted arms while idle — the ONE shared
        // split, so the submenu can never render a different source than a pick routes to.
        let arms = crate::recording_ui::audio_arm_source(
            st.recording,
            (st.mic, st.system),
            (self.armed_mic, self.armed_system),
        );
        build_menu_items(&st, arms, self.delay_idx, self.accent)
    }
}

/// Relay one shared in-recording action to the connected child over the IPC socket.
/// Module-level (not nested in `menu`) since DRAGON-558, because a live Audio Recording
/// radio pick relays its toggle diff through it too ([`audio_pick`]).
fn relay(t: &ResidentTray, action: RecordingAction) {
    let cmd = match action {
        RecordingAction::TogglePause => Command::TogglePause,
        RecordingAction::ToggleMic => Command::ToggleMic,
        RecordingAction::ToggleSystemAudio => Command::ToggleSystemAudio,
        RecordingAction::Stop => Command::Stop,
        RecordingAction::Cancel => Command::Cancel,
    };
    t.ipc.send(cmd);
}

/// An "Audio Recording" radio pick (DRAGON-558), routed by the portable decision: while a
/// child records, relay the TOGGLE DIFF from its current live arms to the picked state
/// (gain automation, reversible; the child's own handler persists each flip, so the saved
/// default follows); while idle, write the picked pair as the PERSISTED arms a new
/// recording reads at start and update the cached render state with what was written. The
/// recording snapshot is read FRESH at pick time (not baked into the closure at menu
/// build), so a menu opened idle and picked after a recording started still routes right.
fn audio_pick(t: &mut ResidentTray, choice: crate::recording_ui::AudioArmState) {
    let st = t.ipc.snapshot();
    if crate::recording_ui::audio_toggles_are_live(st.recording) {
        for action in crate::recording_ui::audio_pick_live_actions((st.mic, st.system), choice) {
            relay(t, action);
        }
    } else {
        let (mic, system) = crate::recording_ui::persist_audio_arms(choice);
        t.armed_mic = mic;
        t.armed_system = system;
    }
}

/// A "Countdown Timer" radio pick (DRAGON-574), the countdown twin of [`audio_pick`].
/// The submenu is disabled while a child records, but the guard re-checks a FRESH
/// snapshot anyway (some hosts still deliver picks into disabled submenus, and a menu
/// opened idle can be picked after a recording started). Idle: persist the preset index
/// (`delay_idx`, the same field every capture launch reads) and update the cached render
/// state with what was written.
fn countdown_pick(t: &mut ResidentTray, index: usize) {
    let st = t.ipc.snapshot();
    if st.recording {
        return;
    }
    if let Some(idx) = crate::recording_ui::countdown_choice_at(index) {
        t.delay_idx = crate::recording_ui::persist_countdown_idx(idx);
    }
}

/// The whole resident dropdown as ksni items, walked from the ONE portable
/// [`visible_tray_menu`] model (DRAGON-574; the flagged-off rows are HIDDEN, never
/// rendered). The resident's own routing: controls relay over the
/// IPC socket, launchers spawn detached one-shot children, the audio radio picks route
/// through [`audio_pick`], the countdown picks through [`countdown_pick`], and Quit
/// (label "Quit Cosmic Capture Kit Tray", present only while the model shows it,
/// [`quit_enabled`] pins the same rule) asks the trigger thread for the clean shutdown.
fn build_menu_items(
    st: &RecordingState,
    arms: (bool, bool),
    delay_idx: usize,
    accent: [u8; 3],
) -> Vec<ksni::MenuItem<ResidentTray>> {
    use ksni::menu::{StandardItem, SubMenu};
    let menu_icon = |icon| crate::tray::menu_icon_png(icon, accent);
    let mut items: Vec<ksni::MenuItem<ResidentTray>> = Vec::new();
    for entry in visible_tray_menu(st.recording, st.paused) {
        match entry {
            TrayItem::Separator => items.push(ksni::MenuItem::Separator),
            TrayItem::Control(control) => match control.kind {
                MenuItemKind::Separator => items.push(ksni::MenuItem::Separator),
                MenuItemKind::Standard | MenuItemKind::Checkmark(_) => {
                    let action = control.action;
                    items.push(
                        StandardItem {
                            label: control.label.to_string(),
                            // The control rows: the shared three-way rule (Finish
                            // success-green, Discard red, Pause/Resume the accent).
                            icon_data: menu_icon(control.icon),
                            activate: Box::new(move |t: &mut ResidentTray| relay(t, action)),
                            ..Default::default()
                        }
                        .into(),
                    );
                }
            },
            TrayItem::Action { item, enabled } => match item.action {
                CaptureAction::Quit => items.push(
                    StandardItem {
                        label: "Quit Cosmic Capture Kit Tray".to_string(),
                        enabled,
                        icon_data: menu_icon(item.icon),
                        activate: Box::new(|_t: &mut ResidentTray| {
                            // Ask the trigger thread to tear the item down + exit (a menu
                            // click runs on the ksni thread; do the clean shutdown centrally).
                            TERM_PENDING.store(true, Ordering::SeqCst);
                        }),
                        ..Default::default()
                    }
                    .into(),
                ),
                action => {
                    // A launcher / Settings: spawn a one-shot child via the ONE portable
                    // path. `spawn_args` because a record entry's argv is its capture
                    // twin's flag plus `--video` (DRAGON-559). A row that spawns a child
                    // defers past the menu dismiss (DRAGON-574: the dropdown was getting
                    // baked into the fallback's frozen frame; DRAGON-598 derived the set
                    // from `spawn_args` so a new row cannot fall out of it). DRAGON-600
                    // moved WHERE that wait happens: the host owns the dropdown and closes
                    // it only when the launched process takes keyboard focus, so the child
                    // is tagged and does the waiting, and the spawn itself is immediate.
                    let args = action.spawn_args().unwrap_or(&[REGION_FLAG]);
                    let from_menu = crate::recording_ui::spawn_waits_for_menu_dismiss(action);
                    items.push(
                        StandardItem {
                            label: item.label.to_string(),
                            enabled,
                            icon_data: menu_icon(item.icon),
                            activate: Box::new(move |_t: &mut ResidentTray| {
                                if from_menu {
                                    crate::recording_ui::spawn_capture_child_args_from_menu(
                                        args,
                                        Vec::new(),
                                    );
                                } else {
                                    crate::recording_ui::spawn_capture_child_args(args, &[]);
                                }
                            }),
                            ..Default::default()
                        }
                        .into(),
                    );
                }
            },
            TrayItem::Launchers { menu, enabled, items: rows } => items.push(
                SubMenu {
                    label: menu.label().to_string(),
                    enabled,
                    icon_data: menu_icon(Some(menu.icon())),
                    submenu: rows
                        .into_iter()
                        .map(|row| {
                            let args = row.action.spawn_args().unwrap_or(&[REGION_FLAG]);
                            StandardItem {
                                label: row.label.to_string(),
                                // Belt and braces: a host that opens a disabled submenu
                                // still gets inert rows.
                                enabled,
                                icon_data: menu_icon(row.icon),
                                // Every launcher row grabs pixels, so every one is tagged
                                // as menu-launched (DRAGON-574, DRAGON-600).
                                activate: Box::new(move |_t: &mut ResidentTray| {
                                    crate::recording_ui::spawn_capture_child_args_from_menu(
                                        args,
                                        Vec::new(),
                                    );
                                }),
                                ..Default::default()
                            }
                            .into()
                        })
                        .collect(),
                    ..Default::default()
                }
                .into(),
            ),
            TrayItem::Radio { menu: RadioSubmenu::Audio, enabled } => {
                items.push(audio_submenu_items(arms, enabled, accent));
            }
            TrayItem::Radio { menu: RadioSubmenu::Countdown, enabled } => {
                items.push(countdown_submenu_items(delay_idx, enabled, accent));
            }
        }
    }
    items
}

/// The "Audio Recording: <state>" radio SUBMENU as one ksni item (DRAGON-558): the title
/// carries the current state so it reads without opening, and inside is ONE
/// `ksni::menu::RadioGroup` with the four portable rows in [`AUDIO_ARM_ORDER`] order
/// ("Mic + System", "Mic Only", "System Only", "None"), the current state marked. A pick
/// maps its index back through [`crate::recording_ui::audio_arm_choice_at`] (a stray
/// index arms nothing) and routes via [`audio_pick`].
fn audio_submenu_items(
    arms: (bool, bool),
    enabled: bool,
    accent: [u8; 3],
) -> ksni::MenuItem<ResidentTray> {
    use crate::recording_ui::{
        audio_arm_choice_at, audio_arm_items, audio_submenu_title, AudioArmState,
        AUDIO_ARM_ORDER,
    };
    use ksni::menu::{RadioGroup, RadioItem, SubMenu};
    let current = AudioArmState::from_arms(arms.0, arms.1);
    let rows = audio_arm_items(current);
    let selected = AUDIO_ARM_ORDER.iter().position(|s| *s == current).unwrap_or(0);
    SubMenu {
        label: audio_submenu_title(current),
        enabled,
        icon_data: crate::tray::menu_icon_png(Some(RadioSubmenu::Audio.icon()), accent),
        submenu: vec![RadioGroup {
            selected,
            select: Box::new(|t: &mut ResidentTray, index: usize| {
                if let Some(choice) = audio_arm_choice_at(index) {
                    audio_pick(t, choice);
                }
            }),
            options: rows
                .into_iter()
                .map(|row| RadioItem { label: row.label.to_string(), ..Default::default() })
                .collect(),
        }
        .into()],
        ..Default::default()
    }
    .into()
}

/// The "Countdown Timer: NN" radio SUBMENU as one ksni item (DRAGON-574): the title
/// carries the current persisted preset zero-padded ("00" = off) so it reads without
/// opening, and inside is ONE `RadioGroup` with the four preset rows, the current one
/// marked. A pick maps its index back through
/// [`crate::recording_ui::countdown_choice_at`] (a stray index persists nothing) and
/// routes via [`countdown_pick`].
fn countdown_submenu_items(
    delay_idx: usize,
    enabled: bool,
    accent: [u8; 3],
) -> ksni::MenuItem<ResidentTray> {
    use crate::recording_ui::{countdown_items, countdown_submenu_title};
    use ksni::menu::{RadioGroup, RadioItem, SubMenu};
    let rows = countdown_items(delay_idx);
    let selected = rows.iter().position(|r| r.selected).unwrap_or(0);
    SubMenu {
        label: countdown_submenu_title(delay_idx),
        enabled,
        icon_data: crate::tray::menu_icon_png(Some(RadioSubmenu::Countdown.icon()), accent),
        submenu: vec![RadioGroup {
            selected,
            select: Box::new(|t: &mut ResidentTray, index: usize| countdown_pick(t, index)),
            options: rows
                .into_iter()
                .map(|row| RadioItem { label: row.label, ..Default::default() })
                .collect(),
        }
        .into()],
        ..Default::default()
    }
    .into()
}

/// The TOP-LEVEL resident menu as `(label, enabled)` pairs, in order, for a given state,
/// cached persisted `armed` pair and cached `delay_idx`. Mirrors what
/// [`ResidentTray::menu`] builds — the identical [`visible_tray_menu`] walk (hidden
/// rows absent) with the resident's label overrides (the full Quit label, the radio
/// submenu titles) — so the whole composition is unit-testable without a D-Bus / ksni
/// host. `#[cfg(test)]` because only the tests consume it; the live menu builds ksni
/// items directly.
#[cfg(test)]
fn resident_menu_shapes(
    st: &RecordingState,
    armed: (bool, bool),
    delay_idx: usize,
) -> Vec<(String, bool)> {
    let arms =
        crate::recording_ui::audio_arm_source(st.recording, (st.mic, st.system), armed);
    let state = crate::recording_ui::AudioArmState::from_arms(arms.0, arms.1);
    visible_tray_menu(st.recording, st.paused)
        .into_iter()
        .map(|entry| match entry {
            TrayItem::Control(c) => (c.label.to_string(), true),
            TrayItem::Separator => (String::new(), true),
            TrayItem::Action { item, enabled } => match item.action {
                CaptureAction::Quit => ("Quit Cosmic Capture Kit Tray".to_string(), enabled),
                _ => (item.label.to_string(), enabled),
            },
            TrayItem::Launchers { menu, enabled, .. } => (menu.label().to_string(), enabled),
            TrayItem::Radio { menu: RadioSubmenu::Audio, enabled } => {
                (crate::recording_ui::audio_submenu_title(state), enabled)
            }
            TrayItem::Radio { menu: RadioSubmenu::Countdown, enabled } => {
                (crate::recording_ui::countdown_submenu_title(delay_idx), enabled)
            }
        })
        .collect()
}

/// Whether the resident's Quit item is usable for a state (hidden while recording,
/// paused or not, since the hide round). Pure and unit-tested. Since DRAGON-574 the
/// LIVE rule comes from the portable model (`tray_menu`'s Quit flag, rendered as
/// visibility by `visible_tray_menu`), so this is test-only: it pins that the model
/// keeps the rule every surface applied by hand before.
#[cfg(test)]
fn quit_enabled(st: &RecordingState) -> bool {
    !st.recording
}

// ── Recording-control IPC (shared protocol with the mac daemon) ──────────────

/// Bind the recording-control Unix socket and spawn the thread that serves it. Removes
/// any stale socket file first (a crashed predecessor's leftover), then binds and
/// detaches the accept loop. Best-effort: any failure logs and returns, leaving the
/// socket absent (recording children then keep their own control surface, the in-frame
/// toolbar / SNI item). Mirrors the mac daemon's `start_recording_ipc`.
fn start_recording_ipc(ipc: IpcShared, handle: ksni::blocking::Handle<ResidentTray>) {
    use std::os::unix::net::UnixListener;
    let path = crate::daemon_ipc::socket_path();
    // A stale socket file from a crashed predecessor would make bind fail with
    // EADDRINUSE; unlink it first (safe: we hold the resident single-instance lock, so
    // no live resident owns it).
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("resident: recording IPC bind failed ({e}); children keep their own control");
            return;
        }
    };
    log::info!("resident: recording IPC listening at {path}");
    std::thread::Builder::new()
        .name("cck-resident-recording-ipc".into())
        .spawn(move || serve_recording_ipc(listener, ipc, handle))
        .expect("spawn resident recording IPC thread");
}

/// Accept and service recording-control connections from capture children forever. Each
/// child connects at recording start and streams [`RecordingState`] lines; we update the
/// shared state and ask the tray to re-render. A menu command is written back on the
/// same stream (via the shared write half). On EOF / read error (the child finished or
/// crashed) we reset to idle so the icon/menu never stick. One child at a time (a
/// resident spawns one-shot captures; a new connection supersedes any stale write half).
/// Best-effort throughout — a socket hiccup never touches the tray. Mirrors the mac
/// daemon's `serve_recording_ipc`, differing only in the redraw call (ksni handle vs an
/// AppKit main-queue dispatch).
fn serve_recording_ipc(
    listener: std::os::unix::net::UnixListener,
    ipc: IpcShared,
    handle: ksni::blocking::Handle<ResidentTray>,
) {
    use std::io::{BufRead as _, BufReader};
    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(e) => {
                log::warn!("resident: recording IPC accept failed: {e}");
                continue;
            }
        };
        // Keep a write half so menu clicks can send commands to THIS child.
        match stream.try_clone() {
            Ok(w) => {
                if let Ok(mut g) = ipc.child_tx.lock() {
                    *g = Some(w);
                }
            }
            Err(e) => log::warn!("resident: recording IPC clone failed: {e}"),
        }
        log::info!("resident: recording child connected");
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if let Some(new_state) = RecordingState::parse(&line) {
                if let Ok(mut g) = ipc.state.lock() {
                    *g = new_state;
                }
                // A no-op update: `ksni` re-queries `icon_pixmap` + `menu` after any
                // `update`, which is exactly the redraw we need (the render reads the
                // shared snapshot, not any field we mutate here).
                handle.update(|_t| {});
            }
        }
        // The child disconnected (clean end or crash): revert to idle and drop the write
        // half so stale commands can't target a dead socket.
        if let Ok(mut g) = ipc.state.lock() {
            *g = RecordingState::default();
        }
        if let Ok(mut g) = ipc.child_tx.lock() {
            *g = None;
        }
        handle.update(|_t| {});
        log::info!("resident: recording child disconnected; tray reverted to idle");
    }
}

/// Run the resident tray loop and NEVER return to the GUI stack. Called from `main()`
/// on a bare `resident` launch. Steps, in order:
///
/// 1. Install the SIGUSR1 (capture NOW) + SIGTERM (quit) handlers (before recording our
///    pid — no boot race).
/// 2. Take the resident single-instance lock; if it's already held, a resident is up — ask it
///    for whatever THIS launch actually wanted ([`crate::instance::held_lock_action`]: a
///    capture for a capture-intent launch, nothing at all for a `resident` one) and exit.
/// 3. Spawn the `ksni` StatusNotifierItem; if no SNI host is present, log and exit (a
///    tray with nowhere to show is useless — the user can retry when a panel is up).
/// 4. Bind + serve the recording-control IPC socket.
/// 5. Poll the SIGUSR1 / SIGTERM flags forever (spawning detached children / exiting).
pub fn run(daemon_intent: bool) -> ! {
    // 1. Signal handlers first — before the lock records our pid, so a racing second
    //    launch that reads our pid and signals us can never hit an unhandled signal.
    install_sigusr1();
    install_sigterm();

    // 2. Single-instance: take the resident lock (capture children take none —
    //    DRAGON-351). If another resident holds it, this is a duplicate bare
    //    launch → ask the running resident to capture (its SIGUSR1 handler is up) and exit.
    //    Intent decides how hard to try (DRAGON-180): an explicit `resident` launch
    //    (autostart / manual) keeps the ~1.5s restart-handoff retry window; a bare
    //    CAPTURE-intent launch (the global hotkey) tries ONCE — a held lock there just
    //    means a live daemon, and the user is waiting for an overlay, so signal it
    //    immediately instead of sitting out the window.
    let acquired = if daemon_intent {
        crate::instance::acquire_daemon_lock()
    } else {
        crate::instance::try_acquire_daemon_lock()
    };
    if !acquired {
        // A resident is already up. WHAT this launch asks it for depends on what this launch
        // wanted, never on the lock being held (DRAGON-471's rule, extended to every
        // daemon-intent launch by DRAGON-473 and shared with macOS and Windows).
        //
        // The marker is PEEKED, never taken: on Linux this process does not deliver the
        // About window itself, so consuming the marker here would lose the release notes for
        // the process that will. The winner of the lock is the one that consumes it.
        match crate::instance::held_lock_action(
            daemon_intent,
            crate::update::post_update_marker_pending(),
        ) {
            // The user pressed the capture key (or re-ran the binary bare) while a resident
            // was up. That IS a capture request, and handing it over is the second-launch UX.
            crate::instance::HeldLockAction::SignalCapture => {
                if crate::instance::signal_existing_capture() {
                    log::info!("resident: another instance is up; asked it to capture");
                } else {
                    log::info!("resident: another instance holds the resident lock; exiting");
                }
            }
            // A post-update relaunch (DRAGON-532) that lost the race. Nobody asked for
            // anything: the installer started us to restore the shape the app was in before
            // the update, and being beaten to it means the winner is already doing that job.
            // Leave quietly rather than drop an unrequested overlay seconds after an update.
            //
            // Same defect DRAGON-465 fixed on Windows, reached differently: there the
            // relaunch's ARGV was read as capture intent (fixed by
            // `update::post_update_relaunch_args`, which this flow already uses); here the
            // argv is right and losing the lock was itself read as intent. Seen for real
            // while testing the AppImage self-update.
            crate::instance::HeldLockAction::RestoreAbout => {
                log::info!(
                    "resident: post-update relaunch found a live resident; exiting without \
                     capturing"
                );
            }
            // DRAGON-473: a plain `resident` launch (the autostart entry at login, the
            // settings tray toggle, a manual re-run) that raced a live resident. It asked for
            // a TRAY and one is already running, so it is simply redundant. It used to ask
            // that resident to CAPTURE, which put a region overlay on screen for a user who
            // had asked for a tray icon.
            //
            // Nothing else is owed here. Windows turns this answer into a liveness ping,
            // because a wedged tray daemon there is reachable-but-not-pumping and worth
            // taking over; Linux has no equivalent test to run from out here (its signal is a
            // `SIGUSR1` to a pid whose liveness it already checked), so exiting IS the whole
            // job. `info`, not `warn`: a working tray is up and nothing was lost.
            crate::instance::HeldLockAction::ProbeOnly => {
                log::info!(
                    "resident: a `resident` launch found the resident lock held by a live \
                     instance; nothing to do, exiting without asking it to capture"
                );
            }
        }
        std::process::exit(0);
    }

    // A CAPTURE-intent launch (the global hotkey) that won the lock found no resident
    // running: it becomes the resident below, but the user pressed a capture key and
    // is owed an overlay — spawn the default capture child NOW (DRAGON-181). Before
    // the tray/SNI setup on purpose: the no-SNI-host path exits without ever reaching
    // the trigger loop, and the capture must happen regardless. Daemon-intent
    // (`resident`) launches raise only the tray, as always.
    //
    // DRAGON-465 (Windows) was this exact rule firing for nobody: an installer relaunched
    // the app BARE, `main` read that as capture intent, and a finished update came back with
    // an overlay over the release notes.
    //
    // Linux DOES have an in-app install flow now (DRAGON-532, the AppImage self-update), so
    // the "Linux cannot hit it" this comment used to claim is no longer true, and
    // `owes_capture_on_start` (which it told a future reader to use) was never written.
    // What actually keeps this spawn safe is `update::post_update_relaunch_args`: a
    // post-update relaunch with `resident` on arrives WITH the resident argument, so
    // `daemon_intent` is true and this branch is skipped; with `resident` off it never
    // reaches this file at all, because `main` only routes a bare launch here when the
    // persisted `resident` setting is on, and `app::run`'s marker check turns it into a
    // settings window on About. The remaining hole was the LOST-LOCK path above, closed by
    // `instance::held_lock_action` (DRAGON-532 closed the post-update half of it first, in a
    // Linux-only predicate that DRAGON-473 then subsumed).
    if !daemon_intent {
        log::info!("resident: capture-intent launch became the resident; spawning the capture");
        spawn_child(REGION_FLAG);
    }

    // 2b. Repair a STALE autostart entry (DRAGON-628). We are the resident, which is the
    //     thing the autostart entry exists to launch, so this is the right moment to make
    //     that entry name a binary that still exists. Only ever rewrites an entry that is
    //     PRESENT and no longer resolves: an absent one may have been removed on purpose and
    //     is left alone. Runs after the single-instance lock so exactly one process per
    //     session does it, and before the tray so a session that ends at the no-SNI-host
    //     exit below has still had its entry fixed.
    //
    //     Reached identically by the macOS and Windows daemons; the portable body decides
    //     what each can do (`platform::autostart_repair_at_daemon_start`).
    crate::platform::autostart_repair_at_daemon_start();

    // 3. Raise the tray item. Tint the icon with the app's accent colour (read from the
    //    COSMIC theme files, dependency-free) so it matches the recording tray + toolbar.
    let ipc = IpcShared::default();
    let accent = effective_accent_rgb();
    // DRAGON-558: seed the persisted audio arms ONCE here (a config read, fine at startup,
    // not on the ksni thread). The trigger loop's config-mtime tick keeps them fresh.
    // DRAGON-574: the countdown preset (`delay_idx`) rides the same seed + tick.
    let persisted = crate::state::load();
    let arms = (persisted.record_mic, persisted.record_system_audio);
    let delay_idx = persisted.delay_idx;
    let tray = ResidentTray {
        ipc: ipc.clone(),
        accent,
        icon_cache: std::cell::RefCell::new(None),
        armed_mic: arms.0,
        armed_system: arms.1,
        delay_idx,
    };
    use ksni::blocking::TrayMethods;
    // `disable_dbus_name` inside a sandbox (`lab/flatpak`). ksni normally requests its own
    // well-known bus name, `org.kde.StatusNotifierItem-<pid>-<n>`, and Flatpak's D-Bus proxy
    // refuses to let a sandboxed app own it. There is no finish-arg that fixes this: the
    // wildcard `--own-name=org.kde.StatusNotifierItem-*` is not valid Flatpak syntax (only a
    // trailing `.*` matches), and Flathub's own rule says that exception is never granted.
    //
    // ksni documents this exact case and takes the connection's unique name instead, which the
    // watcher accepts just as happily. Without it the daemon reached its "no StatusNotifierItem
    // host available" arm and EXITED, so the tray never appeared and the resident looked like
    // it had failed to start at all.
    //
    // Gated on the sandbox rather than applied everywhere, because outside one the well-known
    // name is the conventional behaviour and there is no reason to change what already works.
    let handle = match tray.disable_dbus_name(crate::util::flatpak_sandboxed()).spawn() {
        Ok(h) => h,
        Err(e) => {
            log::warn!("resident: no StatusNotifierItem host available ({e}); exiting");
            std::process::exit(0);
        }
    };
    log::info!("resident: tray up (pid {})", std::process::id());

    // 4. Bind + serve the recording-control socket on a background thread. Best-effort:
    //    a bind failure just means daemon-attached recordings fall back to the child's own
    //    control surface, so recording is never uncontrollable.
    start_recording_ipc(ipc, handle.clone());

    // Startup update notice (DRAGON-177 follow-up): the DRAGON-177 "a new update is
    // available" dialog lives in the SETTINGS window, which the resident never opens on
    // its own — so a user who only ever starts the resident never learns an update is
    // out. Check ONCE at startup, on a background thread (never block the trigger loop /
    // tray), after a modest grace so a login-launched resident doesn't race the desktop
    // / network coming up. If an update is available AND the user wants notices, open the
    // settings window; it runs its own check and shows the dialog. Shared with the mac
    // daemon via `update` so the two can't drift. No polling loop — startup only.
    // Post-update relaunch parity with the mac daemon (the marker is only written
    // by the macOS one-click installer today; a Linux consume is a no-op until a
    // Linux install flow exists).
    if crate::update::take_post_update_marker() {
        crate::recording_ui::spawn_capture_child_with_env(
            "--settings",
            &[(crate::update::SETTINGS_TAB_ENV, "about")],
        );
    }
    std::thread::Builder::new()
        .name("cck-resident-update-notice".into())
        .spawn(|| {
            std::thread::sleep(crate::update::DAEMON_STARTUP_CHECK_DELAY);
            crate::update::notify_daemon_startup_if_update_available(|| {
                spawn_child("--settings");
            });
        })
        .expect("spawn resident update-notice thread");

    // ── Tombstone: the cloud auto-delete sweep (DRAGON-505) ─────────────────────────────
    //
    // A `cck-resident-cloud-ledger` thread ran here from DRAGON-482 until DRAGON-505,
    // calling `cloud::ledger::sweep_forever`: at start and every six hours it deleted the
    // uploads whose "delete after N hours" window had passed. The mac and Windows daemons
    // each carried the identical thread, which is the shape any replacement should keep.
    //
    // It is gone because the FEATURE is gone, not because the daemon was the wrong owner.
    // No provider offers server-side file TTL (Google Drive, Dropbox and OneDrive were each
    // re-checked on 2026-08-03), so this sweep was the whole implementation, and its
    // guarantee was too weak to ship: files were only ever removed while this daemon
    // happened to be running, so an uninstall or a machine left off meant "delete after 24
    // hours" silently never happened. Removed at the owner's direction; DRAGON-505 has the
    // history, and `cloud/mod.rs` carries the fuller tombstone beside the deleted module.
    //
    // If a scheduled delete is ever revisited, note what has NOT changed: a resident is
    // still the only process of ours that runs long enough to afford network I/O, and a
    // CAPTURE child must still never do this work (a capture owes the user an overlay in
    // the time they notice, and a sweep is requests to however many providers are named).

    // 5. Trigger loop: drain SIGUSR1 (→ default capture) and SIGTERM (→ clean shutdown).
    //    A short sleep keeps the loop cheap; the ksni item + IPC live on their own threads.
    let mut last_accent = accent;
    let mut last_arms = arms;
    let mut last_delay = delay_idx;
    let mut last_config_mtime = crate::state::config_mtime();
    let mut ticks: u32 = 0;
    loop {
        if TERM_PENDING.swap(false, Ordering::SeqCst) {
            log::info!("resident: SIGTERM/Quit → shutting down the tray and exiting");
            handle.shutdown();
            std::process::exit(0);
        }
        if USR1_PENDING.swap(false, Ordering::SeqCst) {
            log::info!("resident: SIGUSR1 → default capture");
            spawn_child(REGION_FLAG);
        }
        // DRAGON-179: follow the app's effective accent — when a settings process
        // SAVES the config (Theme override toggled / accent picked), re-tint the
        // tray without a daemon restart. Detected by the config file's mtime (one
        // stat about once a second on this already-running tick); the config
        // re-read + re-render run only on an actual save. An IPC poke was rejected:
        // the recording socket treats ANY connection as the recording child
        // (stashes the write half, resets to idle on disconnect), so a transient
        // notifier connection could clobber a live recording's controls.
        // DRAGON-558 rides the same tick for the persisted audio arms, so a settings
        // window (or a capture child's toolbar toggle) changing them re-checks the
        // tray's Enable items within ~1s.
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(25) {
            let mtime = crate::state::config_mtime();
            if mtime != last_config_mtime {
                last_config_mtime = mtime;
                let now = effective_accent_rgb();
                let p = crate::state::load();
                let now_arms = (p.record_mic, p.record_system_audio);
                let now_delay = p.delay_idx;
                if now != last_accent || now_arms != last_arms || now_delay != last_delay {
                    last_accent = now;
                    last_arms = now_arms;
                    last_delay = now_delay;
                    let _ = handle.update(|t: &mut ResidentTray| {
                        t.accent = now;
                        t.armed_mic = now_arms.0;
                        t.armed_system = now_arms.1;
                        t.delay_idx = now_delay;
                    });
                }
            }
        }
        // 40ms (DRAGON-180, was 120): the hotkey handshake's residual latency is one
        // tick of this loop between SIGUSR1 landing and the capture child spawning —
        // still a trivial idle wake rate, and the mtime probe above stays ~1Hz via
        // its tick divisor.
        std::thread::sleep(Duration::from_millis(40));
    }
}

// ── Accent colour ─────────────────────────────────────────────────────────────
//
// The resident tints its tray icon with the app's RESOLVED accent so it can never
// disagree with the capture children's chrome (or the Windows daemon, which tints from
// the same resolver). DRAGON-289 folds the Automatic Contrast Boost into that resolver,
// so a boosted accent must tint a boosted icon — the plain-std COSMIC-file read this used
// to do can't reproduce the boost's contrast derivation, so it now goes through the
// shared `resolved_appearance_accent_rgba` instead. That resolver is HEADLESS (pure theme
// math + a cosmic-config file read, no iced/wgpu), so the resident stays lightweight.

/// The tray tint (DRAGON-179 / DRAGON-289): the app's EFFECTIVE, RESOLVED accent —
/// override-vs-system pick AND the Automatic Contrast Boost, exactly as `App::tray_accent`
/// and the capture children draw it. Re-reads the persisted state fresh each call (the
/// resolver loads state internally), so a config saved by a separate settings process is
/// picked up on the next mtime-gated re-tint tick.
fn effective_accent_rgb() -> [u8; 3] {
    let [r, g, b, _] = crate::app::theme::resolved_appearance_accent_rgba();
    [r, g, b]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(recording: bool, paused: bool, mic: bool, system: bool) -> RecordingState {
        RecordingState { recording, paused, mic, system }
    }

    #[test]
    fn capture_flags_match_the_shared_launch_flags() {
        // The launchers + Settings map to the exact CLI argv via the ONE shared
        // `CaptureAction::spawn_args` (the record entries are their capture twins plus
        // `--video`, DRAGON-559); the SIGUSR1 default is Region.
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
        assert_eq!(REGION_FLAG, "--region");
    }

    #[test]
    fn idle_menu_is_the_owners_shape() {
        // No child recording (DRAGON-574, plus DRAGON-582's Color Picker at the
        // head): Color Picker, Scanner, the Capture / Record submenus, the
        // Countdown Timer and Audio Recording radio submenus, a separator, Settings...,
        // Quit — everything enabled. The radio titles carry the CACHED persisted state
        // (mic-only arms, preset index 1) so both read without opening.
        let shapes = resident_menu_shapes(&rec(false, false, false, false), (true, false), 1);
        assert_eq!(
            shapes,
            vec![
                ("Color Picker".to_string(), true),
                ("Scanner".to_string(), true),
                ("Capture".to_string(), true),
                ("Record".to_string(), true),
                ("Countdown Timer: 03".to_string(), true),
                ("Audio Recording: Mic Only".to_string(), true),
                (String::new(), true), // separator
                ("Settings...".to_string(), true),
                ("Quit Cosmic Capture Kit Tray".to_string(), true),
            ]
        );
    }

    #[test]
    fn recording_menu_leads_with_audio_then_the_controls() {
        // While a child records (DRAGON-574 + the recolour-round amendment; hide
        // round): the Audio Recording submenu leads the WHOLE menu, then the three
        // controls and a separator, then the same group with the flagged-off rows
        // HIDDEN — no Record, no Countdown Timer, no Quit (the COSMIC applet cannot
        // gray or subdue a dbusmenu row, so the uniform answer everywhere is
        // omission; see `visible_tray_menu`). The audio title carries the LIVE arms
        // (system-only here), not the persisted ones, and a radio pick applies live.
        let shapes = resident_menu_shapes(&rec(true, false, false, true), (true, false), 2);
        assert_eq!(
            shapes,
            vec![
                ("Audio Recording: System Only".to_string(), true),
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

    #[test]
    fn paused_flips_the_pause_label_to_resume() {
        let shapes = resident_menu_shapes(&rec(true, true, false, false), (false, false), 0);
        let labels: Vec<&str> = shapes.iter().map(|(l, _)| l.as_str()).collect();
        assert!(labels.contains(&"Resume Recording"));
        assert!(!labels.contains(&"Pause Recording"));
    }

    #[test]
    fn quit_is_hidden_only_while_recording() {
        assert!(quit_enabled(&rec(false, false, false, false)), "idle: Quit present");
        assert!(!quit_enabled(&rec(true, false, false, false)), "recording: Quit hidden");
        assert!(!quit_enabled(&rec(true, true, false, false)), "paused: Quit still hidden");
        // The rendered menu agrees with the pinned rule, state by state: the row
        // exists exactly when the rule says it is usable (hide round: a false flag is
        // rendered as omission, not as a grayed row).
        for st in [rec(false, false, false, false), rec(true, false, false, false)] {
            let shapes = resident_menu_shapes(&st, (false, false), 0);
            let has_quit =
                shapes.iter().any(|(l, _)| l == "Quit Cosmic Capture Kit Tray");
            assert_eq!(has_quit, quit_enabled(&st));
            if has_quit {
                let (label, flag) = shapes.last().expect("Quit closes the idle menu");
                assert_eq!(label, "Quit Cosmic Capture Kit Tray");
                assert!(*flag);
            }
        }
    }

    #[test]
    fn no_menu_label_contains_a_dash() {
        for st in [rec(false, false, false, false), rec(true, false, true, true), rec(true, true, true, true)] {
            for armed in [(false, false), (true, true)] {
                for delay_idx in [0usize, 3] {
                    for (l, _) in resident_menu_shapes(&st, armed, delay_idx) {
                        assert!(
                            !l.contains('\u{2014}') && !l.contains('\u{2013}'),
                            "dash in {l:?}"
                        );
                    }
                }
            }
        }
    }

}
