//! System-tray recording controls: ONE StatusNotifierItem (via `ksni`) with a click
//! menu, the Linux counterpart of the macOS menu-bar item (`platform/mac/tray.rs`).
//!
//! A per-session status icon (DRAGON-174): raised at capture start (idle brackets + the
//! capture menu), kept for the WHOLE session, and flipped to the record dot + in-recording
//! menu once a recording begins. It ALWAYS carries the recording controls; the "Hide
//! toolbar on full screen captures" setting decides only the in-frame toolbar's visibility
//! (hidden when it can't fit outside a full-screen capture), never this icon's content.
//!
//! ## One item + menu (DRAGON-159, aligned to the mac daemon in DRAGON-172)
//!
//! The item shows ONE viewfinder icon whose centre glyph reads the state at a glance
//! (corner brackets always; + a solid centre dot while recording; + centre pause bars
//! while paused) and its menu is the UNIFORM in-recording menu every surface renders
//! (DRAGON-173): Toggle Microphone (a checkmark reflects armed vs muted), Toggle System
//! Audio (checkmark), a separator, Pause / Resume Recording (label reflects state), Finish &
//! Save Recording, Cancel & Delete Recording, a separator, then a "Capture Menu" submenu of
//! the capture launchers + Settings + Quit (Quit disabled). This mirrors the macOS resident
//! daemon's menu (`crate::daemon`) — labels, order, and the three-state icon all come from
//! the ONE portable source (`crate::recording_ui`) so the surfaces can never drift. The
//! recording actions still send the same [`TrayEvent`] the app dispatches; the Capture Menu
//! launchers spawn a fresh one-shot process directly, like the daemon.
//!
//! The icon is rendered as a raw ARGB pixmap (NOT a themed icon name — several symbolic
//! names don't resolve in every panel, which showed as blank/white squares). We
//! rasterize the shared bracket SVG, tinted with the app's accent colour. `ksni`'s
//! blocking API owns its own thread + D-Bus connection. Failure-safe: if no host is
//! present, [`TraySession::start`] returns `None` and the caller keeps the in-frame
//! toolbar.

use crate::daemon_ipc::{Command, RecordingState};
use crate::recording_ui::{
    capture_menu, icon_svg, icon_state, recording_menu, CaptureAction, IconState, MenuItemKind,
    RecordingAction,
};
use std::cell::RefCell;
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender};

/// A control the user activated from the tray. Mirrors the macOS enum so the app's
/// event handling is platform-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    Stop,
    TogglePause,
    ToggleMic,
    ToggleSystemAudio,
    /// Cancel + discard the recording. Emitted by this tray's Cancel recording menu item
    /// (DRAGON-172; the app dispatches it to `RecordingMsg::CancelRecording`, mirroring
    /// the macOS daemon menu). Now constructed on Linux (the menu builds it), so the
    /// DRAGON-170 `allow(dead_code)` for this variant is no longer needed here.
    Cancel,
    /// Quit the whole capture session/app (DRAGON-174). Emitted by the IDLE session icon's
    /// Quit item — a child-owned icon has no resident to quit, so the least-weird uniform
    /// behavior is to end this capture session (the app dispatches it to `finish_session`).
    Quit,
}

/// The state the item renders: whether a recording is in progress (idle vs recording icon
/// and menu), whether it's paused (record vs pause glyph), and the two audio checkmarks. An
/// idle session icon (DRAGON-174) sits at `recording: false` until a recording begins.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TrayState {
    recording: bool,
    paused: bool,
    mic: bool,
    system_audio: bool,
}

/// The single recording tray item: its live state, the app's accent colour (the icon
/// tint, matching the toolbar buttons), the click channel, and a cache of the
/// last-rendered icon pixmap.
struct RecordingTray {
    state: TrayState,
    accent: [u8; 3],
    tx: Sender<TrayEvent>,
    /// The last-rendered icon, keyed by [`IconState`], so a host re-querying
    /// `icon_pixmap()` (some poll it on every panel redraw) doesn't re-run the usvg parse
    /// and resvg render when nothing changed. `ksni::Tray` methods are `&self`, so the
    /// cache needs interior mutability; `icon_pixmap` is the only reader/writer.
    icon_cache: std::cell::RefCell<Option<(IconState, Vec<ksni::Icon>)>>,
}

