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

/// What became of an upload to a connected cloud account (DRAGON-482).
///
/// A separate vocabulary from [`CopyOutcome`] on purpose: the two banners answer different
/// questions ("is my capture on the clipboard" vs "did my capture reach my drive"), and
/// folding them together would mean one builder with two unrelated halves. The honesty rule
/// is the same one though, see [`upload_notification_text`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UploadOutcome<'a> {
    /// It landed, and there is a share link for it. Carries the link, which the banner's
    /// CLICK opens (it is never logged, and never printed into the banner text).
    Shared(&'a str),
    /// It landed, with no SHARE link: either the provider makes none, the user did not ask
    /// for one, or the request for one failed.
    ///
    /// Carries the provider's own WEB VIEW url for the uploaded file when it reported one
    /// (`RemoteFile::web_url`, already checked against that provider's hosts by
    /// `cloud::child`). It is not a share link, so it opens only for the user who uploaded
    /// it, which is exactly right for a click on their own banner: it lands them on the file
    /// in their drive instead of on a folder in their file manager. `None` when the provider
    /// reported none, or reported one this app would not open.
    Delivered(Option<&'a str>),
    /// It did not land. Carries the reason, already written as user-facing copy by whoever
    /// failed (the provider call, or the account lookup).
    Failed(&'a str),
}

/// Where a click on a banner goes. Both arms already exist as behaviours; this only names
/// the choice so one `post` can serve the capture banner (always a reveal) and the upload
/// banner (a link when there is one).
enum Click<'a> {
    /// Show the file in the desktop's file manager, the historical capture-banner click.
    Reveal(&'a Path),
    /// Hand a URI to the desktop's default handler: the share link, in a browser.
    Open(&'a str),
}

/// The upload banner's two lines. Pure; unit-tested.
///
/// The rules, which are the DRAGON-450 honesty rule applied to a different action:
///
/// * The title names the ACCOUNT, because a user with several connected drives needs to
///   know which one this was, and because "Uploaded" alone says nothing they did not
///   already ask for.
/// * A failure never says "uploaded". It says the upload failed, and the body is the
///   reason, so a user is not left comparing a banner against their drive.
/// * Nothing here claims the link is on the CLIPBOARD. The copy is done by a detached
///   worker on Linux and cannot be confirmed from this process, and a banner that asserts
///   an unverified copy is exactly what DRAGON-450 removed. What it says instead is true
///   everywhere: the link is ready, and clicking opens it.
pub fn upload_notification_text(label: &str, outcome: UploadOutcome<'_>) -> (String, String) {
    let who = if label.trim().is_empty() { "your cloud account" } else { label.trim() };
    match outcome {
        UploadOutcome::Shared(_) => (
            format!("Uploaded to {who}"),
            "The share link is ready. Click to open it.".to_string(),
        ),
        UploadOutcome::Delivered(Some(_)) => (
            format!("Uploaded to {who}"),
            format!("Your capture is in {who}. Click to open it."),
        ),
        UploadOutcome::Delivered(None) => {
            (format!("Uploaded to {who}"), format!("Your capture is in {who}."))
        }
        UploadOutcome::Failed(reason) => (format!("Upload to {who} failed"), reason.to_string()),
    }
}

/// Where the upload banner's click goes, best destination first. Pure; unit-tested.
///
/// The ladder, and each rung is better than the one under it:
///
/// 1. The SHARE link, when one was made. It is what the user asked for and what they would
///    paste to somebody else.
/// 2. The provider's WEB VIEW url for the file. Not shareable, but it opens the capture in
///    the user's own drive, which is where "uploaded" says it went. This rung is what makes a
///    failed share-link request still land somewhere useful rather than dropping to the local
///    file: the upload itself worked, so sending the user to their file manager would
///    understate what happened.
/// 3. The LOCAL copy. A user who reads "uploaded" and clicks wants to end up somewhere, and
///    with neither url this is the only place we can send them. `local` is the STAGED copy
///    (`cloud::upload::stage_for_upload`), which is why that copy is kept rather than deleted
///    at the end of the transfer, and why its name carries the process and a sequence number:
///    the file this reveals is always the one this banner is about.
fn upload_click<'a>(outcome: UploadOutcome<'a>, local: &'a Path) -> Click<'a> {
    match outcome {
        UploadOutcome::Shared(url) => Click::Open(url),
        UploadOutcome::Delivered(Some(url)) => Click::Open(url),
        UploadOutcome::Delivered(None) | UploadOutcome::Failed(_) => Click::Reveal(local),
    }
}

/// Post the upload banner. Runs IN the detached upload child (`cloud::child`), which is
/// already a short-lived process of its own, so there is no second re-exec: the process
/// that did the upload owns the banner and its click, exactly as the `--notify-*` helper
/// does for a capture.
///
/// Nothing logged here carries the link, the label or the path.
pub fn run_upload_notify(label: &str, outcome: UploadOutcome<'_>, local: &Path) {
    let (title, body) = upload_notification_text(label, outcome);
    post(&title, &body, upload_click(outcome, local));
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
    const NAME: &str = "dev.thedragon.CosmicCaptureKit";
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
    // DRAGON-519: this helper is a detached re-exec of our own binary, so a sibling
    // committing a capture matched it by exe path and SIGTERMed it. The banner belongs to the
    // notification service and survives that, but the CLICK is handled HERE, in `post`'s
    // signal loop (Linux) or its delegate (macOS), so a killed helper turns "click to reveal
    // your capture" into nothing happening. For a "(no editor)" capture (DRAGON-428) that
    // click is the entire delivery. The guard's scope is exactly `post`'s: about 20s on
    // Linux, 5 minutes on macOS, and instant on Windows, where the toast's click re-enters us
    // through the URI scheme rather than through this process. See `instance::SHARE_MARKER`.
    let _lingering = crate::instance::ShareMarker::new();
    let (title, body) = notification_text(kind, outcome, path);
    post(&title, &body, Click::Reveal(path));
}

/// Windows (DRAGON-229): dispatch to the WinRT toast body under `platform/windows/`
/// (closed split). The click target rides along as the toast's protocol-activation launch
/// URI: our own `cosmic-capture-kit:reveal?path=` scheme for a file (DRAGON-450), or the
/// share link itself for an upload (DRAGON-482), which the shell hands to the browser. Both
/// are `activationType="protocol"`, so neither needs a COM activator.
#[cfg(target_os = "windows")]
fn post(title: &str, body: &str, click: Click<'_>) {
    match click {
        Click::Reveal(path) => crate::platform::windows::services::run_notify(title, body, path),
        Click::Open(uri) => crate::platform::windows::services::run_notify_uri(title, body, uri),
    }
}

/// Any other (non-Linux/macOS/Windows) target: no notification path.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos"), not(target_os = "windows")))]
fn post(_title: &str, _body: &str, _click: Click<'_>) {
    log::debug!("desktop notification is not implemented on this platform");
}

/// macOS (DRAGON-230): dispatch to the notification body under `platform/mac/`
/// (closed split). A bundled `.app` posts a UNUserNotificationCenter banner whose
/// click reveals the file (or opens the share link, DRAGON-482); an unbundled dev binary
/// degrades to a click-less `osascript` banner.
#[cfg(target_os = "macos")]
fn post(title: &str, body: &str, click: Click<'_>) {
    match click {
        Click::Reveal(path) => crate::platform::mac::notify::run_notify(title, body, path),
        Click::Open(uri) => crate::platform::mac::notify::run_notify_uri(title, body, uri),
    }
}

/// Post a desktop notification (no `transient` hint, so it stays in the drawer)
/// and stay alive only long enough to catch a click on it, then act on `click`
/// (reveal the file, or open the share link). Exits as soon as the notification is
/// closed (popup dismissed/expired) or after a short backstop, so we don't linger
/// like a daemon.
#[cfg(target_os = "linux")]
fn post(summary: &str, body: &str, click: Click<'_>) {
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
                    match click {
                        Click::Reveal(path) => run_reveal(path),
                        // A real URL, so the portal's OpenURI is the right handler for it
                        // (the folder special-case in `run_open_uri` only touches `file://`).
                        Click::Open(uri) => super::open::run_open_uri(uri),
                    }
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

    /// The upload banner names the ACCOUNT in every line it can, and a failure never reads
    /// as a success. This is the table to argue with if the upload wording changes.
    #[test]
    fn the_upload_banner_names_the_account_and_never_overclaims() {
        let text = |o| upload_notification_text("Work Drive", o);

        assert_eq!(
            text(UploadOutcome::Shared("https://example.test/x")),
            (
                "Uploaded to Work Drive".to_string(),
                "The share link is ready. Click to open it.".to_string()
            )
        );
        assert_eq!(
            text(UploadOutcome::Delivered(None)),
            ("Uploaded to Work Drive".to_string(), "Your capture is in Work Drive.".to_string())
        );
        // With a view url there IS somewhere to click, and the body says so rather than
        // leaving the user to guess whether the banner is inert.
        assert_eq!(
            text(UploadOutcome::Delivered(Some("https://drive.example.test/file/abc"))),
            (
                "Uploaded to Work Drive".to_string(),
                "Your capture is in Work Drive. Click to open it.".to_string()
            )
        );
        // A failure says so, and the reason rides through verbatim (it is already copy).
        let (title, body) = text(UploadOutcome::Failed("The account is no longer connected."));
        assert_eq!(title, "Upload to Work Drive failed");
        assert_eq!(body, "The account is no longer connected.");
        assert!(!title.contains("Uploaded to"), "a failure must not read as a success");

        // The LINK never appears in the text: it is what the click opens, not what the
        // banner says, and a banner is read by anyone looking at the screen.
        let secret = "https://drive.example.test/file/abc123";
        let (t, b) = text(UploadOutcome::Shared(secret));
        assert!(!t.contains(secret) && !b.contains(secret));

        // Nothing claims the clipboard, which this process cannot verify (DRAGON-450).
        for o in [
            UploadOutcome::Shared(secret),
            UploadOutcome::Delivered(None),
            UploadOutcome::Delivered(Some(secret)),
        ] {
            assert!(!text(o).1.contains("clipboard"), "{o:?} claims an unverified copy");
            // …and the view url is no more printable than the share link is.
            let (t, b) = text(o);
            assert!(!t.contains(secret) && !b.contains(secret), "{o:?} printed a url");
        }
    }

    /// An account with no label still produces a sentence, not a blank. The same fallback
    /// the tray tooltip uses, so the two surfaces name a nameless account identically.
    #[test]
    fn an_unlabelled_account_still_reads_as_a_sentence() {
        for label in ["", "   "] {
            let (title, body) = upload_notification_text(label, UploadOutcome::Delivered(None));
            assert_eq!(title, "Uploaded to your cloud account");
            assert_eq!(body, "Your capture is in your cloud account.");
        }
        // A real label is trimmed rather than rendered with its whitespace.
        assert_eq!(
            upload_notification_text("  Work  ", UploadOutcome::Delivered(None)).0,
            "Uploaded to Work"
        );
        // House rule: no em/en-dashes in user-facing copy.
        for o in [
            UploadOutcome::Shared("https://x.test"),
            UploadOutcome::Delivered(None),
            UploadOutcome::Delivered(Some("https://x.test")),
        ] {
            let (t, b) = upload_notification_text("Work", o);
            for s in [t, b] {
                assert!(!s.contains('\u{2014}') && !s.contains('\u{2013}'), "dash in {s:?}");
            }
        }
    }

    /// The click ladder: share link, then the provider's view url, then the local copy.
    /// The middle rung is what keeps an upload that LANDED from sending the user to their
    /// file manager just because the share-link request failed.
    #[test]
    fn the_upload_click_prefers_the_link_then_the_view_url_then_the_file() {
        let local = PathBuf::from("/run/user/1000/cck-upload-42-0.png");
        let url = "https://drive.example.test/file/abc123";
        let view = "https://drive.example.test/file/abc123/view";
        assert!(matches!(
            upload_click(UploadOutcome::Shared(url), &local),
            Click::Open(u) if u == url
        ));
        assert!(matches!(
            upload_click(UploadOutcome::Delivered(Some(view)), &local),
            Click::Open(u) if u == view
        ));
        assert!(matches!(
            upload_click(UploadOutcome::Delivered(None), &local),
            Click::Reveal(p) if p == local
        ));
        // A FAILURE never opens a url, even if one is somehow to hand: the capture is not
        // there, and sending the user to the provider would say it was.
        assert!(matches!(
            upload_click(UploadOutcome::Failed("nope"), &local),
            Click::Reveal(p) if p == local
        ));
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
