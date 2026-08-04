//! The `--cloud-upload` helper process (DRAGON-482).
//!
//! An upload runs in a DETACHED re-exec of this binary, the same technique every other
//! post-capture action uses (`share::reexec`): a single-threaded child does the network
//! work and the editor is free to close. That matters more here than for a clipboard copy,
//! because an upload can take minutes and the app's whole model is one-shot.
//!
//! [`run_cloud_upload`] is that child's WHOLE life, in order: resolve the account, raise the
//! tray counter, hand the file to [`super::providers`], copy the share link if one was asked
//! for and the provider makes them, post the banner, exit. Every one of those steps is
//! best-effort AFTER the upload itself: a link that could not be made is a warning in the
//! log, not a failed upload. The user's capture is in their drive either way.
//!
//! # Nothing here waits without a deadline
//!
//! DRAGON-118's rule reaches this process too, and here it is one ring: [`UPLOAD_BUDGET`],
//! armed on a detached thread before anything else happens. Inside it, the transfer's own
//! bounds are the HTTP transport's (`super::http`), the tray's handshakes are bounded in
//! their platform files, and the banner's click window is the notifier's existing 20s
//! backstop. Nothing in this file blocks on anything that has no clock.
//!
//! # Why this path records no `diag::Failure`
//!
//! The failure vocabulary (`diag::Failure`) classifies a CAPTURE SESSION that delivered
//! nothing, and `App::fail_session` is how it reaches the user. This process is not a
//! capture session: the capture was already delivered to the editor (and, with "save
//! originals" on, to disk) long before an upload was asked for. Its own outcome reaches the
//! user through the banner it posts, which is the surface this feature owns. Adding an
//! upload code to that closed vocabulary would be the second failure vocabulary CLAUDE.md
//! forbids.
//!
//! # Privacy
//!
//! Nothing here logs the file's path (only [`crate::diag::path_shape`]), the share URL, the
//! remote file id or a token. The account id IS logged: it is random hex from
//! [`super::accounts::new_id`] and identifies nothing about the user, and without it a
//! two-account failure could not be told apart in a report.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::ProviderCaps;
use super::accounts::CloudAccount;
use super::session;
use super::upload::tray::UploadTray;
use crate::share::UploadOutcome;

/// How often the cancel-poll thread checks session `id`'s cancel marker (DRAGON-490).
///
/// Internal to this process only (nothing here is a UI subscription pace), so it can be
/// tighter than the editor's own poll: this is the responsiveness a user seeing an "X" in the
/// titlebar or the tray actually gets, and a chunk's own transfer time dwarfs this either way.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// The outer bound on one upload child's life.
///
/// Generous on purpose: this is not a timeout on the transfer, it is the answer to "what if
/// the transfer never ends at all". A recording of a long meeting over a slow uplink can
/// legitimately take many minutes, and killing that would lose a real upload; a child still
/// running after half an hour is stuck on something that is not going to finish. The
/// transport's own per-request timeouts are the inner ring and do the ordinary work.
///
/// The exit code is 1 rather than 0 for the record, though nothing reads it: the child is
/// detached, so its parent is gone and its status is collected by init. The visible signal
/// is the tray counter disappearing without a banner following it.
pub const UPLOAD_BUDGET: Duration = Duration::from_secs(30 * 60);

/// What the banner says when the account named in argv is not in the accounts file: a
/// disconnect (or a settings edit) between the editor asking for the upload and this child
/// starting. Named rather than inlined so the wording can be checked without running the
/// upload, which would post a real notification.
const ACCOUNT_GONE: &str = "That cloud account is no longer connected.";

