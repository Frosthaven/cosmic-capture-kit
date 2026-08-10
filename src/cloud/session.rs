//! Cross-process upload session state (DRAGON-490).
//!
//! An upload runs in a DETACHED `--cloud-upload` child (see [`super::child`]): by the time
//! bytes are moving, the editor that asked for the upload is a SEPARATE, unrelated process,
//! and may not even still be open. Two things still need to cross that gap:
//!
//! * **Progress and outcome, child -> editor.** The editor's titlebar indicator and its
//!   completion toasts need to know how an upload it started is getting on.
//! * **Cancellation, editor -> child.** The editor's titlebar carries a Cancel control next
//!   to the indicator, and it has to reach a process it has no handle to.
//!
//! # Reusing the existing precedent
//!
//! [`crate::instance`] already solves this SHAPE for a different pair of processes (the
//! resident daemon and its capture siblings): a discoverable marker file under the runtime
//! dir, checked by whichever side needs the answer. This module is the same idea, keyed by a
//! SESSION id instead of a pid, because the editor mints the id before the child even exists
//! (it has to be in the child's own argv) and has no pid to key on until the child reports
//! one back — which is exactly the direction this exists to avoid needing.
//!
//! Two sidecars per session, both plain `std::fs`, both best-effort:
//!
//! * [`state_path`] — the CHILD writes its progress/outcome here, throttled to the same
//!   bucket cadence [`super::tray::BUCKET_STEP`] uses (a percent that has not moved a bucket
//!   is not worth a write). The EDITOR polls it on a cheap timer subscription while it has an
//!   upload it started still in this state. Deliberately NOT signal-based: a marker file the
//!   reader polls needs no signal-handling code on any platform, and the editor may not even
//!   be running when the terminal state lands.
//! * [`cancel_path`] — the EDITOR creates this (empty; its existence is the whole message)
//!   when the user presses Cancel from the titlebar. The child's transfer loop already calls
//!   back into its own progress reporter once per chunk (the natural point between one
//!   provider API call and the next), so checking for this file there costs nothing extra and
//!   needs no new wait of its own. The tray's OWN Cancel menu item (same process, DRAGON-490)
//!   sets the same in-process flag directly and never touches this file at all — the file
//!   only exists for the direction that has to cross a process boundary.
//!
//! # Why fixed tokens, not free text
//!
//! The state file carries no failure REASON, only `done` / `done-shared` / `failed` /
//! `canceled` / `percent:NN` / `indeterminate` (DRAGON-490 follow-up: a transfer with no real
//! midpoint to report). The editor's own toast wording is generic ("Upload failed"),
//! matching the desktop banner's own title (`share::notify::upload_notification_text`), which
//! already carries the detailed reason for the user who wants it. A provider's error text is
//! exactly the kind of string this module's privacy rules ([`super`]'s module doc) route
//! through `redact_oauth` before any sink; leaving it out of this file removes a second sink
//! to keep that rule for, rather than adding one.
//!
//! # Cleanup
//!
//! Neither sidecar is pid-keyed, so neither rides [`crate::instance::sweep_stale_markers`]'s
//! per-pid sweep. [`sweep_stale_sessions`] is this module's own equivalent: a session whose
//! state file is older than [`STALE_AFTER`] is assumed abandoned (the child crashed before
//! writing a terminal state, or the editor that would have consumed one closed and nobody
//! ever will) and both its sidecars are removed. Called from the same places
//! `sweep_stale_markers` already is, so a leftover session tidies up on the next ordinary
//! launch rather than needing a mechanism of its own.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// How long a session's sidecars may sit with no update before [`sweep_stale_sessions`]
/// treats it as abandoned.
///
/// Generous on purpose, the same reasoning as [`super::child::UPLOAD_BUDGET`]: a real
/// transfer over a slow link can go a while between chunk-sized progress writes, and this is
/// a cleanup floor, not a liveness probe. Comfortably above the child's own outer bound so a
/// session that is still legitimately running is never swept out from under it.
pub const STALE_AFTER: Duration = Duration::from_secs(45 * 60);

