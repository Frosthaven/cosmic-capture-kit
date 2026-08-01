//! Desktop notification helper.
//!
//! The banner is written HERE, once, for every platform ([`notification_text`]) — the
//! per-OS bodies below only present the two lines it produces. A "(no editor)" capture
//! (DRAGON-428) delivers with no UI at all, so this text is the whole feedback the user
//! gets, and it has to be both specific and honest: it names WHAT was captured and never
//! claims a clipboard copy that did not happen (DRAGON-450).
//!
//! WHO POSTS ONE (DRAGON-451): only a delivery with no editor on screen — in practice
//! `App::finish_share`. The preview editor reports its own saves and copies in its in-app
//! toasts, where the user is already looking; it used to ALSO post a system banner, which
//! meant two notifications for one action the user had just asked for. Keep this channel
//! for the case where there is no window to say anything in.

use std::path::Path;

#[cfg(target_os = "linux")]
use super::open::run_reveal;
use super::reexec::{NOTIFY_COPIED, NOTIFY_KIND, NOTIFY_REASON, NOTIFY_SAVED, spawn_self};

/// What a capture was OF, named on the banner's first line (DRAGON-450).
///
/// The three "(no editor)" hotkeys all deliver the same way and used to produce the same
/// wordless "Copied to clipboard", leaving the user to guess which one had fired. The
/// words are the overlay's own mode vocabulary, so the banner and the picker agree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotifyKind {
    Region,
    Window,
    Monitor,
}

impl NotifyKind {
    /// The word the banner uses.
    pub fn label(self) -> &'static str {
        match self {
            NotifyKind::Region => "Region",
            NotifyKind::Window => "Window",
            NotifyKind::Monitor => "Monitor",
        }
    }

    /// The argv token that carries this across the `--notify-*` re-exec. A fixed
    /// vocabulary word, so it is safe to log and can never carry user content.
    fn token(self) -> &'static str {
        match self {
            NotifyKind::Region => "region",
            NotifyKind::Window => "window",
            NotifyKind::Monitor => "monitor",
        }
    }

    /// Inverse of [`Self::token`]. An unknown token yields `None`, which degrades to the
    /// unnamed wording rather than to a wrong name.
    fn from_token(token: &str) -> Option<Self> {
        match token {
            "region" => Some(NotifyKind::Region),
            "window" => Some(NotifyKind::Window),
            "monitor" => Some(NotifyKind::Monitor),
            _ => None,
        }
    }
}

/// What became of the automatic clipboard copy — the one thing the banner must never get
/// wrong (DRAGON-450).
///
/// It replaces a `copied: bool` that was a PREDICTION: the old delivery path compared the
/// file size against the limit, copied, threw the copy's own result away, and then told
/// the user "Copied to clipboard" whether or not the write had actually happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CopyOutcome {
    /// The capture is on the clipboard.
    Copied,
    /// Not attempted: a still image over [`super::AUTO_COPY_MAX_BYTES`].
    TooLarge,
    /// Attempted, and the clipboard write did not happen.
    Failed,
    /// Not copied, with no reason recorded. Nothing in the app produces this — every
    /// delivery names its outcome — but it is what a `--notify-saved` carrying no reason
    /// token decodes to, so a hand-run or a future caller degrades to the plain "Saved"
    /// wording instead of to a claim of a copy.
    NotCopied,
}