/// Run an upload for `path` against the account named by `account_id`, optionally creating
/// and copying a share link. Called from `main`'s argv dispatch, before any GUI init.
///
/// `session_id` is empty for a caller with no cross-process watcher (a terminal run, or an
/// older editor build that never minted one); this whole function degrades to exactly its
/// pre-DRAGON-490 behaviour in that case, since every session-marker write below is a no-op
/// on an empty id (see [`session::write_state`] and [`spawn_cancel_poll`]).
///
/// `Err` is returned for the log (and for a terminal run's stderr); the USER has already
/// been told by the banner this posts (UNLESS the upload was cancelled, which posts none: see
/// [`cancel_cleanup`]'s call site, which returns `Ok(())` for every cancelled ending), so a
/// caller must not post a second one.
///
/// **A cancel is honoured whether or not the transfer could be stopped** (DRAGON-495). The
/// per-chunk check inside the provider only exists on the chunked path; a small file goes up in
/// one request with no cancel point at all, so the flag is read AGAIN here and a file that
/// landed anyway is deleted back off the provider. See [`upload_ending`].
pub fn run_cloud_upload(
    path: &Path,
    account_id: &str,
    auto_share: bool,
    session_id: &str,
) -> Result<(), String> {
    arm_backstop(UPLOAD_BUDGET);
    // **Say we are here, before anything else can be interrupted** (DRAGON-516).
    //
    // This process is a detached re-exec of the app's own binary, so a SIBLING that commits a
    // capture finds it in its exe-path sweep (`instance::close_other_instances`) and, until
    // this marker existed, SIGTERMed it. Nothing here installs a SIGTERM handler, so taking a
    // screenshot during an upload simply killed the upload: the tray counter froze on its last
    // bucket and the editor's meter froze with it, because this process never reached any of
    // the `session::write_state` calls below that would have said how it ended.
    //
    // FIRST statement of the function, so the window between `Command::spawn` and being
    // spare-able is only this process's own startup. The guard drops at every `return` in this
    // function; a kill or the backstop's `process::exit` leaves it for the stale sweep.
    let _uploading = crate::instance::UploadMarker::new();
    // Best-effort, cheap (one `read_dir`), and tied to the one path that ever mints a
    // session (DRAGON-490): sweeps any sidecar left behind by a PAST upload that crashed
    // before reporting a terminal state, or whose watching editor closed and never will read
    // it. The same posture `instance::sweep_stale_markers` has for its own per-pid markers.
    session::sweep_stale_sessions();
    log::debug!(
        "cloud upload: starting ({}) to account {account_id} (auto_share={auto_share}, watched={})",
        crate::diag::path_shape(path),
        !session_id.is_empty()
    );

    // The account. Gone means a disconnect (or a settings edit) between the editor asking
    // for the upload and this child starting, which is rare but entirely possible: say so
    // out loud rather than exiting quietly on a job the user asked for.
    let Some(acct) = super::accounts::get(account_id) else {
        log::warn!("cloud upload: account {account_id} is not in the accounts file");
        // No label to name it by, so the banner falls back to naming the feature.
        crate::share::run_upload_notify("", UploadOutcome::Failed(ACCOUNT_GONE), path);
        session::write_state(session_id, &session::UploadState::Failed);
        return Err(ACCOUNT_GONE.to_string());
    };
    let label = acct.display_label();
    let caps = acct.spec().map(|s| s.caps);

    // ONE cancel flag, TWO triggers (DRAGON-490): the tray's own Cancel menu item sets it
    // directly (same process), and this poll thread sets it when the editor's cancel request
    // arrives cross-process through `session_id`'s marker file. The provider's per-chunk loop
    // reads only this flag, so it never has to know which trigger fired.
    let canceled = Arc::new(AtomicBool::new(false));
    let _cancel_poll = if session_id.is_empty() {
        None
    } else {
        Some(spawn_cancel_poll(session_id.to_string(), canceled.clone()))
    };

    // The counter, from the first byte to the last, and then a few seconds of the OUTCOME.
    // `finish` swaps the spinner (or a number, if one arrived) for a tick or a cross, holds it
    // for `tray::FINISH_HOLD` and then removes the item, so the tray says how the upload
    // ended rather than the counter simply vanishing (which looked identical for a success, a
    // failure and a crash). Its OWN Cancel menu item shares the same flag every other trigger
    // does.
    //
    // DRAGON-490 dynamic follow-up: every upload starts as the spinner, unconditionally
    // (`UploadTray::start`'s own doc has the why). Written ONCE here too, so the editor's very
    // first poll already shows the spinner rather than nothing at all before any progress has
    // arrived.
    let mut tray = UploadTray::start(&label, canceled.clone());
    session::write_state(session_id, &session::UploadState::Indeterminate);
    let mut last_written: Option<u8> = None;
    let uploaded = super::providers::upload(
        &acct,
        path,
        &mut |percent| {
            // `tray` is the ONE source of truth for whether this transfer has switched to a
            // real percentage yet (`tray::still_indeterminate`); reading it back after
            // feeding it `percent` is what keeps the session marker in lockstep with the tray
            // rather than making its own, possibly-disagreeing decision.
            tray.set_percent(percent);
            if tray.is_indeterminate() {
                // Nothing genuine yet: the session marker already says `Indeterminate` from
                // the write above, and every call while this stays true is either the
                // synthetic 0 `providers::upload` makes before dispatching, or the synthetic
                // 100 every provider makes once at the very end.
                return;
            }
            // Bucketed the same way the tray's own redraw is (`tray::counter`), so a session
            // watcher is not worth a file write for every single percent tick.
            if let Some(bucket) = super::upload::tray::counter(percent)
                && Some(bucket) != last_written
            {
                last_written = Some(bucket);
                session::write_state(session_id, &session::UploadState::Percent(bucket));
            }
        },
        &|| canceled.load(Ordering::Relaxed),
    );
    // **The cancel is decided HERE, not only inside the transfer** (DRAGON-495, the reported
    // bug). Every provider sends a SMALL file (a screenshot: under 4-8 MB depending on the
    // provider) as ONE request, on a path that never consults `should_cancel` because it has
    // no chunk boundary to consult it at. Cancel therefore did nothing at all for the common
    // case: the request completed, the file landed, and the child carried on to the share
    // link and the banner as though nothing had been asked. Re-reading the flag
    // after the transfer is what makes the answer honest for the single-request path AND for
    // the genuine race on the chunked one, where a cancel can arrive during the final chunk.
    //
    // `is_canceled` is still consulted: a chunked transfer that STOPPED itself reports it that
    // way, and that distinction (did the provider commit anything?) is exactly what decides
    // whether there is a remote file to clean up.
    let stopped_itself = uploaded.as_ref().err().is_some_and(|r| super::providers::is_canceled(r));
    let ending = upload_ending(
        canceled.load(Ordering::Relaxed) || stopped_itself,
        uploaded.is_ok(),
    );
    tray.finish(tray_ending(ending));

    if let Some(cleanup) = cancel_cleanup(ending) {
        // The user asked for this, from the tray or the editor: not a failure, and no desktop
        // banner. The editor's own toast ("Upload canceled") is what confirms it landed for
        // whoever asked; the tray item taking itself away is what confirms it for a user who
        // only has the tray. Logged at debug, not warn: this is expected behaviour.
        log::debug!("cloud upload: account {account_id}'s upload was canceled ({cleanup:?})");
        if cleanup == CancelCleanup::DeleteRemote
            && let Ok(file) = uploaded.as_ref()
        {
            // The race the single-request path makes ordinary rather than rare: the bytes are
            // already at the provider, so the only way to honour the cancel is to take them
            // back off it.
            delete_remote_best_effort(&acct, &file.id);
        }
        session::write_state(session_id, &session::UploadState::Canceled);
        remove_staged(path);
        return Ok(());
    }

    let file = match uploaded {
        Ok(file) => file,
        Err(reason) => {
            // Through `redact_oauth`, because a provider's own message is the one string
            // here that we did not write and cannot vouch for: it is the sanctioned filter
            // for anything that might carry a credential (see `cloud`'s privacy note).
            log::warn!(
                "cloud upload: account {account_id} did not accept the file: {}",
                crate::diag::redact_oauth(&reason)
            );
            session::write_state(session_id, &session::UploadState::Failed);
            // The RECONNECT prefix comes off before the reason becomes banner copy. It earns
            // its place beside a Reconnect BUTTON, which is the settings page and nowhere
            // else; a banner has no button, so the words would label an affordance that is
            // not there. What is left is already a whole sentence telling the user to connect
            // the account again.
            let banner = super::oauth::reconnect_reason(&reason);
            crate::share::run_upload_notify(&label, UploadOutcome::Failed(banner), path);
            return Err(reason);
        }
    };
    log::debug!("cloud upload: account {account_id} accepted the file");

    // The share link. Best-effort from here down: the file has landed, so nothing below can
    // turn this into a failed upload.
    let link = if share_link_wanted(auto_share, caps) {
        match super::providers::create_share_link(&acct, &file.id) {
            // The link is CHECKED before it is used (DRAGON-482). It is provider output, and
            // its two consumers are the clipboard and the banner's click target, which on
            // Windows is a toast's protocol-activation launch value: a URL we cannot vouch for
            // is one the shell would hand to whatever handles its scheme. `web_url_allowed`
            // asks whether it belongs to THIS account's provider, not merely whether it is a
            // host the app talks to.
            Ok(url) if super::web_url_allowed(&acct.provider, &url) => {
                // The clipboard write is a detached worker on Linux and inline elsewhere, so
                // it is fire-and-forget by construction. That is exactly why the banner says
                // the link is READY rather than claiming it is on the clipboard.
                crate::share::copy_text(&url);
                Some(url)
            }
            Ok(_) => {
                // Behaviour, not content: the rejected URL is never logged.
                log::warn!(
                    "cloud upload: account {account_id} returned a share link on an address \
                     {} does not use; it was not offered",
                    acct.provider
                );
                None
            }
            Err(e) => {
                // Redacted for the same reason as the upload's own failure above. There is
                // no link in this arm to leak, but the message is still the provider's.
                log::warn!(
                    "cloud upload: no share link for account {account_id}: {}",
                    crate::diag::redact_oauth(&e)
                );
                None
            }
        }
    } else {
        None
    };
    // Terminal state for whoever is watching (DRAGON-490): `shared` is whether the link
    // above was BOTH made and handed to the clipboard, so the editor knows whether to fire
    // its own "Copied to clipboard" toast alongside "Uploaded".
    // DRAGON-507: the file's own id rides along, so the EDITOR can undo this upload
    // during the meter's finish-hold. The child is about to exit; without the id the
    // editor would know a file landed but not which one to take back.
    // DRAGON-520: the share LINK rides along too, for the same reason and no other. The
    // editor's meter offers to copy it again, for the user who copied something else in
    // between; this process is the only one that ever holds it, and re-deriving it in the
    // editor would mean a second `create_share_link` against the provider.
    session::write_state(
        session_id,
        &session::UploadState::Done {
            shared: link.is_some(),
            file_id: Some(file.id.clone()),
            url: link.clone(),
        },
    );

    // The provider's own view url for the file, checked the same way the share link is: it
    // becomes the banner's click target when there is no share link, including when making
    // one FAILED, so an upload that landed still opens the capture in the user's drive rather
    // than dropping them into their file manager.
    let web = file.web_url.as_deref().filter(|url| {
        let ok = super::web_url_allowed(&acct.provider, url);
        if !ok {
            log::debug!(
                "cloud upload: account {account_id} reported a view address {} does not use; \
                 the banner will reveal the local copy instead",
                acct.provider
            );
        }
        ok
    });
    let outcome = match &link {
        Some(url) => UploadOutcome::Shared(url),
        None => UploadOutcome::Delivered(web),
    };
    crate::share::run_upload_notify(&label, outcome, path);
    Ok(())
}