const _: () = assert!(
    STALE_AFTER.as_secs() > super::child::UPLOAD_BUDGET.as_secs(),
    "DRAGON-490: the cleanup floor must sit outside the child's own outer bound, or a slow \
     but legitimate upload could be swept while it is still running"
);

/// A fresh, random session id: filesystem-safe hex, one path component, safe in argv and
/// safe in a log (it identifies nothing about the user). The same shape
/// [`super::accounts::new_id`] mints an account id with, at a third of the length: this id
/// only has to be unique among the uploads one desktop session runs at once, not durable
/// forever like an account id is.
///
/// Empty on an unavailable OS random source, which the caller treats as "could not start a
/// session" rather than minting a guessable id.
pub fn new_session_id() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        log::error!("cloud upload: the OS random source is unavailable; no upload session id was minted");
        return String::new();
    }
    let mut out = String::with_capacity(16);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Whether `id` is a session id this module could have minted: non-empty, lowercase hex,
/// nothing that could smuggle a path separator. Pure; unit-tested.
///
/// The one gate between an argv value and a filename component: [`state_path`] and
/// [`cancel_path`] both interpolate their `id` argument directly, so a caller MUST check this
/// first for any `id` that did not come from [`new_session_id`] itself (in practice: the one
/// read back out of argv by the child, `child::run_cloud_upload`'s entry point).
pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// The sidecar the CHILD writes its progress/outcome to, for session `id`.
///
/// A hyphenated stem (`cosmic-capture-kit-upload.<id>.state`), deliberately NOT the dotted
/// `cosmic-capture-kit.<pid>.<suffix>` shape [`crate::instance`]'s per-pid markers use: that
/// shape is parsed as pid-dot-suffix by `instance::parse_marker_name`, and this sidecar is
/// keyed by a session id, not a pid, so it must never be mistaken for one by that sweep. The
/// hyphen stem is the same trick the lock/socket sidecars already use for the same reason
/// (see `instance::parse_marker_name`'s own doc).
pub fn state_path(id: &str) -> PathBuf {
    state_path_in(&crate::util::runtime_dir(), id)
}

fn state_path_in(dir: &str, id: &str) -> PathBuf {
    PathBuf::from(dir).join(format!("cosmic-capture-kit-upload.{id}.state"))
}

/// The sidecar the EDITOR creates to ask the child to cancel, for session `id`. Its
/// EXISTENCE is the whole message; it carries no content.
pub fn cancel_path(id: &str) -> PathBuf {
    cancel_path_in(&crate::util::runtime_dir(), id)
}

fn cancel_path_in(dir: &str, id: &str) -> PathBuf {
    PathBuf::from(dir).join(format!("cosmic-capture-kit-upload.{id}.cancel"))
}

