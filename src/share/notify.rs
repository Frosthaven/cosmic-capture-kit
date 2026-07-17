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
    display_notification("Processing capture", "Processing edited capture...");
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

/// Windows: no notification yet (no bare-binary path implemented).
#[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
pub fn run_notify(_path: &Path, _copied: bool) {
    log::debug!("desktop notification is not implemented on this platform");
}

/// macOS: post the capture notification. Inside a signed `.app` bundle this goes
/// through UNUserNotificationCenter — a real, drawer-persistent banner whose click
/// reveals the file in Finder (best-effort; see [`mac_un`]). A bare (unbundled)
/// `cargo run` binary CANNOT use UN (its APIs throw — `bundleProxyForCurrentProcess`
/// is nil), so it degrades to the click-less `osascript` banner. Never panics.
///
/// This runs in the short-lived re-exec helper (`--notify-saved <path>`), mirroring
/// the Linux path: the helper posts, then lingers briefly as its own UN delegate to
/// catch a click before exiting.
#[cfg(target_os = "macos")]
pub fn run_notify(path: &Path, copied: bool) {
    let summary = if copied { "Copied to clipboard" } else { "Saved" };
    let body = path.display().to_string();
    if crate::platform::mac::is_bundled() {
        mac_un::post(summary, &body, path);
    } else {
        display_notification(summary, &body);
    }
}

/// Post a one-shot macOS banner. Best-effort: a missing or failing `osascript`
/// (e.g. notification permission denied for the interpreter) is swallowed.
#[cfg(target_os = "macos")]
fn display_notification(summary: &str, body: &str) {
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_escape(body),
        applescript_escape(summary),
    );
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .status();
}

