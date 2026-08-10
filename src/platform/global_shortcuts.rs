//! Recording hotkeys via the xdg-desktop-portal **GlobalShortcuts** interface —
//! press/release delivery regardless of keyboard focus, which is exactly what
//! hold-to-talk needs (DRAGON-109). While the portal binding is live, the
//! recording overlays don't need their layer-shell keyboard grab, so the
//! recorded desktop keeps normal focus behavior.
//!
//! Lifecycle: [`start`] spawns one listener thread for the process (the app is
//! one-shot); it binds the shortcuts and pushes timestamped [`HotkeyEvent`]s into
//! a shared queue that the recording poll drains. `bound` flips once the portal
//! accepted the bind; on ANY failure (interface missing on this desktop, user
//! denied, session died) `dead` flips and the app simply keeps its historical
//! keyboard-grab behavior — the portal is an upgrade, never a requirement.

#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicBool;
#[cfg(target_os = "linux")]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A recording hotkey delivered by the portal. `Instant`-stamped at signal
/// arrival so the audio-toggle timeline stays sample-accurate however late the
/// UI drains the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Variants are matched cross-platform in `App::drain_portal_hotkeys` but only
// CONSTRUCTED on Linux (the portal `run` loop); gating them would fragment that
// shared match, so allow the never-constructed lint off Linux.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum HotkeyEvent {
    /// The push-to-talk key went down (hold began).
    PttPressed,
    /// The push-to-talk key was released (hold ended).
    PttReleased,
    /// The stop-recording shortcut fired.
    Stop,
}

/// Shared state between the portal listener thread and the UI.
pub struct Hotkeys {
    /// Timestamped events, oldest first; the recording poll drains them.
    pub events: Arc<Mutex<Vec<(Instant, HotkeyEvent)>>>,
    /// The portal accepted the bind — recording overlays may drop their
    /// keyboard grab (the hotkeys arrive focus-free from here on).
    /// Linux-only: the fields carry xdg-portal GlobalShortcuts binding state,
    /// read solely by the Linux `start` thread; off Linux there is no portal.
    #[cfg(target_os = "linux")]
    pub bound: Arc<AtomicBool>,
    /// The portal path is gone (unavailable / denied / errored) — keep the
    /// keyboard-grab fallback. Never set while `bound` still delivers.
    #[cfg(target_os = "linux")]
    pub dead: Arc<AtomicBool>,
}

/// macOS/Windows: no xdg-portal GlobalShortcuts. Recording hotkeys become the
/// `global-hotkey` crate when the resident menu-bar app lands (DRAGON-94 phase 4);
/// until then report `dead` immediately so the app keeps its keyboard-grab path.
#[cfg(not(target_os = "linux"))]
pub fn start(_ptt_trigger: Option<String>, _stop_trigger: Option<String>) -> Hotkeys {
    Hotkeys {
        events: Arc::new(Mutex::new(Vec::new())),
    }
}

/// Whether this desktop exposes the portal `GlobalShortcuts` interface AT ALL: the one
/// capability question behind "can a recording shortcut be delivered without focus"
/// (DRAGON-583).
///
/// A REAL probe, not a guess and not a sandbox test. It reads the interface's `version`
/// property off the portal; a desktop that does not implement it answers with an error,
/// which is exactly what a `busctl introspect` of COSMIC's portal shows today (an empty
/// introspection: the interface is simply not there). Whether the interface is missing
/// because the desktop never shipped it or because a sandbox hid it, the answer we need is
/// the same, so this never asks about Flatpak.
///
/// Memoized for the process: at most one D-Bus round trip per launch, and the settings
/// page re-renders many times. Bounded, per DRAGON-118's rule: zbus's blocking calls wait
/// for a reply indefinitely, so the call runs on its own thread against
/// [`PROBE_BUDGET`] and a portal that never answers reads as "not available" instead of
/// stalling the UI. The thread is DETACHED on timeout rather than joined, exactly as
/// `platform/linux/secrets.rs` does, because it is parked inside zbus and waiting for it
/// would be the hang this exists to prevent.
#[cfg(target_os = "linux")]
pub fn interface_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        if std::thread::Builder::new()
            .name("cck-global-shortcuts-probe".into())
            .spawn(move || {
                let _ = tx.send(probe_interface());
            })
            .is_err()
        {
            return false;
        }
        let found = rx.recv_timeout(PROBE_BUDGET).unwrap_or(false);
        log::debug!("portal GlobalShortcuts interface available: {found}");
        found
    })
}

