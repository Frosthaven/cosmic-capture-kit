//! Desktop notification helper.

use std::path::Path;

#[cfg(target_os = "linux")]
use super::open::run_reveal;
use super::reexec::{NOTIFY_COPIED, NOTIFY_SAVED, spawn_self};

/// Post the capture notification — "Copied to clipboard" when `copied`, else
/// "Saved" — whose click reveals the file. Detached.
pub fn notify(path: &Path, copied: bool) {
    spawn_self(if copied { NOTIFY_COPIED } else { NOTIFY_SAVED }, path);
}

/// macOS/Windows: no sticky/replaceable notification (that's
/// UNUserNotificationCenter, which needs a signed .app bundle — DRAGON-130
/// packaging). The processing notification is best-effort even on Linux, so here
/// we just run the work; on macOS we additionally fire a one-shot `osascript`
/// banner at the start (it can't be updated or closed, only posted).
#[cfg(not(target_os = "linux"))]
pub fn with_processing_notification<T>(work: impl FnOnce() -> T) -> T {
    #[cfg(target_os = "macos")]
    crate::platform::mac::notify::display_notification(
        "Processing capture",
        "Processing edited capture...",
    );
    work()
}

/// Run `work` with a persistent "Processing capture…" notification up for its
/// duration — posted before, closed after (best effort; the work runs regardless
/// of D-Bus availability). Blocking: call from a worker thread, never the UI.
#[cfg(target_os = "linux")]
pub fn with_processing_notification<T>(work: impl FnOnce() -> T) -> T {
    let posted = zbus::blocking::Connection::session().ok().and_then(|conn| {
        let hints: std::collections::HashMap<&str, zbus::zvariant::Value> =
            std::collections::HashMap::new();
        let id = conn
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &(
                    "Cosmic Capture Kit",
                    0u32,
                    notification_icon(),
                    "Processing capture",
                    "Processing edited capture...",
                    Vec::<&str>::new(),
                    hints,
                    0i32, // sticks until we close it
                ),
            )
            .ok()?
            .body()
            .deserialize::<u32>()
            .ok()?;
        Some((conn, id))
    });
    let out = work();
    if let Some((conn, id)) = posted {
        let _ = conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "CloseNotification",
            &(id,),
        );
    }
    out
}

/// The notification icon: the installed app icon when present (packaging puts
/// it in hicolor), else a stock camera glyph so dev runs aren't iconless.
#[cfg(target_os = "linux")]
fn notification_icon() -> &'static str {
    const NAME: &str = "dev.frosthaven.CosmicCaptureKit";
    let installed = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))
        .into_iter()
        .map(|d| d.join("icons"))
        .chain(["/usr/share/icons".into(), "/usr/local/share/icons".into()])
        .any(|base: std::path::PathBuf| {
            base.join(format!("hicolor/scalable/apps/{NAME}.svg")).is_file()
        });
    if installed { NAME } else { "camera-photo-symbolic" }
}

/// Windows (DRAGON-229): dispatch to the WinRT toast body under `platform/windows/`
/// (closed split). Runs in the short-lived `--notify-*` re-exec helper, mirroring the
/// Linux/macOS process model.
#[cfg(target_os = "windows")]
pub fn run_notify(path: &Path, copied: bool) {
    crate::platform::windows::services::run_notify(path, copied);
}

/// Any other (non-Linux/macOS/Windows) target: no notification path.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos"), not(target_os = "windows")))]
pub fn run_notify(_path: &Path, _copied: bool) {
    log::debug!("desktop notification is not implemented on this platform");
}

/// macOS (DRAGON-230): dispatch to the notification body under `platform/mac/`
/// (closed split). A bundled `.app` posts a UNUserNotificationCenter banner whose
/// click reveals the file; an unbundled dev binary degrades to a click-less
/// `osascript` banner. Runs in the short-lived `--notify-*` re-exec helper.
#[cfg(target_os = "macos")]
pub fn run_notify(path: &Path, copied: bool) {
    crate::platform::mac::notify::run_notify(path, copied);
}

/// Post a desktop notification (no `transient` hint, so it stays in the drawer)
/// and stay alive only long enough to catch a click on it — then reveal the
/// file. Exits as soon as the notification is closed (popup dismissed/expired)
/// or after a short backstop, so we don't linger like a daemon.
#[cfg(target_os = "linux")]
pub fn run_notify(path: &Path, copied: bool) {
    let summary = if copied { "Copied to clipboard" } else { "Saved" };
    let body = path.display().to_string();

    let Ok(conn) = zbus::blocking::Connection::session() else {
        return;
    };
    // Subscribe before notifying so a fast click can't slip through the gap.
    let Ok(proxy) = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    ) else {
        return;
    };
    let Ok(signals) = proxy.receive_all_signals() else {
        return;
    };

    // "default" fires on a body click (and we give it the visible label "Open").
    let hints: std::collections::HashMap<&str, zbus::zvariant::Value> =
        std::collections::HashMap::new();
    let Ok(reply) = conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "Cosmic Capture Kit",
            0u32,
            notification_icon(),
            summary,
            body.as_str(),
            vec!["default", "Open"],
            hints,
            5000i32, // popup shows ~5s, then tidies itself into the drawer
        ),
    ) else {
        return;
    };
    let Ok(id) = reply.body().deserialize::<u32>() else {
        return;
    };

    // Backstop so we never linger if the user neither clicks nor dismisses.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(20));
        std::process::exit(0);
    });

    for msg in signals {
        let header = msg.header();
        let member = header.member().map(|m| m.as_str().to_string());
        match member.as_deref() {
            Some("ActionInvoked") => {
                if let Ok((sig_id, _key)) = msg.body().deserialize::<(u32, String)>()
                    && sig_id == id
                {
                    run_reveal(path);
                    return;
                }
            }
            Some("NotificationClosed") => {
                // reason 1 = popup merely expired (it lives on in the drawer, so
                // keep listening for a click there); 2/3 = user dismissed/closed it
                // — nothing left to click, so stop.
                if let Ok((sig_id, reason)) = msg.body().deserialize::<(u32, u32)>()
                    && sig_id == id
                    && reason != 1
                {
                    return;
                }
            }
            _ => {}
        }
    }
}