/// Escape a string for embedding inside an AppleScript double-quoted literal.
/// Backslash and double-quote must be escaped; raw control characters (newline,
/// carriage return, tab) can't appear literally in a `"..."` literal, so map them
/// to their AppleScript escapes too.
#[cfg(target_os = "macos")]
fn applescript_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::applescript_escape;

    #[test]
    fn escapes_quote_and_backslash() {
        assert_eq!(applescript_escape(r#"a"b"#), r#"a\"b"#);
        assert_eq!(applescript_escape(r"a\b"), r"a\\b");
        // A path a hostile filename could carry: quote + backslash together.
        assert_eq!(applescript_escape(r#"/tmp/e"il\x"#), r#"/tmp/e\"il\\x"#);
    }

    #[test]
    fn escapes_control_chars() {
        assert_eq!(applescript_escape("a\nb\tc\rd"), "a\\nb\\tc\\rd");
    }

    #[test]
    fn plain_text_unchanged() {
        let s = "/Users/me/Pictures/capture-2026.png";
        assert_eq!(applescript_escape(s), s);
    }
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

/// UNUserNotificationCenter path — the bundle-only rich notification (DRAGON-130).
///
/// ## Process model & the click→reveal decision (read this)
///
/// Notifications are posted by a SHORT-LIVED re-exec helper (`--notify-saved
/// <path>`), not the main app — the same detached-child model the Linux path uses.
/// That single fact drives the one-shot-vs-resident question the ticket raises:
///
/// - The helper sets ITSELF as the notification-center delegate BEFORE posting,
///   then pumps its run loop for a bounded window (like the Linux helper's 20s
///   backstop). A click within that window routes `didReceiveNotificationResponse`
///   → [`run_reveal`] → exit. This is symmetric with Linux and self-contained: the
///   process that posts also owns the click, so it works the same whether the main
///   app is resident or one-shot.
/// - CAVEAT (honest): whether macOS delivers the click to this *background*
///   (`LSUIElement`) helper versus activating/relaunching the main bundle app is
///   not guaranteed by the OS. If it activates the app instead, only the banner is
///   honored (no reveal). The banner POSTING is reliable; the reveal is best-effort
///   for the helper's lifetime. Wiring a second delegate into the resident main app
///   is a possible future belt-and-suspenders, but was left out here because it is
///   not verifiable in automation and would compete with the helper's delegate.
///
/// Authorization: requested once (alert only — no badge/sound). The request is
/// async; the notification is added from the auth completion handler so a first-run
/// grant still delivers. All UN calls are bundle-gated by the caller — never
/// reached from a bare binary (where they would throw).
#[cfg(target_os = "macos")]
mod mac_un {
    use crate::share::run_reveal;
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, ProtocolObject};
    use objc2::{define_class, msg_send, AllocAnyThread};
    use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    /// The file the click should reveal. One helper process posts exactly one
    /// notification, so a single global path is sufficient (no per-id map needed).
    static REVEAL_PATH: OnceLock<PathBuf> = OnceLock::new();

    define_class!(
        // SAFETY:
        // - Root class NSObject (no subclassing constraints), no ivars, no `Drop`.
        // - Only ever built/used on the helper's main thread; carries no
        //   main-thread-only state, so the plain NSObject super is sound.
        #[unsafe(super(NSObject))]
        #[name = "CckNotifyDelegate"]
        struct NotifyDelegate;

        // SAFETY: NSObjectProtocol has no safety requirements.
        unsafe impl NSObjectProtocol for NotifyDelegate {}

        // SAFETY: the one implemented selector matches the delegate protocol's
        // `didReceiveNotificationResponse` shape (center, response, completion
        // block); `willPresent` is optional and intentionally omitted.
        unsafe impl UNUserNotificationCenterDelegate for NotifyDelegate {
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive_response(
                &self,
                _center: &UNUserNotificationCenter,
                _response: &UNNotificationResponse,
                completion_handler: &block2::DynBlock<dyn Fn()>,
            ) {
                if let Some(p) = REVEAL_PATH.get() {
                    run_reveal(p);
                }
                completion_handler.call(());
                // The click is handled — nothing left for this helper to do.
                std::process::exit(0);
            }
        }
    );

    impl NotifyDelegate {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            // SAFETY: NSObject's `init` has the standard signature.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// Post the notification and linger briefly to catch a click. Blocking (runs a
    /// bounded run loop) — always called on the helper's main thread.
    pub fn post(summary: &str, body: &str, path: &Path) {
        let _ = REVEAL_PATH.set(path.to_path_buf());

        let center = UNUserNotificationCenter::currentNotificationCenter();

        // Set the delegate BEFORE posting so a click can't slip in unrouted. It is a
        // WEAK property, so the retained object must outlive the run loop below —
        // `_delegate` is held on this stack frame for exactly that long.
        let delegate = NotifyDelegate::new();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

        // Build the alert content + request up front.
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(summary));
        content.setBody(&NSString::from_str(body));
        let ident = NSString::from_str(&format!("cck-{}", std::process::id()));
        let request =
            UNNotificationRequest::requestWithIdentifier_content_trigger(&ident, &content, None);

        // Request authorization (alert only), adding the request from the completion
        // handler so a first-run grant still delivers. The handler fires immediately
        // with `granted = true` on subsequent (already-authorized) runs.
        let center_for_add = center.clone();
        let handler = RcBlock::new(move |granted: Bool, _err: *mut NSError| {
            if granted.as_bool() {
                center_for_add.addNotificationRequest_withCompletionHandler(&request, None);
            } else {
                log::warn!(
                    "notification authorization denied; enable it under System Settings > \
                     Notifications for Cosmic Capture Kit"
                );
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert,
            &handler,
        );

        // Pump the run loop so the async grant + delivery complete and a click can
        // route before this short-lived helper exits (the delegate `exit(0)`s on a
        // click; otherwise we fall out after the window). Mirrors the Linux helper's
        // 20s backstop.
        pump_run_loop(Duration::from_secs(20));
        // Keep the (weak-referenced) delegate alive across the whole pump.
        drop(delegate);
    }

    /// Run the current thread's run loop in short slices until `budget` elapses, so
    /// notification-center callbacks (and the async auth/add completions) can fire.
    fn pump_run_loop(budget: Duration) {
        use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};
        let run_loop = NSRunLoop::currentRunLoop();
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            let slice = NSDate::dateWithTimeIntervalSinceNow(0.1);
            // SAFETY: `NSDefaultRunLoopMode` is a framework-owned static string;
            // `runMode:beforeDate:` runs one pass in that mode until the slice date.
            let progressed =
                unsafe { run_loop.runMode_beforeDate(NSDefaultRunLoopMode, &slice) };
            // With no input sources the call returns instantly; nap to avoid a busy
            // spin (the async completions live on background dispatch queues, so this
            // still lets them run and deliver).
            if !progressed {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}