impl CopyOutcome {
    /// The `--notify-reason` token, for the two outcomes that need to explain themselves.
    /// [`Self::Copied`] rides the `--notify-copied` flag and [`Self::NotCopied`] is the
    /// bare `--notify-saved`, so neither carries a reason.
    fn reason_token(self) -> Option<&'static str> {
        match self {
            CopyOutcome::TooLarge => Some("too-large"),
            CopyOutcome::Failed => Some("copy-failed"),
            CopyOutcome::Copied | CopyOutcome::NotCopied => None,
        }
    }

    /// Rebuild the outcome from the helper's argv: which `--notify-*` flag launched it,
    /// plus the optional `--notify-reason` token. An unrecognised reason falls back to the
    /// plain "saved" wording — never to a claim of a copy.
    fn from_wire(copied: bool, reason: Option<&str>) -> Self {
        if copied {
            return CopyOutcome::Copied;
        }
        match reason {
            Some("too-large") => CopyOutcome::TooLarge,
            Some("copy-failed") => CopyOutcome::Failed,
            _ => CopyOutcome::NotCopied,
        }
    }
}

/// The banner's two lines: the title, then the body.
///
/// The honesty rule lives here and nowhere else: the title says "copied to clipboard"
/// for [`CopyOutcome::Copied`] and for nothing else. Every other outcome says "saved",
/// and the body leads with the reason so a user who pressed a copy hotkey learns why
/// their paste is stale instead of wondering.
pub fn notification_text(
    kind: Option<NotifyKind>,
    outcome: CopyOutcome,
    path: &Path,
) -> (String, String) {
    let title = match (kind, outcome) {
        (Some(k), CopyOutcome::Copied) => format!("{} copied to clipboard", k.label()),
        (None, CopyOutcome::Copied) => "Copied to clipboard".to_string(),
        (Some(k), _) => format!("{} saved", k.label()),
        (None, _) => "Saved".to_string(),
    };
    let where_it_went = path.display();
    let body = match outcome {
        CopyOutcome::TooLarge => format!("Too large to copy to the clipboard. {where_it_went}"),
        CopyOutcome::Failed => format!("The clipboard copy failed. {where_it_went}"),
        CopyOutcome::Copied | CopyOutcome::NotCopied => where_it_went.to_string(),
    };
    (title, body)
}

/// Post the capture notification, whose click reveals the file. Detached: a short-lived
/// re-exec of ourselves owns the banner (and the click), so the capture process can exit.
///
/// The `kind` is required on the way OUT and optional on the way back IN
/// ([`notify_from_argv`]) on purpose: a delivery always knows what it captured, while a
/// helper reading an argv it did not write might not.
pub fn notify(path: &Path, kind: NotifyKind, outcome: CopyOutcome) {
    let flag = if matches!(outcome, CopyOutcome::Copied) { NOTIFY_COPIED } else { NOTIFY_SAVED };
    let _ = spawn_self(flag, path, &notify_argv_tail(Some(kind), outcome));
}

/// The extra argv the notification helper is launched with, beyond its flag and the path.
/// Split out from [`notify`] so the encode/decode pair can be round-tripped in a test —
/// the whole banner rides this handful of fixed tokens.
fn notify_argv_tail(kind: Option<NotifyKind>, outcome: CopyOutcome) -> Vec<&'static str> {
    let mut tail = Vec::new();
    if let Some(k) = kind {
        tail.push(NOTIFY_KIND);
        tail.push(k.token());
    }
    if let Some(reason) = outcome.reason_token() {
        tail.push(NOTIFY_REASON);
        tail.push(reason);
    }
    tail
}

/// Decode what the banner should say out of the helper's own argv. `copied` is which
/// `--notify-*` flag `main` matched; the kind and reason are read from their flags here.
pub fn notify_from_argv<S: AsRef<str>>(
    args: &[S],
    copied: bool,
) -> (Option<NotifyKind>, CopyOutcome) {
    let value = |flag: &str| {
        args.iter()
            .position(|a| a.as_ref() == flag)
            .and_then(|i| args.get(i + 1))
            .map(|s| s.as_ref())
    };
    (
        value(NOTIFY_KIND).and_then(NotifyKind::from_token),
        CopyOutcome::from_wire(copied, value(NOTIFY_REASON)),
    )
}