/// How an upload ENDED, once the cancel flag has been read against what the provider actually
/// committed (DRAGON-495).
///
/// Four states, because "was it cancelled" and "did the bytes land" are independent questions
/// and the interesting case is the one where BOTH are true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadEnding {
    /// Not cancelled, the file is at the provider: deliver it (share link, ledger, banner).
    Deliver,
    /// Not cancelled, the transfer failed: report it.
    Failed,
    /// Cancelled, and the provider never committed anything. Nothing remote to clean up.
    CanceledInFlight,
    /// Cancelled, but the file IS at the provider: the cancel lost the race and the only way
    /// to honour it is to delete what landed.
    CanceledCommitted,
}

/// What ending an upload reached. Pure; unit-tested.
///
/// `committed` is simply whether the transfer returned a file. The pairing is the whole
/// decision: a cancel that arrives after the provider has committed CANNOT be honoured by
/// stopping anything, only by deleting what landed, and that is not a rare case here (see the
/// call site: every small file goes up in ONE request that has no cancel point at all).
pub fn upload_ending(canceled: bool, committed: bool) -> UploadEnding {
    match (canceled, committed) {
        (true, true) => UploadEnding::CanceledCommitted,
        (true, false) => UploadEnding::CanceledInFlight,
        (false, true) => UploadEnding::Deliver,
        (false, false) => UploadEnding::Failed,
    }
}