/// What an upload session's state file records. The wire form [`encode_state`]/[`parse_state`]
/// convert to and from.
///
/// NOT `Copy` since DRAGON-507: [`UploadState::Done`] carries the remote file's id, which is
/// a `String`. The handful of by-value call sites took a borrow instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadState {
    /// Still transferring, 0-100 (already bucketed by the caller — this module does not
    /// re-bucket, [`super::tray::counter`] already owns that decision).
    Percent(u8),
    /// Still transferring, but no genuine interim percentage has arrived yet (DRAGON-490
    /// follow-up, dynamic detection — `cloud::tray::still_indeterminate`). Every upload
    /// starts here; the child switches to [`Self::Percent`] the moment a real one arrives, so
    /// a single-request upload (or a chunked one whose own first chunk covers the whole file)
    /// simply never leaves this state. A watcher should show a spinner instead of a
    /// percentage that would either freeze at one number or lie about how far along the
    /// transfer is.
    Indeterminate,
    /// Finished. `shared` is whether a share link was made AND copied to the clipboard BY THE
    /// CHILD, so the editor knows whether to ALSO fire the "Copied to clipboard" toast.
    ///
    /// DRAGON-553 narrowed that to "by the child" and it is load-bearing. On a session whose
    /// copies must go through a FOCUSED WINDOW ([`crate::share::CopyRoute::ThisWindow`]) the
    /// child cannot hold a selection at all, so it writes `shared: false` with the `url` still
    /// attached and the editor does the copy through its own window. `false` therefore means
    /// "not copied yet", never "no link".
    ///
    /// `file_id` is the provider's own id for the file that landed (DRAGON-507), so the
    /// EDITOR can undo the upload during the meter's finish-hold: by then the child has
    /// exited, and without this the editor knows a file exists but not which one. `None` is
    /// an OLD-FORMAT done state, written by a build before DRAGON-507 (or by a path that had
    /// no id to give), and it means exactly one thing to a reader: no undo is offered.
    ///
    /// **Not a secret, but never logged.** It is an opaque provider handle, and on at least
    /// one provider (Dropbox) it is the file's PATH, which is user content by any reading.
    /// The privacy rule in CLAUDE.md covers it: this module logs what happened, never what
    /// was uploaded.
    ///
    /// `url` is the SHARE LINK the child made, when it made one (DRAGON-520), so the editor's
    /// meter can offer to copy it AGAIN after the user has copied something else — and, since
    /// DRAGON-553, so the editor can make the FIRST copy on the window route. It is `None`
    /// when no link was made, and `None` for a done state written by a build before
    /// DRAGON-520; a reader treats both as "no copy control". It is deliberately INDEPENDENT
    /// of `shared`: the wire format carries the two separately and always has.
    ///
    /// **Never logged either, and for a stronger reason than the file id.** A share link
    /// opens the capture for anyone holding it. It is written to one file in the session
    /// runtime dir, read once by the editor, and removed with the rest of the session.
    Done { shared: bool, file_id: Option<String>, url: Option<String> },
    /// Did not finish.
    Failed,
    /// Stopped by a cancel request, from either the tray or the editor.
    Canceled,
}

/// Encode a state to the file's wire form. Pure; unit-tested.
///
/// A done state's file id (DRAGON-507) is appended as a THIRD field, `done:shared:<id>`, and
/// everything after that second colon is the id VERBATIM to the end of the LINE: provider ids
/// are opaque and at least one of them (Dropbox) is a path full of slashes and possibly colons,
/// so the id is never split, escaped or re-encoded. An id-less done state keeps the exact
/// two-field form older builds wrote and read, which is what makes the format change invisible
/// in both directions.
///
/// **The share link rides on a SECOND LINE** (DRAGON-520), for exactly the reason the id takes
/// the rest of the first one: with the id already claiming every remaining colon there is no
/// fourth field to be had, and a URL is full of colons and slashes itself. A line break is the
/// one separator neither value can contain (both are refused if they do), and a reader that only
/// looks at the first line is every build before this one, which is why an older reader still
/// parses a newer file correctly and simply does not see the link.
pub fn encode_state(state: &UploadState) -> String {
    match state {
        UploadState::Percent(n) => format!("percent:{}", n.min(&100)),
        UploadState::Indeterminate => "indeterminate".to_string(),
        UploadState::Done { shared, file_id, url } => {
            let head = if *shared { "done:shared" } else { "done:plain" };
            let mut out = match file_id {
                // An id with a newline in it would not survive the round trip (the reader
                // reads one line), so it is dropped rather than written half. The cost is one
                // upload with no undo; the alternative is a state file that parses as
                // something else entirely.
                Some(id) if !id.is_empty() && !id.contains('\n') => format!("{head}:{id}"),
                _ => head.to_string(),
            };
            // A URL with a line break in it is not a URL (`http::check_url` refuses a control
            // character outright), so this can only be a corrupted value; dropping it costs the
            // re-copy button and nothing else.
            if let Some(url) = url.as_deref().filter(|u| !u.is_empty() && !u.contains('\n')) {
                out.push('\n');
                out.push_str(url);
            }
            out
        }
        UploadState::Failed => "failed".to_string(),
        UploadState::Canceled => "canceled".to_string(),
    }
}