impl RecordingTray {
    /// The three-state icon for the current state (DRAGON-172/174): shared with the macOS
    /// daemon via [`crate::recording_ui`]. The session icon exists for the WHOLE capture
    /// session, so all three states are reachable: [`IconState::Idle`] (corner brackets)
    /// while not recording (during selection), [`IconState::Recording`] (centre dot) while
    /// recording, [`IconState::Paused`] (pause bars) while paused. The corner brackets are
    /// constant so the icon never shifts; only the centre glyph changes.
    fn icon_state(&self) -> IconState {
        icon_state(self.state.recording, self.state.paused)
    }
}

impl ksni::Tray for RecordingTray {
    /// A left-click opens the menu (all controls live there now) instead of firing a
    /// single action, so every control is one click away. Right-click also shows it.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "dev.frosthaven.CosmicCaptureKit.Recording".to_string()
    }

    fn title(&self) -> String {
        "Cosmic Capture Kit recording".to_string()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let key = self.icon_state();
        if let Some((cached_key, icons)) = &*self.icon_cache.borrow()
            && *cached_key == key
        {
            return icons.clone();
        }
        let icons = render_icon(icon_svg(key), self.accent).map(|i| vec![i]).unwrap_or_default();
        *self.icon_cache.borrow_mut() = Some((key, icons.clone()));
        icons
    }

    /// The control menu (DRAGON-174): while NOT recording (the idle session icon), the
    /// capture group rendered FLAT (launchers, separator, Settings, Quit ENABLED) — the
    /// same idle menu the resident shows; while recording, the UNIFORM in-recording menu
    /// every surface renders (DRAGON-173): the six-entry in-recording group (Toggle
    /// Microphone, Toggle System Audio, separator, Pause/Resume Recording, Finish & Save,
    /// Cancel & Delete), a separator, then a "Capture Menu" submenu of the launchers +
    /// Settings + Quit (Quit disabled while recording). Both are built from the ONE portable
    /// models so labels/order match the daemon / resident exactly. A left-click opens the
    /// menu, so every control is one click away.
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
        if !self.state.recording {
            // Idle session icon: the flat capture group (launchers, separator, Settings,
            // Quit ENABLED — a child-owned idle icon quits the capture session/app).
            return capture_group_items(self.state.recording);
        }
        // Map a shared `RecordingAction` to the `TrayEvent` the app dispatches. Each menu
        // closure sends its own event through the channel (the closures are `&mut Self`,
        // so capture the action by value).
        fn send(t: &RecordingTray, action: RecordingAction) {
            let ev = match action {
                RecordingAction::TogglePause => TrayEvent::TogglePause,
                RecordingAction::ToggleMic => TrayEvent::ToggleMic,
                RecordingAction::ToggleSystemAudio => TrayEvent::ToggleSystemAudio,
                RecordingAction::Stop => TrayEvent::Stop,
                RecordingAction::Cancel => TrayEvent::Cancel,
            };
            let _ = t.tx.send(ev);
        }
        let mut items: Vec<ksni::MenuItem<Self>> =
            recording_menu(self.state.paused, self.state.mic, self.state.system_audio)
                .into_iter()
                .map(|item| match item.kind {
                    MenuItemKind::Separator => ksni::MenuItem::Separator,
                    MenuItemKind::Standard => {
                        let action = item.action;
                        StandardItem {
                            label: item.label.to_string(),
                            activate: Box::new(move |t: &mut Self| send(t, action)),
                            ..Default::default()
                        }
                        .into()
                    }
                    MenuItemKind::Checkmark(checked) => {
                        let action = item.action;
                        CheckmarkItem {
                            label: item.label.to_string(),
                            checked,
                            activate: Box::new(move |t: &mut Self| send(t, action)),
                            ..Default::default()
                        }
                        .into()
                    }
                })
                .collect();
        items.push(ksni::MenuItem::Separator);
        items.push(
            SubMenu {
                label: crate::recording_ui::CAPTURE_MENU_LABEL.to_string(),
                submenu: capture_group_items(self.state.recording),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// The capture group (launchers, separator, Settings, Quit) for a child-owned session icon,
/// from the ONE portable [`capture_menu`] model — rendered FLAT as the idle menu and, while
/// recording, nested inside the "Capture Menu" submenu. The launchers spawn a fresh one-shot
/// process directly (like the daemon), NOT through the recording event channel. Quit differs
/// by `recording` (DRAGON-174): while recording it is DISABLED (the child-owned surface can't
/// quit a resident that never spawned it, and Quit is disabled everywhere while recording);
/// while IDLE it is ENABLED and emits [`TrayEvent::Quit`] to end this capture session/app.
/// Quit uses the full "Quit Cosmic Capture Kit" label.
fn capture_group_items(recording: bool) -> Vec<ksni::MenuItem<RecordingTray>> {
    use ksni::menu::StandardItem;
    let mut items: Vec<ksni::MenuItem<RecordingTray>> = Vec::new();
    for item in capture_menu() {
        match item.kind {
            MenuItemKind::Separator => items.push(ksni::MenuItem::Separator),
            MenuItemKind::Checkmark(_) | MenuItemKind::Standard => match item.action {
                CaptureAction::Quit if recording => items.push(
                    StandardItem {
                        label: "Quit Cosmic Capture Kit".to_string(),
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                ),
                CaptureAction::Quit => items.push(
                    // Idle: Quit ends this capture session/app (there is no resident to quit).
                    StandardItem {
                        label: "Quit Cosmic Capture Kit".to_string(),
                        activate: Box::new(move |t: &mut RecordingTray| {
                            let _ = t.tx.send(TrayEvent::Quit);
                        }),
                        ..Default::default()
                    }
                    .into(),
                ),
                action => {
                    let flag = action.spawn_flag().unwrap_or("--region");
                    items.push(
                        StandardItem {
                            label: item.label.to_string(),
                            activate: Box::new(move |_t: &mut RecordingTray| {
                                crate::recording_ui::spawn_capture_child(flag);
                            }),
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

/// Rasterize a `#000`-filled bracket glyph SVG into a square ARGB32 tray icon tinted
/// `color` — full-bleed so the glyph fills the panel's icon slot. Rendered ourselves (not
/// a themed icon name) so it looks identical on every host. The shared SVGs use opaque
/// black (`#000`) for template rendering on macOS; here we recolour it to the accent.
///
/// `pub(crate)` so the resident tray (`crate::daemon_linux`, DRAGON-173) renders its
/// three-state icon through the SAME rasterizer, keeping the two Linux surfaces pixel-
/// identical.
pub(crate) fn render_icon(svg_black: &str, color: [u8; 3]) -> Option<ksni::Icon> {
    let hex = format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
    // Recolour both the stroked brackets and the filled centre glyph to the accent.
    let svg_tmpl = svg_black.replace("#000", &hex);
    render_icon_hex(&svg_tmpl)
}

/// Rasterize an already-coloured SVG into the ARGB32 tray icon pixmap.
fn render_icon_hex(svg: &str) -> Option<ksni::Icon> {
    // 64px source: comfortably above typical panel icon sizes (22-48) so the host
    // downscales (crisp) rather than upscales (blurry).
    const S: u32 = 64;
    let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(S, S)?;
    let scale = S as f32 / 24.0;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia is premultiplied; emit straight-alpha ARGB32 in network (big-endian)
    // byte order: [A, R, G, B] per pixel, which SNI hosts expect.
    let mut data = Vec::with_capacity((S * S * 4) as usize);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        data.extend_from_slice(&[c.alpha(), c.red(), c.green(), c.blue()]);
    }
    Some(ksni::Icon { width: S as i32, height: S as i32, data })
}

/// A live recording control surface. Two backings share the seam the app calls
/// (`start` / `start_daemon` / `poll` / `set_audio` / `set_paused` / `Drop`),
/// byte-identical to the macOS `TraySession` (DRAGON-173):
///
/// * [`Backing::Daemon`] — when the resident tray RESIDENT (`crate::daemon_linux`)
///   spawned this recording, the in-recording actions live in the RESIDENT's ONE tray
///   item. This session raises NO item of its own: it relays the recording state to the
///   resident over the IPC socket and drains the resident's menu commands (as
///   [`TrayEvent`]s) from a reader thread. Preferred whenever a resident is reachable, so
///   a resident-launched recording shows a SINGLE tray item, not two.
/// * [`Backing::Own`] — the historical Linux control surface: this process's OWN `ksni`
///   StatusNotifierItem + menu. Used for recordings with NO resident (terminal / CLI
///   launches, or the resident off), so non-resident recordings never lose their controls.
pub struct TraySession {
    backing: Backing,
}

enum Backing {
    /// Relay to the resident's tray item over the IPC socket.
    Daemon(DaemonBacking),
    /// This process's own `ksni` item (no resident attached).
    Own(OwnBacking),
}

/// The own-item control: the single `ksni` item handle (dropped → the item vanishes)
/// and the shared channel of clicks the app drains on a timer.
struct OwnBacking {
    handle: ksni::blocking::Handle<RecordingTray>,
    events: Receiver<TrayEvent>,
}

/// The resident-backed control: the socket write half (to push state) + the last full
/// state (so a partial `set_audio` / `set_paused` resends the whole snapshot) behind a
/// `RefCell` (the seam is `&self`; this session is drained only on the app's UI thread, so
/// the borrow is never contended), and the command channel a reader thread fills from the
/// resident's menu clicks. Mirrors the macOS `DaemonBacking` exactly.
struct DaemonBacking {
    conn: RefCell<UnixStream>,
    state: RefCell<RecordingState>,
    commands: Receiver<TrayEvent>,
}

impl TraySession {
    /// Raise this process's OWN tray item in the RECORDING state: a recording is
    /// starting with no resident present. The own icon exists ONLY while a recording
    /// is live (DRAGON-182 — no idle session icon) and drops with it. Returns `None`
    /// when no StatusNotifierItem host is available (the caller keeps the in-frame
    /// toolbar).
    ///
    /// Prefer [`TraySession::start_daemon`] first: when a resident is present, ALL
    /// in-recording controls belong in the resident's ONE tray item.
    pub fn start_recording(mic: bool, system_audio: bool, accent: [u8; 3]) -> Option<Self> {
        let own = OwnBacking::start(true, mic, system_audio, accent)?;
        Some(TraySession { backing: Backing::Own(own) })
    }

    /// Try to route the in-recording controls to the resident tray RESIDENT's item
    /// (DRAGON-173): connect the IPC socket, push the initial state, and drain the
    /// resident's menu commands. Returns `None` when no resident is listening (the caller
    /// then decides between its own item and the in-frame toolbar). Preferred whenever a
    /// resident is present, so a resident-launched recording shows a SINGLE tray item.
    /// Portable seam parity with the macOS `TraySession::start_daemon`.
    pub fn start_daemon(mic: bool, system_audio: bool) -> Option<Self> {
        let d = DaemonBacking::connect(mic, system_audio)?;
        Some(TraySession { backing: Backing::Daemon(d) })
    }

    /// Drain and return every click since the last poll.
    pub fn poll(&self) -> Vec<TrayEvent> {
        match &self.backing {
            Backing::Daemon(d) => d.commands.try_iter().collect(),
            Backing::Own(o) => o.events.try_iter().collect(),
        }
    }

    /// Reflect the current mic / system-audio state (resident: resend the snapshot; own:
    /// update the menu checkmarks via the ksni handle).
    pub fn set_audio(&self, mic: bool, system_audio: bool) {
        match &self.backing {
            Backing::Daemon(d) => {
                {
                    let mut st = d.state.borrow_mut();
                    st.mic = mic;
                    st.system = system_audio;
                }
                d.push();
            }
            Backing::Own(o) => {
                o.handle.update(|t| {
                    t.state.mic = mic;
                    t.state.system_audio = system_audio;
                });
            }
        }
    }

    /// Reflect pause state (resident: resend the snapshot so the icon + Pause/Resume label
    /// update there; own: flip the item's icon + Pause/Resume label via the ksni handle).
    pub fn set_paused(&self, paused: bool) {
        match &self.backing {
            Backing::Daemon(d) => {
                d.state.borrow_mut().paused = paused;
                d.push();
            }
            Backing::Own(o) => {
                o.handle.update(|t| {
                    t.state.paused = paused;
                });
            }
        }
    }
}

impl OwnBacking {
    /// Raise this process's own `ksni` session item in the given state (idle when
    /// `recording` is false, recording otherwise). Returns `None` when no
    /// StatusNotifierItem host is available.
    fn start(recording: bool, mic: bool, system_audio: bool, accent: [u8; 3]) -> Option<Self> {
        use ksni::blocking::TrayMethods;
        let (tx, events) = std::sync::mpsc::channel();
        let tray = RecordingTray {
            state: TrayState { recording, paused: false, mic, system_audio },
            accent,
            tx,
            icon_cache: std::cell::RefCell::new(None),
        };
        match tray.spawn() {
            Ok(handle) => Some(OwnBacking { handle, events }),
            Err(e) => {
                log::warn!("recording tray unavailable ({e}); keeping the in-frame toolbar");
                None
            }
        }
    }
}

impl DaemonBacking {
    /// Try to attach to a running resident: connect the IPC socket, push the initial
    /// recording state, and start a reader thread that turns the resident's command lines
    /// into [`TrayEvent`]s. Returns `None` when no resident is listening (the socket is
    /// absent or refuses), so the caller falls back to its own item. Mirrors the macOS
    /// `DaemonBacking::connect`.
    fn connect(mic: bool, system_audio: bool) -> Option<Self> {
        let path = crate::daemon_ipc::socket_path();
        let conn = UnixStream::connect(&path).ok()?;
        let reader = conn.try_clone().ok()?;
        let (tx, commands) = std::sync::mpsc::channel();
        // Reader thread: block on resident command lines, map each to its TrayEvent, and
        // push into the channel the app drains on its tray poll. Exits on EOF / a closed
        // receiver (session dropped) — the write half closing signals the resident too.
        std::thread::Builder::new()
            .name("cck-resident-ipc-reader".into())
            .spawn(move || {
                use std::io::{BufRead as _, BufReader};
                let r = BufReader::new(reader);
                for line in r.lines() {
                    let Ok(line) = line else { break };
                    if let Some(cmd) = Command::parse(&line) {
                        let ev = match cmd {
                            Command::TogglePause => TrayEvent::TogglePause,
                            Command::ToggleMic => TrayEvent::ToggleMic,
                            Command::ToggleSystemAudio => TrayEvent::ToggleSystemAudio,
                            Command::Stop => TrayEvent::Stop,
                            Command::Cancel => TrayEvent::Cancel,
                        };
                        if tx.send(ev).is_err() {
                            break;
                        }
                    }
                }
            })
            .ok()?;
        let d = DaemonBacking {
            conn: RefCell::new(conn),
            state: RefCell::new(RecordingState {
                recording: true,
                paused: false,
                mic,
                system: system_audio,
            }),
            commands,
        };
        d.push();
        Some(d)
    }

    /// Push the current full state to the resident (best-effort; a broken pipe is ignored,
    /// the resident reverts on EOF).
    fn push(&self) {
        let line = self.state.borrow().encode();
        let mut conn = self.conn.borrow_mut();
        let _ = conn.write_all(line.as_bytes());
        let _ = conn.flush();
    }
}

impl Drop for DaemonBacking {
    /// Tell the resident the recording ended (state recording=false), then close the
    /// connection so the resident reverts even if the final line is lost.
    fn drop(&mut self) {
        self.state.borrow_mut().recording = false;
        self.push();
        // Dropping `conn` (and the reader's clone once the reader thread exits) closes the
        // socket; the resident's read hits EOF and reverts to idle.
    }
}

impl Drop for OwnBacking {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `id()` / `title()` are `ksni::Tray` trait methods; the trait must be in scope to
    // call them (the `impl` alone does not import it). DRAGON-159 added the test on mac,
    // where `tray.rs` is never compiled, so this only surfaced on the Linux build.
    use ksni::Tray as _;

    fn tray(state: TrayState) -> RecordingTray {
        let (tx, _rx) = std::sync::mpsc::channel();
        RecordingTray {
            state,
            accent: [0x33, 0x99, 0xff],
            tx,
            icon_cache: std::cell::RefCell::new(None),
        }
    }

    #[test]
    fn icon_state_covers_idle_recording_and_paused() {
        // The session icon (DRAGON-174) reaches all three states: idle brackets during
        // selection, the record dot while recording, the pause bars while paused — the
        // shared three-state icon from `recording_ui`.
        let idle = tray(TrayState { recording: false, paused: false, mic: false, system_audio: false });
        let recording = tray(TrayState { recording: true, paused: false, mic: true, system_audio: true });
        let paused = tray(TrayState { recording: true, paused: true, mic: true, system_audio: true });
        assert_eq!(idle.icon_state(), IconState::Idle);
        assert_eq!(recording.icon_state(), IconState::Recording);
        assert_eq!(paused.icon_state(), IconState::Paused);
        // The SVGs differ between all three states.
        assert_ne!(icon_svg(idle.icon_state()), icon_svg(recording.icon_state()));
        assert_ne!(icon_svg(recording.icon_state()), icon_svg(paused.icon_state()));
    }

    /// Flatten a ksni menu to top-level labels (a submenu becomes its own label; "" for a
    /// separator; the "?" arm never fires for our items).
    fn top_labels(items: &[ksni::MenuItem<RecordingTray>]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                ksni::MenuItem::Standard(s) => s.label.clone(),
                ksni::MenuItem::Checkmark(c) => c.label.clone(),
                ksni::MenuItem::SubMenu(s) => s.label.clone(),
                ksni::MenuItem::Separator => String::new(),
                _ => "?".to_string(),
            })
            .collect()
    }

    #[test]
    fn idle_menu_is_the_flat_capture_group_with_quit_enabled() {
        // DRAGON-174: while NOT recording, the session icon shows the flat capture group
        // (the same idle menu the resident shows) with Quit ENABLED (a child-owned idle
        // icon quits the capture session/app). No in-recording controls, no submenu.
        let t = tray(TrayState { recording: false, paused: false, mic: false, system_audio: false });
        let menu = t.menu();
        let labels = top_labels(&menu);
        assert_eq!(
            labels,
            vec![
                "Scanner".to_string(),
                "Capture Region".to_string(),
                "Capture Window".to_string(),
                "Capture Monitor".to_string(),
                String::new(), // separator
                "Settings".to_string(),
                "Quit Cosmic Capture Kit".to_string(),
            ]
        );
        // The idle Quit is a live StandardItem (enabled); clicking it emits TrayEvent::Quit.
        let quit = match menu.into_iter().last().unwrap() {
            ksni::MenuItem::Standard(s) => s,
            _ => panic!("last idle item is Quit"),
        };
        assert!(quit.enabled, "idle Quit is enabled");
    }

    #[test]
    fn menu_is_the_uniform_group_then_a_capture_submenu() {
        // The child-owned recording tray renders the SAME uniform menu every surface does
        // (DRAGON-173): the six-entry control group, a separator, then a "Capture Menu"
        // submenu — all from the ONE portable models, so it can never drift.
        let t = tray(TrayState { recording: true, paused: false, mic: true, system_audio: false });
        let menu = t.menu();
        let labels = top_labels(&menu);
        assert_eq!(
            labels,
            vec![
                "Toggle Microphone".to_string(),
                "Toggle System Audio".to_string(),
                String::new(), // separator
                "Pause Recording".to_string(),
                "Finish & Save Recording".to_string(),
                "Cancel & Delete Recording".to_string(),
                String::new(), // separator before the Capture Menu submenu
                "Capture Menu".to_string(),
            ]
        );
        // The last item is the "Capture Menu" submenu; its contents are the capture group.
        let sub_labels = match menu.into_iter().last().unwrap() {
            ksni::MenuItem::SubMenu(s) => top_labels(&s.submenu),
            _ => panic!("last item is the Capture Menu submenu"),
        };
        assert_eq!(
            sub_labels,
            vec![
                "Scanner".to_string(),
                "Capture Region".to_string(),
                "Capture Window".to_string(),
                "Capture Monitor".to_string(),
                String::new(), // separator
                "Settings".to_string(),
                "Quit Cosmic Capture Kit".to_string(),
            ]
        );
        // Paused flips the Pause item (index 3) to Resume.
        let paused = tray(TrayState { recording: true, paused: true, mic: true, system_audio: false });
        assert_eq!(top_labels(&paused.menu())[3], "Resume Recording");
        // No em/en-dash anywhere (top level + submenu).
        for l in labels.into_iter().chain(sub_labels) {
            assert!(!l.contains('\u{2014}') && !l.contains('\u{2013}'), "dash in {l:?}");
        }
    }

    #[test]
    fn id_is_stable_and_carries_the_dev_prefix() {
        let t = tray(TrayState { recording: true, paused: false, mic: false, system_audio: false });
        assert_eq!(t.id(), "dev.frosthaven.CosmicCaptureKit.Recording");
        assert_eq!(t.title(), "Cosmic Capture Kit recording");
    }
}