/// How long the interface probe may take before it is treated as "not available". The
/// portal is a local service on the session bus, so a real answer is sub-millisecond;
/// this is a bound, not an expectation.
#[cfg(target_os = "linux")]
const PROBE_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// The blocking half of [`interface_available`]: one `Properties.Get` for the interface's
/// `version`. Any error at all (no session bus, no portal, no such interface, an
/// unreadable reply) means we cannot promise a focus-free shortcut, which is the answer
/// the caller wants.
#[cfg(target_os = "linux")]
fn probe_interface() -> bool {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        return false;
    };
    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("org.freedesktop.portal.GlobalShortcuts", "version"),
    )
    .is_ok()
}

/// Bind the recording shortcuts through the portal and stream their
/// activations. Returns immediately; `bound`/`dead` report the outcome.
/// `ptt_trigger` / `stop_trigger` are optional XDG-spec trigger hints (e.g.
/// `"CTRL+F9"`) — the desktop may honor, remap, or ignore them.
#[cfg(target_os = "linux")]
pub fn start(ptt_trigger: Option<String>, stop_trigger: Option<String>) -> Hotkeys {
    let hk = Hotkeys {
        events: Arc::new(Mutex::new(Vec::new())),
        bound: Arc::new(AtomicBool::new(false)),
        dead: Arc::new(AtomicBool::new(false)),
    };
    let events = hk.events.clone();
    let bound = hk.bound.clone();
    let dead = hk.dead.clone();
    std::thread::spawn(move || {
        // ashpd/zbus (tokio flavour) needs an ambient runtime; this thread owns a
        // small single-threaded one for the session's whole life.
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                log::warn!("global shortcuts: tokio runtime failed ({e}); keyboard fallback");
                dead.store(true, Ordering::Relaxed);
                return;
            }
        };
        if let Err(e) = rt.block_on(run(ptt_trigger, stop_trigger, &events, &bound)) {
            // Missing interface (not every desktop ships it), user denial, or a
            // session drop — all downgrade to the keyboard-grab behavior.
            log::info!("global shortcuts unavailable ({e}); keyboard fallback");
        }
        dead.store(true, Ordering::Relaxed);
        bound.store(false, Ordering::Relaxed);
    });
    hk
}

/// Session body: bind `ptt` + `stop`, then pump Activated/Deactivated into the
/// queue until the session/connection ends.
#[cfg(target_os = "linux")]
async fn run(
    ptt_trigger: Option<String>,
    stop_trigger: Option<String>,
    events: &Mutex<Vec<(Instant, HotkeyEvent)>>,
    bound: &AtomicBool,
) -> Result<(), ashpd::Error> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use cosmic::iced::futures::{stream, StreamExt};

    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session().await?;
    let shortcuts = [
        NewShortcut::new("ptt", "Push to talk (hold while recording)")
            .preferred_trigger(ptt_trigger.as_deref()),
        NewShortcut::new("stop", "Stop the recording")
            .preferred_trigger(stop_trigger.as_deref()),
    ];
    // The desktop MAY show a one-time confirmation dialog here; it remembers the
    // grant per app id, so subsequent recordings bind silently.
    let response = portal
        .bind_shortcuts(&session, &shortcuts, None)
        .await?
        .response()?;
    for sc in response.shortcuts() {
        log::info!(
            "global shortcut bound: {} → {}",
            sc.id(),
            sc.trigger_description()
        );
    }
    let activated = portal.receive_activated().await?;
    let deactivated = portal.receive_deactivated().await?;
    bound.store(true, Ordering::Relaxed);

    // Merge both signal streams into one press/release feed (no select! macro —
    // the tokio "macros" feature isn't enabled, and a merged stream reads better).
    let presses = activated.filter_map(|a| async move {
        match a.shortcut_id() {
            "ptt" => Some(HotkeyEvent::PttPressed),
            "stop" => Some(HotkeyEvent::Stop),
            _ => None,
        }
    });
    let releases = deactivated.filter_map(|d| async move {
        (d.shortcut_id() == "ptt").then_some(HotkeyEvent::PttReleased)
    });
    let mut feed = std::pin::pin!(stream::select(presses, releases));
    // Stamp at arrival: the audio-toggle timeline uses these instants, so a lazy
    // UI drain can't smear the mute boundaries.
    while let Some(ev) = feed.next().await {
        if let Ok(mut g) = events.lock() {
            g.push((Instant::now(), ev));
        }
    }
    // Both signal streams ended → the session/connection is gone.
    Ok(())
}