/// Decode a state file's contents. Pure; unit-tested. `None` for anything that is not one of
/// [`encode_state`]'s own outputs — a torn write (a poll caught the file mid-rewrite), a
/// missing/unreadable file the caller already turned into an empty string, or a stale format
/// from some future version. A caller sees this as "nothing new to report", never as a
/// failure: the state file is a courtesy, not a source of truth the upload itself depends on.
pub fn parse_state(text: &str) -> Option<UploadState> {
    // The FIRST line is the grammar every build has ever written; the second, when there is
    // one, is a done state's share link (DRAGON-520). Split before anything else is read, so
    // the id's "verbatim to the end" rule keeps meaning the end of its own line.
    let (head, tail) = text.split_once('\n').unwrap_or((text, ""));
    let head = head.trim();
    let url = || {
        let tail = tail.trim();
        (!tail.is_empty()).then(|| tail.to_string())
    };
    match head {
        "indeterminate" => Some(UploadState::Indeterminate),
        // The OLD two-field forms, still read exactly as they were: a done state written by a
        // build before DRAGON-507 says the upload finished and nothing about which file, which
        // is `file_id: None` and therefore no undo affordance.
        "done:shared" => Some(UploadState::Done { shared: true, file_id: None, url: url() }),
        "done:plain" => Some(UploadState::Done { shared: false, file_id: None, url: url() }),
        "failed" => Some(UploadState::Failed),
        "canceled" => Some(UploadState::Canceled),
        _ => {
            // The three-field done forms. `strip_prefix` rather than `split(':')`, because the
            // id is opaque and keeps every colon and slash it came with.
            for (prefix, shared) in [("done:shared:", true), ("done:plain:", false)] {
                if let Some(id) = head.strip_prefix(prefix) {
                    // An empty tail is an id-less done state written oddly, not an id of "".
                    let file_id = (!id.is_empty()).then(|| id.to_string());
                    return Some(UploadState::Done { shared, file_id, url: url() });
                }
            }
            let n: u8 = head.strip_prefix("percent:")?.parse().ok()?;
            (n <= 100).then_some(UploadState::Percent(n))
        }
    }
}

/// Whether a state is TERMINAL — the transfer is over, one way or another, and the editor
/// should stop watching this session once it has acted on it. Pure; unit-tested.
pub fn is_terminal(state: &UploadState) -> bool {
    !matches!(state, UploadState::Percent(_) | UploadState::Indeterminate)
}

/// Write `state` into session `id`'s state file (best-effort: a write that fails costs the
/// editor one missed update, never the upload itself). Overwrites atomically enough for this
/// purpose — a single small write to a file nothing else writes to — by truncating and
/// writing in one open, so a poll never reads a torn mix of two states.
///
/// A TRUE no-op for an empty `id`: every call site in `child.rs` calls this unconditionally
/// (a caller with no session id to watch should not have to guard every single call), so an
/// empty id has to mean "nobody is watching, write nothing" rather than quietly minting a
/// `cosmic-capture-kit-upload..state` file in the runtime dir for every unwatched upload.
pub fn write_state(id: &str, state: &UploadState) {
    if id.is_empty() {
        return;
    }
    let path = state_path(id);
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(encode_state(state).as_bytes());
    }
}

/// Read session `id`'s current state, if the file exists and parses. Best-effort: any error
/// (missing, unreadable, torn) reads as `None`, "nothing new to report" rather than a failure.
/// Also `None`, without touching the filesystem, for an empty `id` (see [`write_state`]).
pub fn read_state(id: &str) -> Option<UploadState> {
    if id.is_empty() {
        return None;
    }
    let text = std::fs::read_to_string(state_path(id)).ok()?;
    parse_state(&text)
}