// DRAGON-353: `with_processing_notification` lived here — it wrapped the preview editor's
// bake/export in a sticky desktop "Processing capture" notification, because the editor
// TORE ITS SURFACE DOWN for the duration and there was nowhere else to show progress. The
// editor now stays up and draws its own spinner over the picture
// (`PREVIEW_PROCESSING_MESSAGES`), so the notification had no callers left and is gone
// rather than kept as an unused helper. Its macOS one-shot `osascript` banner rode the
// same function; the mac `display_notification` body it called survives for `run_notify`.

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

/// The notification helper's whole job: build the banner, hand it to the OS. Runs in the
/// short-lived `--notify-*` re-exec child, which also owns the click.
pub fn run_notify(path: &Path, kind: Option<NotifyKind>, outcome: CopyOutcome) {
    let (title, body) = notification_text(kind, outcome, path);
    post(&title, &body, path);
}

/// Windows (DRAGON-229): dispatch to the WinRT toast body under `platform/windows/`
/// (closed split). `path` rides along for the toast's protocol-activation launch URI,
/// which is what routes a click back into the reveal service (DRAGON-450).
#[cfg(target_os = "windows")]
fn post(title: &str, body: &str, path: &Path) {
    crate::platform::windows::services::run_notify(title, body, path);
}

/// Any other (non-Linux/macOS/Windows) target: no notification path.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos"), not(target_os = "windows")))]
fn post(_title: &str, _body: &str, _path: &Path) {
    log::debug!("desktop notification is not implemented on this platform");
}

/// macOS (DRAGON-230): dispatch to the notification body under `platform/mac/`
/// (closed split). A bundled `.app` posts a UNUserNotificationCenter banner whose
/// click reveals the file; an unbundled dev binary degrades to a click-less
/// `osascript` banner.
#[cfg(target_os = "macos")]
fn post(title: &str, body: &str, path: &Path) {
    crate::platform::mac::notify::run_notify(title, body, path);
}