/// What the tray should show for `ending`. Pure; unit-tested.
pub fn tray_ending(ending: UploadEnding) -> super::upload::tray::Ending {
    match ending {
        UploadEnding::Deliver => super::upload::tray::Ending::Done,
        UploadEnding::Failed => super::upload::tray::Ending::Failed,
        UploadEnding::CanceledInFlight | UploadEnding::CanceledCommitted => {
            super::upload::tray::Ending::Canceled
        }
    }
}

/// What a cancelled upload has to clean up at the PROVIDER, or `None` when the upload was not
/// cancelled at all. Pure; unit-tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelCleanup {
    /// The provider committed nothing, so there is nothing of ours on it.
    ///
    /// A resumable SESSION may still be open (the chunked path), and that is deliberately not
    /// chased here for Google, Dropbox and YouTube: an abandoned upload session expires on the
    /// provider's own schedule (Google and YouTube give a resumable session about a week,
    /// Dropbox expires an idle upload session likewise), it holds no file the user can see, and
    /// it costs no quota. OneDrive is the exception and is handled where the session lives
    /// (`providers::onedrive`, which DELETEs its `uploadUrl`), because Graph documents that
    /// call and the session there is addressable without a token.
    SessionAbandoned,
    /// The file is at the provider and must be deleted again.
    DeleteRemote,
}