/// Remove both of session `id`'s sidecars, best-effort. Called by the CHILD as it exits
/// (mirroring [`crate::instance`]'s per-pid markers: created on entering the state, removed
/// on leaving it) and by the EDITOR once it has consumed a terminal state, so ordinarily
/// neither sidecar outlives the session that made it by more than a poll interval. A no-op
/// for an empty `id` (see [`write_state`]).
pub fn clear_session(id: &str) {
    if id.is_empty() {
        return;
    }
    let _ = std::fs::remove_file(state_path(id));
    let _ = std::fs::remove_file(cancel_path(id));
}

/// Whether session `id` has a pending cancel request (the editor's titlebar Cancel button).
/// A bare existence check, so it costs nothing to poll between chunks. Always `false`,
/// without touching the filesystem, for an empty `id` (see [`write_state`]).
pub fn cancel_requested(id: &str) -> bool {
    !id.is_empty() && cancel_path(id).exists()
}

/// Ask session `id` to cancel: create its cancel sidecar. Best-effort and idempotent
/// (creating a file that already exists is a no-op success as far as the caller is
/// concerned); a caller that cannot even do this has no better fallback than to let the
/// upload keep running. A no-op for an empty `id` (see [`write_state`]).
pub fn request_cancel(id: &str) {
    if id.is_empty() {
        return;
    }
    let _ = std::fs::File::create(cancel_path(id));
}

/// Sweep every upload session sidecar whose STATE file is older than [`STALE_AFTER`].
///
/// Cheap (one `read_dir`) and best-effort, the same posture as
/// `crate::instance::sweep_stale_markers`: a sidecar this misses is retried on the next
/// launch that calls it, and one it removes early (a clock skew, a slow filesystem) costs
/// nobody anything a session file wasn't already going to lose — the upload itself does not
/// read these files, only the editor and the child's own cancel check do, and both treat a
/// missing file as "nothing to report" / "no cancel requested".
pub fn sweep_stale_sessions() {
    sweep_stale_sessions_in(&crate::util::runtime_dir(), SystemTime::now());
}

fn sweep_stale_sessions_in(dir: &str, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name
            .strip_prefix("cosmic-capture-kit-upload.")
            .and_then(|rest| rest.strip_suffix(".state"))
        else {
            continue;
        };
        let age = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| now.duration_since(m).ok());
        if age.is_none_or(|a| a >= STALE_AFTER) {
            clear_session_in(dir, id);
        }
    }
}

fn clear_session_in(dir: &str, id: &str) {
    let _ = std::fs::remove_file(state_path_in(dir, id));
    let _ = std::fs::remove_file(cancel_path_in(dir, id));
}

#[cfg(test)]
mod id_tests {
    use super::*;

    /// A freshly minted id is always valid by the module's own gate, and two mints do not
    /// collide (astronomically unlikely to, but the shape — length, alphabet — is what the
    /// gate actually checks).
    #[test]
    fn a_fresh_id_is_valid_hex_and_one_path_component() {
        let id = new_session_id();
        assert!(!id.is_empty());
        assert!(is_valid_session_id(&id), "{id}");
        assert!(!id.contains('/') && !id.contains('\\'));
        assert_eq!(id.len(), 16, "8 bytes hex-encoded");
        let other = new_session_id();
        assert_ne!(id, other, "two mints must not collide in practice");
    }

