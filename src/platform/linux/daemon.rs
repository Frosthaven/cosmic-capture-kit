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
//!   paused, all from [`crate::recording_ui`]) and a menu built from the SAME portable
//!   sources as the mac daemon + the Linux recording tray, so the surfaces can never
//!   drift. Idle: the capture group flat (Scanner / Capture Region / Capture Window /
//!   Capture Monitor, separator, Settings, Quit). While a child records (DRAGON-173): the
//!   uniform in-recording menu — the six-entry control group (Toggle Microphone, Toggle
//!   System Audio, separator, Pause/Resume Recording, Finish & Save Recording, Cancel &
//!   Delete Recording), a separator, then the SAME capture group nested under a "Capture
//!   Menu" submenu (Quit disabled while recording).
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
//! * The resident takes the shared resident single-instance lock at startup (separate
//!   from the capture lock, so capture children can still take that) and installs the
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

use crate::daemon_ipc::{icon_svg, icon_state, Command, IconState, RecordingState};
use crate::recording_ui::{recording_menu, CaptureAction, MenuItemKind, RecordingAction};
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The CLI flag the SIGUSR1 "capture NOW" default spawns (Region). The menu launchers spawn
/// via [`crate::recording_ui::CaptureAction::spawn_flag`] (the ONE shared mapping); this
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
        "dev.frosthaven.CosmicCaptureKit.Resident".to_string()
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
        let icons = crate::tray::render_icon(icon_svg(key.0), self.accent)
            .map(|i| vec![i])
            .unwrap_or_default();
        *self.icon_cache.borrow_mut() = Some((key, icons.clone()));
        icons
    }

    /// The resident menu. Built from the ONE portable model so labels + order match the mac
    /// daemon exactly (see [`resident_menu_labels`] for the pinned shape). Idle: the capture
    /// group rendered FLAT (launchers, separator, Settings, Quit). While a child records
    /// (DRAGON-173): the uniform in-recording menu — the six-entry control group from
    /// [`recording_menu`], a separator, then the SAME capture group nested under a "Capture
    /// Menu" submenu (Quit disabled while recording).
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let st = self.ipc.snapshot();

        if !crate::daemon_ipc::shows_recording_controls(&st) {
            // Idle: the capture group, flat.
            return capture_group_items(&st);
        }

        // In-recording: the six-entry control group, a separator, then the "Capture Menu"
        // submenu of the whole capture group.
        use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
        // Relay each shared action to the connected child over the IPC socket.
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
        let mut items: Vec<ksni::MenuItem<Self>> = Vec::new();
        for item in recording_menu(st.paused, st.mic, st.system) {
            match item.kind {
                MenuItemKind::Separator => items.push(ksni::MenuItem::Separator),
                MenuItemKind::Standard => {
                    let action = item.action;
                    items.push(
                        StandardItem {
                            label: item.label.to_string(),
                            activate: Box::new(move |t: &mut Self| relay(t, action)),
                            ..Default::default()
                        }
                        .into(),
                    );
                }
                MenuItemKind::Checkmark(checked) => {
                    let action = item.action;
                    items.push(
                        CheckmarkItem {
                            label: item.label.to_string(),
                            checked,
                            activate: Box::new(move |t: &mut Self| relay(t, action)),
                            ..Default::default()
                        }
                        .into(),
                    );
                }
            }
        }
        items.push(ksni::MenuItem::Separator);
        items.push(
            SubMenu {
                label: crate::recording_ui::CAPTURE_MENU_LABEL.to_string(),
                submenu: capture_group_items(&st),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// The capture group as ksni menu items (launchers, separator, Settings, Quit), from the ONE
/// portable [`crate::recording_ui::capture_menu`] model. Used BOTH as the flat idle menu and
/// as the "Capture Menu" submenu contents while recording, so the two can never drift. Quit
/// is disabled while recording (it would orphan the child's control surface) and uses the
/// resident's full "Quit Cosmic Capture Kit Tray" label; the launchers carry the model labels.
fn capture_group_items(st: &RecordingState) -> Vec<ksni::MenuItem<ResidentTray>> {
    use ksni::menu::StandardItem;
    let mut items: Vec<ksni::MenuItem<ResidentTray>> = Vec::new();
    for item in crate::recording_ui::capture_menu() {
        match item.kind {
            MenuItemKind::Separator => items.push(ksni::MenuItem::Separator),
            MenuItemKind::Checkmark(_) | MenuItemKind::Standard => match item.action {
                CaptureAction::Quit => items.push(
                    StandardItem {
                        label: "Quit Cosmic Capture Kit Tray".to_string(),
                        enabled: quit_enabled(st),
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
                    // A launcher / Settings: spawn a one-shot child via the ONE portable path.
                    let flag = action.spawn_flag().unwrap_or("--region");
                    items.push(
                        StandardItem {
                            label: item.label.to_string(),
                            activate: Box::new(move |_t: &mut ResidentTray| spawn_child(flag)),
                            ..Default::default()
                        }
                        .into(),
                    );
                }
            },
        }
    }
    items
}

/// The capture-group labels the resident renders (the flat idle menu AND the "Capture Menu"
/// submenu contents), in order: Scanner, Capture Region, Capture Window, Capture Monitor, a
/// separator, Settings, Quit. Mirrors what [`capture_group_items`] builds (the ksni widgets
/// come from the identical `capture_menu` model), factored out so the composition is
/// unit-testable without a D-Bus / ksni host. `#[cfg(test)]` because only the tests consume
/// it — the live menu builds ksni items directly.
#[cfg(test)]
fn capture_group_labels() -> Vec<String> {
    crate::recording_ui::capture_menu()
        .into_iter()
        .map(|item| match item.action {
            CaptureAction::Quit => "Quit Cosmic Capture Kit Tray".to_string(),
            _ => item.label.to_string(),
        })
        .collect()
}

/// The TOP-LEVEL resident menu labels the resident renders, in order, for a given state.
/// Idle: the capture group, flat (so this equals [`capture_group_labels`]). While a child
/// records (DRAGON-173): the six-entry in-recording group from [`recording_menu`], a
/// separator, then a single "Capture Menu" submenu entry (its contents are
/// [`capture_group_labels`]). Mirrors what [`ResidentTray::menu`] builds so the whole
/// composition is unit-testable without a D-Bus / ksni host.
#[cfg(test)]
fn resident_menu_labels(st: &RecordingState) -> Vec<String> {
    if !crate::daemon_ipc::shows_recording_controls(st) {
        return capture_group_labels();
    }
    let mut labels = Vec::new();
    for item in recording_menu(st.paused, st.mic, st.system) {
        labels.push(item.label.to_string());
    }
    labels.push(String::new()); // separator
    labels.push(crate::recording_ui::CAPTURE_MENU_LABEL.to_string());
    labels
}

/// Whether the resident's Quit item is enabled for a state (disabled while recording,
/// paused or not) — the exact rule the mac daemon applies. Pure, so it is unit-tested.
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
/// 2. Take the resident single-instance lock; if it's already held, a resident is up —
///    signal IT to capture (the second-launch UX) and exit.
/// 3. Spawn the `ksni` StatusNotifierItem; if no SNI host is present, log and exit (a
///    tray with nowhere to show is useless — the user can retry when a panel is up).
/// 4. Bind + serve the recording-control IPC socket.
/// 5. Poll the SIGUSR1 / SIGTERM flags forever (spawning detached children / exiting).
pub fn run(daemon_intent: bool) -> ! {
    // 1. Signal handlers first — before the lock records our pid, so a racing second
    //    launch that reads our pid and signals us can never hit an unhandled signal.
    install_sigusr1();
    install_sigterm();

    // 2. Single-instance: take the resident lock (not the capture lock — that stays free
    //    for capture children). If another resident holds it, this is a duplicate bare
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
        if crate::instance::signal_existing_capture() {
            log::info!("resident: another instance is up; asked it to capture");
        } else {
            log::info!("resident: another instance holds the resident lock; exiting");
        }
        std::process::exit(0);
    }

    // A CAPTURE-intent launch (the global hotkey) that won the lock found no resident
    // running: it becomes the resident below, but the user pressed a capture key and
    // is owed an overlay — spawn the default capture child NOW (DRAGON-181). Before
    // the tray/SNI setup on purpose: the no-SNI-host path exits without ever reaching
    // the trigger loop, and the capture must happen regardless. Daemon-intent
    // (`resident`) launches raise only the tray, as always.
    if !daemon_intent {
        log::info!("resident: capture-intent launch became the resident; spawning the capture");
        spawn_child(REGION_FLAG);
    }

    // 3. Raise the tray item. Tint the icon with the app's accent colour (read from the
    //    COSMIC theme files, dependency-free) so it matches the recording tray + toolbar.
    let ipc = IpcShared::default();
    let accent = effective_accent_rgb();
    let tray = ResidentTray {
        ipc: ipc.clone(),
        accent,
        icon_cache: std::cell::RefCell::new(None),
    };
    use ksni::blocking::TrayMethods;
    let handle = match tray.spawn() {
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

    // 5. Trigger loop: drain SIGUSR1 (→ default capture) and SIGTERM (→ clean shutdown).
    //    A short sleep keeps the loop cheap; the ksni item + IPC live on their own threads.
    let mut last_accent = accent;
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
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(25) {
            let mtime = crate::state::config_mtime();
            if mtime != last_config_mtime {
                last_config_mtime = mtime;
                let now = effective_accent_rgb();
                if now != last_accent {
                    last_accent = now;
                    let _ = handle.update(|t: &mut ResidentTray| t.accent = now);
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
        // The launchers + Settings map to the exact CLI flags via the ONE shared
        // `CaptureAction::spawn_flag`; the SIGUSR1 default is Region.
        assert_eq!(CaptureAction::Scan.spawn_flag(), Some("--scan"));
        assert_eq!(CaptureAction::Region.spawn_flag(), Some("--region"));
        assert_eq!(CaptureAction::Window.spawn_flag(), Some("--window"));
        assert_eq!(CaptureAction::Monitor.spawn_flag(), Some("--monitor"));
        assert_eq!(CaptureAction::Settings.spawn_flag(), Some("--settings"));
        assert_eq!(REGION_FLAG, "--region");
    }

    #[test]
    fn idle_menu_is_the_flat_capture_group() {
        // No child recording: the capture group rendered FLAT — four launchers, a separator,
        // Settings, Quit. NO in-recording group, NO submenu. Unchanged from before.
        let labels = resident_menu_labels(&rec(false, false, false, false));
        assert_eq!(
            labels,
            vec![
                "Scanner".to_string(),
                "Capture Region".to_string(),
                "Capture Window".to_string(),
                "Capture Monitor".to_string(),
                String::new(), // separator
                "Settings".to_string(),
                "Quit Cosmic Capture Kit Tray".to_string(),
            ]
        );
    }

    #[test]
    fn recording_menu_is_the_six_group_then_a_capture_submenu() {
        // While a child records: the six-entry in-recording group (new DRAGON-173 order), a
        // separator, then a single "Capture Menu" submenu entry — the uniform menu every
        // surface renders, drawn from the SAME models so it can never drift.
        let labels = resident_menu_labels(&rec(true, false, true, false));
        assert_eq!(
            labels,
            vec![
                "Toggle Microphone".to_string(),
                "Toggle System Audio".to_string(),
                String::new(), // the recording_menu's own separator
                "Pause Recording".to_string(),
                "Finish & Save Recording".to_string(),
                "Cancel & Delete Recording".to_string(),
                String::new(), // separator before the Capture Menu submenu
                "Capture Menu".to_string(),
            ]
        );
        // The submenu (and the flat idle menu) contents are the capture group.
        assert_eq!(
            capture_group_labels(),
            vec![
                "Scanner".to_string(),
                "Capture Region".to_string(),
                "Capture Window".to_string(),
                "Capture Monitor".to_string(),
                String::new(), // separator
                "Settings".to_string(),
                "Quit Cosmic Capture Kit Tray".to_string(),
            ]
        );
    }

    #[test]
    fn paused_flips_the_pause_label_to_resume() {
        let labels = resident_menu_labels(&rec(true, true, false, false));
        assert!(labels.contains(&"Resume Recording".to_string()));
        assert!(!labels.contains(&"Pause Recording".to_string()));
    }

    #[test]
    fn quit_is_disabled_only_while_recording() {
        assert!(quit_enabled(&rec(false, false, false, false)), "idle: Quit enabled");
        assert!(!quit_enabled(&rec(true, false, false, false)), "recording: Quit disabled");
        assert!(!quit_enabled(&rec(true, true, false, false)), "paused: Quit still disabled");
    }

    #[test]
    fn no_menu_label_contains_a_dash() {
        for st in [rec(false, false, false, false), rec(true, false, true, true), rec(true, true, true, true)] {
            for l in resident_menu_labels(&st) {
                assert!(
                    !l.contains('\u{2014}') && !l.contains('\u{2013}'),
                    "dash in {l:?}"
                );
            }
        }
        for l in capture_group_labels() {
            assert!(!l.contains('\u{2014}') && !l.contains('\u{2013}'), "dash in {l:?}");
        }
    }

}