/// The cleanup `ending` calls for. Pure; unit-tested.
pub fn cancel_cleanup(ending: UploadEnding) -> Option<CancelCleanup> {
    match ending {
        UploadEnding::Deliver | UploadEnding::Failed => None,
        UploadEnding::CanceledInFlight => Some(CancelCleanup::SessionAbandoned),
        UploadEnding::CanceledCommitted => Some(CancelCleanup::DeleteRemote),
    }
}

/// How many times a cancel's remote delete is attempted before it is given up on.
///
/// TWO: one retry, no more. This runs while the user is waiting for a cancelled upload to be
/// over, and the delete is best effort by nature (the file is the provider's now). A longer
/// ladder would turn "cancel" into a visibly slow operation to buy a marginal chance on a
/// provider that is already refusing.
const CANCEL_DELETE_ATTEMPTS: u32 = 2;

/// Delete a file that a cancel lost the race to, best effort (DRAGON-495).
///
/// Bounded by [`CANCEL_DELETE_ATTEMPTS`] and by the transport's own per-request budget, so
/// this can never turn a cancel into a hang (CLAUDE.md: nothing waits unboundedly). A failure
/// is a debug line carrying the ATTEMPT COUNT and nothing else: the user asked for a cancel,
/// not for a report on the provider's delete API, and the file id is not ours to log.
fn delete_remote_best_effort(acct: &CloudAccount, file_id: &str) {
    for attempt in 1..=CANCEL_DELETE_ATTEMPTS {
        match super::providers::delete_file(acct, file_id) {
            Ok(()) => {
                log::debug!(
                    "cloud upload: a cancel arrived after the file landed; it was deleted \
                     again (attempt {attempt})"
                );
                return;
            }
            Err(e) => {
                log::debug!(
                    "cloud upload: could not delete the file a cancel raced (attempt \
                     {attempt}/{CANCEL_DELETE_ATTEMPTS}): {}",
                    crate::diag::redact_oauth(&e)
                );
            }
        }
    }
    // Said once, plainly, so the debug log shows the outcome and not just the attempts.
    log::debug!(
        "cloud upload: the file a cancel raced is still at the provider after \
         {CANCEL_DELETE_ATTEMPTS} attempts"
    );
}

/// Drop the staged copy a cancelled upload no longer needs (DRAGON-495).
///
/// The staged copy normally OUTLIVES this process on purpose (`upload::stage_for_upload`: it
/// is what the delivered-upload notification reveals when there is no share link). A cancel
/// posts no notification and delivers nothing, so nothing will ever reveal this copy, and
/// leaving it would mean a cancelled upload silently costing disk until the staging directory
/// is next swept. Best effort, and the path is DESCRIBED rather than printed (the privacy
/// rule); a failure changes nothing the user can see, since both staging directories are
/// self-tidying anyway.
fn remove_staged(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        log::debug!(
            "cloud upload: the staged copy of a cancelled upload could not be removed ({}): {e}",
            crate::diag::path_shape(path)
        );
    }
}