    /// The gate a caller MUST run any argv-sourced id through before it becomes a path
    /// component. Only lowercase hex, non-empty, bounded.
    #[test]
    fn the_gate_refuses_anything_that_is_not_plain_lowercase_hex() {
        assert!(is_valid_session_id("0123456789abcdef"));
        for bad in ["", "../../etc/passwd", "ABCDEF01", "has space", "with/slash", "with\\backslash"]
        {
            assert!(!is_valid_session_id(bad), "{bad:?} must be refused");
        }
        // Absurdly long is refused too, even if every character is otherwise valid hex.
        assert!(!is_valid_session_id(&"a".repeat(64)));
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    /// A done state with neither of its optional fields, which is what most of these tests want
    /// to say and what every pre-DRAGON-507 build wrote.
    fn done(shared: bool) -> UploadState {
        UploadState::Done { shared, file_id: None, url: None }
    }

    /// Every state the child can write round-trips through the wire form exactly.
    #[test]
    fn every_state_round_trips() {
        let with = |shared, file_id: Option<&str>, url: Option<&str>| UploadState::Done {
            shared,
            file_id: file_id.map(str::to_string),
            url: url.map(str::to_string),
        };
        for state in [
            UploadState::Percent(0),
            UploadState::Percent(1),
            UploadState::Percent(99),
            UploadState::Percent(100),
            UploadState::Indeterminate,
            done(true),
            done(false),
            // DRAGON-507: with an id, and with the two shapes a real provider id takes — an
            // opaque token, and Dropbox's, which is a PATH full of separators.
            with(true, Some("1AbC_dEf-99"), None),
            with(false, Some("/Apps/CCK/shot 1.png"), None),
            with(true, Some("id:with:colons"), None),
            // DRAGON-520: with the share link too, including beside an id that already claims
            // the whole first line, and including the colon-and-slash-heavy real thing.
            with(true, Some("id:with:colons"), Some("https://drive.google.com/file/d/1AbC/view?usp=sharing")),
            with(true, Some("/Apps/CCK/shot 1.png"), Some("https://www.dropbox.com/s/abc/shot%201.png?dl=0")),
            with(true, None, Some("https://1drv.ms/i/s!AbCdEf")),
            UploadState::Failed,
            UploadState::Canceled,
        ] {
            let wire = encode_state(&state);
            assert_eq!(parse_state(&wire), Some(state.clone()), "{wire:?} did not round-trip");
        }
    }

    /// **The link is a SECOND LINE, and an older reader never sees it** (DRAGON-520).
    ///
    /// The compatibility contract, in both directions: an old editor reads the first line, which
    /// is byte-identical to what it has always read, and simply offers no re-copy; a new editor
    /// reading an old child's file finds no second line and does the same.
    #[test]
    fn the_share_link_rides_on_its_own_line_and_older_readers_ignore_it() {
        let state = UploadState::Done {
            shared: true,
            file_id: Some("id:with:colons".to_string()),
            url: Some("https://example.test/s/abc".to_string()),
        };
        let wire = encode_state(&state);
        let (first, second) = wire.split_once('\n').expect("two lines");
        assert_eq!(first, "done:shared:id:with:colons", "the first line must not change shape");
        assert_eq!(second, "https://example.test/s/abc");
        // No link, no second line: nothing is written that an older reader would have to skip.
        assert!(!encode_state(&done(true)).contains('\n'));
        // A link that could not survive the round trip is dropped, exactly as an id is. The
        // upload keeps its undo and loses only the re-copy.
        let torn = UploadState::Done {
            shared: true,
            file_id: Some("f1".to_string()),
            url: Some("https://example.test/a\nb".to_string()),
        };
        assert_eq!(encode_state(&torn), "done:shared:f1");
        // An empty second line is no link, not a link of "".
        assert_eq!(parse_state("done:shared:f1\n"), Some(UploadState::Done {
            shared: true,
            file_id: Some("f1".to_string()),
            url: None,
        }));
    }

    /// A percent above 100 is clamped on the way OUT rather than ever written, so nothing
    /// downstream has to guard against an unrepresentable value.
    #[test]
    fn a_percent_is_clamped_before_it_is_ever_written() {
        assert_eq!(encode_state(&UploadState::Percent(255)), "percent:100");
    }

    /// DRAGON-507: a done state written by a build that had never heard of file ids still
    /// reads, and reads as "finished, nothing named". That is the compatibility contract in
    /// both directions — an old editor polling a new child sees a `done:shared:...` line it
    /// does not recognise and treats it as "nothing new to report", which is the same
    /// best-effort answer it already gives for a torn read, while a new editor polling an old
    /// child gets a done state with no undo offered.
    #[test]
    fn an_old_format_done_state_still_reads_and_simply_names_no_file() {
        assert_eq!(parse_state("done:shared"), Some(done(true)));
        assert_eq!(parse_state("done:plain"), Some(done(false)));
        // An id-less state is still WRITTEN in the old two-field form, so an older reader is
        // never handed a line it cannot parse unless there is genuinely an id to carry.
        assert_eq!(encode_state(&done(true)), "done:shared");
        assert_eq!(encode_state(&done(false)), "done:plain");
        // A trailing empty field is an id-less state written oddly, not an id of "".
        assert_eq!(parse_state("done:shared:"), Some(done(true)));
        // An id that cannot survive the round trip (the reader takes one trimmed line) is
        // dropped rather than written half: one upload with no undo beats a corrupt state file.
        let newline =
            UploadState::Done { shared: true, file_id: Some("a\nb".to_string()), url: None };
        assert_eq!(encode_state(&newline), "done:shared");
        // Whitespace around the id is the file's, not the id's: the reader trims the line.
        assert_eq!(
            parse_state("  done:plain:abc123  "),
            Some(UploadState::Done {
                shared: false,
                file_id: Some("abc123".to_string()),
                url: None
            })
        );
    }

    /// Anything that is not exactly one of this module's own outputs decodes to `None`, never
    /// a guess: a torn read (a poll catching the file mid-rewrite), an empty file, or garbage.
    #[test]
    fn unrecognised_or_torn_content_decodes_to_nothing() {
        for junk in ["", "percent:", "percent:101", "percent:-1", "percent:abc", "PERCENT:40", "done", "garbage"] {
            assert_eq!(parse_state(junk), None, "{junk:?} must not decode to a state");
        }
    }

    /// Only a percentage or the indeterminate state is NOT terminal; every named outcome is.
    #[test]
    fn only_a_percentage_or_indeterminate_is_non_terminal() {
        assert!(!is_terminal(&UploadState::Percent(0)));
        assert!(!is_terminal(&UploadState::Percent(99)));
        assert!(!is_terminal(&UploadState::Indeterminate));
        for terminal in [
            done(true),
            UploadState::Done { shared: false, file_id: Some("f".to_string()), url: None },
            UploadState::Failed,
            UploadState::Canceled,
        ] {
            assert!(is_terminal(&terminal), "{terminal:?} must be terminal");
        }
    }
}

#[cfg(test)]
mod filesystem_tests {
    use super::*;