/// Post a desktop notification (no `transient` hint, so it stays in the drawer)
/// and stay alive only long enough to catch a click on it — then reveal the
/// file. Exits as soon as the notification is closed (popup dismissed/expired)
/// or after a short backstop, so we don't linger like a daemon.
#[cfg(target_os = "linux")]
fn post(summary: &str, body: &str, path: &Path) {
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
            body,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Every (kind, outcome) pair the banner can be asked for, spelled out. This is the
    /// table to argue with if the wording changes — including the two the honesty rule
    /// exists for, where a copy was wanted and did not happen.
    #[test]
    fn the_banner_names_the_kind_and_never_overclaims_the_copy() {
        let p = PathBuf::from("/home/jane/Pictures/shot.png");
        let title = |k, o| notification_text(k, o, &p).0;

        // Copied: the kind leads, and the clipboard is stated.
        assert_eq!(title(Some(NotifyKind::Region), CopyOutcome::Copied), "Region copied to clipboard");
        assert_eq!(title(Some(NotifyKind::Window), CopyOutcome::Copied), "Window copied to clipboard");
        assert_eq!(title(Some(NotifyKind::Monitor), CopyOutcome::Copied), "Monitor copied to clipboard");
        // No kind (the preview editor's own save): the historical wording, unchanged.
        assert_eq!(title(None, CopyOutcome::Copied), "Copied to clipboard");
        assert_eq!(title(None, CopyOutcome::NotCopied), "Saved");

        // NOT copied, for any reason: "saved" — never a word about the clipboard.
        for outcome in [CopyOutcome::TooLarge, CopyOutcome::Failed, CopyOutcome::NotCopied] {
            for (kind, want) in [
                (NotifyKind::Region, "Region saved"),
                (NotifyKind::Window, "Window saved"),
                (NotifyKind::Monitor, "Monitor saved"),
            ] {
                assert_eq!(title(Some(kind), outcome), want, "{outcome:?}");
                assert!(!title(Some(kind), outcome).contains("clipboard"), "{outcome:?}");
            }
        }
    }

    /// The body always ends with the path (it is what the click acts on), and the two
    /// degraded outcomes lead with why the paste will be stale.
    #[test]
    fn the_body_carries_the_path_and_the_reason() {
        let p = PathBuf::from("/home/jane/Pictures/shot.png");
        let body = |o| notification_text(Some(NotifyKind::Region), o, &p).1;
        let shown = p.display().to_string();

        assert_eq!(body(CopyOutcome::Copied), shown);
        assert_eq!(body(CopyOutcome::NotCopied), shown);
        assert_eq!(body(CopyOutcome::TooLarge), format!("Too large to copy to the clipboard. {shown}"));
        assert_eq!(body(CopyOutcome::Failed), format!("The clipboard copy failed. {shown}"));
        for o in [CopyOutcome::Copied, CopyOutcome::TooLarge, CopyOutcome::Failed] {
            assert!(body(o).ends_with(&shown), "{o:?} must still name the file");
        }
    }

    /// Round-trip every (kind, outcome) through the argv the helper is launched with —
    /// the banner survives the re-exec or it says the wrong thing in another process.
    #[test]
    fn the_notification_survives_the_reexec_argv() {
        let kinds = [None, Some(NotifyKind::Region), Some(NotifyKind::Window), Some(NotifyKind::Monitor)];
        let outcomes =
            [CopyOutcome::Copied, CopyOutcome::TooLarge, CopyOutcome::Failed, CopyOutcome::NotCopied];
        for kind in kinds {
            for outcome in outcomes {
                let copied = matches!(outcome, CopyOutcome::Copied);
                let mut argv = vec![
                    "cosmic-capture-kit".to_string(),
                    if copied { NOTIFY_COPIED } else { NOTIFY_SAVED }.to_string(),
                    "/tmp/shot.png".to_string(),
                ];
                argv.extend(notify_argv_tail(kind, outcome).into_iter().map(str::to_string));
                assert_eq!(notify_from_argv(&argv, copied), (kind, outcome), "{kind:?} {outcome:?}");
            }
        }
    }

    /// A helper launched with no kind/reason tokens — or with tokens it does not know —
    /// degrades to the unnamed "Saved", never to a wrong name or an invented copy.
    #[test]
    fn unknown_or_missing_tokens_degrade_to_the_plain_banner() {
        let bare = ["cck".to_string(), NOTIFY_SAVED.to_string(), "/tmp/x.png".to_string()];
        assert_eq!(notify_from_argv(&bare, false), (None, CopyOutcome::NotCopied));

        let odd = [
            "cck".to_string(),
            NOTIFY_SAVED.to_string(),
            "/tmp/x.png".to_string(),
            NOTIFY_KIND.to_string(),
            "screen".to_string(),
            NOTIFY_REASON.to_string(),
            "gremlins".to_string(),
        ];
        assert_eq!(notify_from_argv(&odd, false), (None, CopyOutcome::NotCopied));
        // And a flag with nothing after it must not panic or read the wrong argument.
        let truncated = ["cck".to_string(), NOTIFY_SAVED.to_string(), NOTIFY_KIND.to_string()];
        assert_eq!(notify_from_argv(&truncated, false), (None, CopyOutcome::NotCopied));
    }

    /// Only `Copied` rides the copied flag; only the two explainable failures carry a
    /// reason token. This is what keeps the wire form from encoding an impossible state.
    #[test]
    fn the_wire_form_has_one_shape_per_outcome() {
        assert_eq!(CopyOutcome::Copied.reason_token(), None);
        assert_eq!(CopyOutcome::NotCopied.reason_token(), None);
        assert_eq!(CopyOutcome::TooLarge.reason_token(), Some("too-large"));
        assert_eq!(CopyOutcome::Failed.reason_token(), Some("copy-failed"));
        // A `copied` launch ignores any reason that somehow rode along.
        assert_eq!(CopyOutcome::from_wire(true, Some("too-large")), CopyOutcome::Copied);
    }
}