/// Watch session `id`'s cancel marker on its own thread and flip `canceled` the moment it
/// appears (DRAGON-490). DETACHED: like [`arm_backstop`], it holds nothing this process needs
/// back, so it never keeps the process alive on the normal path and needs no explicit stop —
/// it simply ends when `main` returns and takes this thread with it. A no-op body (never
/// spawned at all) when `id` is empty, handled by the caller rather than in here, so this
/// function's own contract stays "there is always something to watch".
fn spawn_cancel_poll(id: String, canceled: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("cck-upload-cancel-poll".into())
        .spawn(move || {
            loop {
                if canceled.load(Ordering::Relaxed) {
                    // Already set by the tray's own Cancel item; nothing left to poll for.
                    return;
                }
                if session::cancel_requested(&id) {
                    log::debug!("cloud upload: a cancel request arrived for this session");
                    canceled.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(CANCEL_POLL_INTERVAL);
            }
        })
        // A `Builder::spawn` failure here means the OS could not start a thread at all, which
        // is a machine in far worse trouble than this upload; fall back to a handle for an
        // already-finished no-op thread so the caller's type stays simple rather than growing
        // an `Option` for a case that is not this feature's to recover from.
        .unwrap_or_else(|_| std::thread::spawn(|| {}))
}

/// Whether this upload should ask the provider for a share link. Pure; unit-tested.
///
/// Both terms are needed and neither is redundant: the user asked (`auto_share`), and the
/// provider can actually make one ([`ProviderCaps::share_links`]). Asking a provider that
/// cannot would only produce an error to swallow, and an unknown provider (an account
/// written by a newer build) claims nothing, so it is treated as "cannot".
pub fn share_link_wanted(auto_share: bool, caps: Option<ProviderCaps>) -> bool {
    auto_share && caps.is_some_and(|c| c.share_links)
}

/// Arm the hard backstop: after `budget`, this process ends no matter what it is doing.
///
/// The same shape the macOS notification helper uses for its own click-catching window
/// (`mac_un::CLICK_WINDOW`), and for the same reason: a detached helper with nobody
/// watching it must not be able to live forever. A DETACHED thread, so it never keeps the
/// process alive on the normal path (the process exits when `main` returns and takes this
/// thread with it). [`UPLOAD_BUDGET`] comfortably outlasts that window: it covers the
/// upload itself PLUS the notification's own wait for a click, which runs after the
/// transfer finishes, inside this same backstop.
fn arm_backstop(budget: Duration) {
    std::thread::Builder::new()
        .name("cck-upload-backstop".into())
        .spawn(move || {
            std::thread::sleep(budget);
            log::error!(
                "cloud upload: still running after {} minutes; ending the upload process",
                budget.as_secs() / 60
            );
            std::process::exit(1);
        })
        .ok();
}

#[cfg(test)]
mod decision_tests {
    use super::*;

    fn caps(share_links: bool) -> Option<ProviderCaps> {
        Some(ProviderCaps {
            folder_browse: false,
            share_links,
            link_expiry: false,
            delete_recoverable: false,
            max_file_bytes: None,
            // A generic file-storage drive (DRAGON-493): irrelevant to what this module's own
            // test decides (the share link), but a real provider's shape.
            accepts_images: true,
            accepts_videos: true,
            visibility: false,
        })
    }

    /// A link is made only when BOTH the user asked and the provider can. The unknown
    /// provider (an account from a newer build) is the case that must not guess.
    #[test]
    fn a_share_link_needs_the_ask_and_the_capability() {
        assert!(share_link_wanted(true, caps(true)));
        assert!(!share_link_wanted(false, caps(true)), "the user did not ask");
        assert!(!share_link_wanted(true, caps(false)), "the provider cannot");
        assert!(!share_link_wanted(true, None), "an unknown provider claims nothing");
        assert!(!share_link_wanted(false, None));
    }
}

#[cfg(test)]
mod banner_copy_tests {
    use super::*;

    /// The account-gone reason is user-facing copy: a sentence, about the account, naming
    /// nothing technical. It is asserted here rather than by running the upload, because
    /// running it would post a real desktop notification on the developer's machine.
    #[test]
    fn the_account_gone_reason_reads_as_a_sentence() {
        assert!(ACCOUNT_GONE.ends_with('.'), "a user-facing reason is a sentence");
        assert!(ACCOUNT_GONE.contains("account"), "it says what is wrong");
        assert!(
            !ACCOUNT_GONE.contains('\u{2014}') && !ACCOUNT_GONE.contains('\u{2013}'),
            "no em/en-dash in user-facing copy"
        );
        // It is the BODY of a failure banner whose title already names the account (see
        // `share::notify::upload_notification_text`), so it must not repeat a title's job.
        assert!(!ACCOUNT_GONE.starts_with("Upload"), "the title says that part");
    }

    /// The budget is a backstop, not a transfer timeout: it has to be long enough that a
    /// real upload over a slow link never trips it.
    #[test]
    fn the_backstop_is_generous_enough_to_be_a_backstop() {
        assert!(UPLOAD_BUDGET >= Duration::from_secs(10 * 60), "a real upload can be slow");
        assert!(UPLOAD_BUDGET <= Duration::from_secs(60 * 60), "but it cannot be forever");
    }

    /// **The prefix does not belong in a banner.** `Reconnect needed: ` labels a button the
    /// settings page has and a desktop banner does not, so it comes off, and what is left is
    /// still a whole sentence that says what to do.
    #[test]
    fn a_reconnect_failure_loses_its_prefix_before_the_banner() {
        let raw = super::super::oauth::reconnect_message(
            "the cloud service no longer accepts this account's saved sign-in. Connect it again.",
        );
        let banner = super::super::oauth::reconnect_reason(&raw);
        assert!(!banner.starts_with(super::super::oauth::RECONNECT_PREFIX), "{banner}");
        assert!(banner.starts_with("the cloud service"), "{banner}");
        assert!(banner.ends_with('.'), "a banner body is a sentence: {banner}");
        // An ordinary failure is untouched, so this is safe to apply to every reason.
        assert_eq!(super::super::oauth::reconnect_reason(ACCOUNT_GONE), ACCOUNT_GONE);
    }
}

#[cfg(test)]
mod cancel_tests {
    use super::*;

    /// **The DRAGON-495 bug, as a decision.** A cancel that arrives after the provider has
    /// committed the file cannot be honoured by stopping anything, so it has to be honoured by
    /// deleting what landed. This is NOT a rare race: every small file (a screenshot) goes up
    /// in ONE request with no cancel point in it at all, so this is the ordinary path for the
    /// case the user actually reported.
    #[rstest::rstest]
    #[case(true, true, UploadEnding::CanceledCommitted)]
    #[case(true, false, UploadEnding::CanceledInFlight)]
    #[case(false, true, UploadEnding::Deliver)]
    #[case(false, false, UploadEnding::Failed)]
    fn the_ending_pairs_the_cancel_with_what_actually_landed(
        #[case] canceled: bool,
        #[case] committed: bool,
        #[case] expected: UploadEnding,
    ) {
        assert_eq!(upload_ending(canceled, committed), expected);
    }

    /// Only a cancel cleans up, and WHAT it cleans up is decided by whether the provider
    /// committed: a remote delete for the file that landed, nothing for a session the provider
    /// expires by itself.
    #[test]
    fn only_a_cancel_cleans_up_and_only_a_committed_one_deletes() {
        assert_eq!(cancel_cleanup(UploadEnding::Deliver), None);
        assert_eq!(cancel_cleanup(UploadEnding::Failed), None, "a failure leaves nothing of ours");
        assert_eq!(
            cancel_cleanup(UploadEnding::CanceledInFlight),
            Some(CancelCleanup::SessionAbandoned)
        );
        assert_eq!(
            cancel_cleanup(UploadEnding::CanceledCommitted),
            Some(CancelCleanup::DeleteRemote)
        );
    }

    /// A cancel is not a failure, in the tray or anywhere else: the item goes without a mark.
    /// A real failure still has to read as one.
    #[test]
    fn the_tray_never_shows_a_cancel_as_a_failure() {
        use super::super::upload::tray::{Ending, Face, finish_face};
        assert_eq!(tray_ending(UploadEnding::Deliver), Ending::Done);
        assert_eq!(tray_ending(UploadEnding::Failed), Ending::Failed);
        assert_eq!(tray_ending(UploadEnding::CanceledInFlight), Ending::Canceled);
        assert_eq!(tray_ending(UploadEnding::CanceledCommitted), Ending::Canceled);
        // And a cancel holds NO face at all, so the item simply disappears.
        assert_eq!(finish_face(Ending::Canceled), None);
        assert_eq!(finish_face(Ending::Done), Some(Face::Done));
        assert_eq!(finish_face(Ending::Failed), Some(Face::Failed));
    }

    /// The remote delete is BOUNDED (CLAUDE.md: nothing in a path a user is waiting on waits
    /// unboundedly). One retry, no ladder: the file is the provider's now, and a cancel must
    /// not become a slow operation to buy a marginal chance against a provider already
    /// refusing.
    #[test]
    fn the_cancel_delete_is_bounded_to_one_retry() {
        assert_eq!(CANCEL_DELETE_ATTEMPTS, 2);
    }

}