    fn scratch_dir(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!("cck-upload-session-test-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.to_string_lossy().into_owned()
    }

    /// The two sidecars really are two different paths, and their names carry the hyphen
    /// stem, never the dotted `cosmic-capture-kit.<pid>.<suffix>` shape the per-pid markers
    /// use — the one thing that keeps `instance::parse_marker_name` from ever mistaking one
    /// for a pid marker.
    #[test]
    fn the_two_sidecars_are_distinct_and_never_look_like_a_pid_marker() {
        let state = state_path("abc123");
        let cancel = cancel_path("abc123");
        assert_ne!(state, cancel);
        for path in [&state, &cancel] {
            let name = path.file_name().unwrap().to_str().unwrap();
            assert!(name.starts_with("cosmic-capture-kit-upload."), "{name}");
            assert!(
                crate::instance::parse_marker_name(name).is_none(),
                "{name} must not parse as a per-pid marker"
            );
        }
    }

    /// Write, read, then clear: the ordinary lifecycle a poll and a child both drive.
    #[test]
    fn write_read_and_clear_round_trip_through_real_files() {
        let dir = scratch_dir("lifecycle");
        let id = "deadbeef01234567";
        let path = state_path_in(&dir, id);
        std::fs::write(&path, encode_state(&UploadState::Percent(40))).unwrap();
        assert_eq!(
            parse_state(&std::fs::read_to_string(&path).unwrap()),
            Some(UploadState::Percent(40))
        );
        std::fs::write(&path, encode_state(&UploadState::Done { shared: true, file_id: None, url: None }))
            .unwrap();
        assert_eq!(
            parse_state(&std::fs::read_to_string(&path).unwrap()),
            Some(UploadState::Done { shared: true, file_id: None, url: None })
        );
        clear_session_in(&dir, id);
        assert!(!path.exists());
        let _ = std::fs::remove_dir(&dir);
    }

    /// **The bug an empty id must never reproduce.** `child.rs` calls every public function
    /// here unconditionally, whether or not the editor gave it a session to watch, so an
    /// empty id has to be a true no-op — never a `cosmic-capture-kit-upload..state` file
    /// quietly minted in the runtime dir for every unwatched upload.
    #[test]
    fn an_empty_id_touches_no_file_at_all() {
        write_state("", &UploadState::Percent(50));
        write_state("", &UploadState::Done { shared: true, file_id: None, url: None });
        request_cancel("");
        assert_eq!(read_state(""), None);
        assert!(!cancel_requested(""));
        clear_session(""); // must not panic or touch anything either
        assert!(!state_path("").exists(), "an empty id must never create a state file");
        assert!(!cancel_path("").exists(), "an empty id must never create a cancel file");
    }

    /// A cancel request is a bare file whose existence is the whole answer, and asking twice
    /// is harmless.
    #[test]
    fn a_cancel_request_is_a_bare_marker_and_asking_twice_is_harmless() {
        let dir = scratch_dir("cancel");
        let id = "cafef00dcafef00d";
        let path = cancel_path_in(&dir, id);
        assert!(!path.exists());
        std::fs::File::create(&path).unwrap();
        assert!(path.exists());
        // A second create (what a double-click on Cancel would do) is a no-op, not an error.
        assert!(std::fs::File::create(&path).is_ok());
        let _ = std::fs::remove_file(&path);
    }

    /// The stale sweep removes a session whose state file is OLD, and leaves a fresh one
    /// alone — the property that keeps a real, still-running upload from being swept out
    /// from under it.
    #[test]
    fn the_stale_sweep_removes_old_sessions_and_spares_fresh_ones() {
        let dir = scratch_dir("stale");
        let old_id = "0000000000000001";
        let fresh_id = "0000000000000002";
        let old_state = state_path_in(&dir, old_id);
        std::fs::write(&old_state, encode_state(&UploadState::Percent(10))).unwrap();
        std::fs::write(cancel_path_in(&dir, old_id), "").unwrap();
        std::fs::write(state_path_in(&dir, fresh_id), encode_state(&UploadState::Percent(90))).unwrap();
        // Backdate the OLD session's mtime for real, rather than faking "now" for the whole
        // sweep (which would age the fresh session by the same amount and defeat the test).
        std::fs::File::options()
            .write(true)
            .open(&old_state)
            .unwrap()
            .set_modified(SystemTime::now() - STALE_AFTER - Duration::from_secs(1))
            .unwrap();

        sweep_stale_sessions_in(&dir, SystemTime::now());
        assert!(!old_state.exists(), "the old session must be swept");
        assert!(!cancel_path_in(&dir, old_id).exists(), "its cancel sidecar goes with it");
        assert!(state_path_in(&dir, fresh_id).exists(), "a session just written must survive");

        let _ = std::fs::remove_file(state_path_in(&dir, fresh_id));
        let _ = std::fs::remove_dir(&dir);
    }
}
