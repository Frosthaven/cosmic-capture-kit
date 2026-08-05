//! Per-provider API calls (DRAGON-482, stage A2).
//!
//! [`super::registry`] describes WHAT each provider can do. This is where the calls that do
//! it live: upload, share link, folder listing, delete, revoke.
//!
//! # The shape
//!
//! Five public functions, one per capability, each taking a [`CloudAccount`] and returning
//! `Result<T, String>`. That list is a PINNED contract: stage A3's upload engine codes
//! against these signatures, so changing one is an orchestrator decision. (One has been:
//! DRAGON-498 replaced `list_folders(acct, parent)` with `folder_setup(acct)`, which does the
//! same listing after making sure the account's app folder exists. The upload path's four are
//! untouched.)
//!
//! **DRAGON-506 changed that one again and added two more, all on the FOLDER side and none on
//! the upload path**, because the setup step became a folder MANAGER: `folder_setup(acct)` became
//! [`browse_folders`]`(acct, parent)`, which lists ONE level (the app folder, or anything inside
//! it, so the list can drill down), [`create_folder`] makes a subfolder at the level being
//! viewed, and [`delete_folder`] removes one. The upload path's four are still untouched.
//!
//! Behind them is exactly ONE dispatch point, [`ops`], which turns an account's provider id
//! into a [`ProviderOps`] implementation. Nothing else in the app ever learns a provider id,
//! which is what stops a new provider from touching the UI: a settings screen reads
//! [`super::ProviderCaps`], and a caller here passes an account.
//!
//! Each provider gets its own file, because they agree on almost nothing:
//!
//! | | Google Drive | OneDrive | Dropbox |
//! |---|---|---|---|
//! | Big uploads | resumable session | `createUploadSession` + ranged PUT | `upload_session/*` |
//! | Chunk rule | multiple of 256 KiB | multiple of **320 KiB** | multiple of 4 MiB |
//! | Folder id is | a Drive file id | a driveItem id | **a path** |
//! | The app folder is | a folder we find or create | `special/approot`, provisioned by Graph | the token's own root |
//! | Share link | permission, then read `webViewLink` | `createLink` | `create_shared_link_with_settings` |
//! | Token revocation | yes | **none exists** | yes |
//!
//! # Where the token comes from
//!
//! Never from a provider file. Each public function here calls
//! [`super::oauth::ensure_fresh`] ONCE and hands the access token down, so refresh-on-expiry,
//! single-flight and the reconnect signal are decided in one place and no provider can
//! forget them.
//!
//! # Bounds
//!
//! Every request carries a budget, because [`super::http::CurlReq`] will not construct
//! without one. Metadata calls get [`META_BUDGET`]; a body-carrying request gets
//! [`upload_budget`], which scales with the bytes actually being sent so a large upload is
//! not killed by a timeout written for a small one.
//!
//! # Privacy
//!
//! No token, share URL, remote file id or `Path` reaches a log. A log line here names the
//! provider, the operation and the HTTP status, which is enough to diagnose one and carries
//! none of the user's content. Two things are treated as CREDENTIALS rather than URLs and
//! never logged even in part: an upload-session URL (Microsoft's carries its own auth token)
//! and anything that came back from a token endpoint.

mod dropbox;
mod gdrive;
mod onedrive;
mod proton;
mod youtube;

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::accounts::CloudAccount;
use super::http::{CurlReq, CurlResponse, Method};

/// A file that exists in the provider after an upload.
pub struct RemoteFile {
    pub id: String,
    /// Where a browser can view the uploaded file, when the provider reports one.
    ///
    /// Not a share link: it opens only for the account that owns the file. That is what the
    /// upload banner's click falls back to when no share link was made, so a click lands on
    /// the capture in the user's own drive rather than on their file manager. Checked against
    /// the provider's own hosts before it is used (`cloud::web_url_allowed`).
    pub web_url: Option<String>,
}

/// One entry of a folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFolder {
    pub id: String,
    pub name: String,
}

/// Everything the setup step needs to offer a destination (DRAGON-498).
///
/// One type rather than two calls, because the two answers are only meaningful together: the
/// folders listed are the ones INSIDE [`Self::root`], so a caller that fetched them separately
/// could show a list from one place and save a destination in another.
///
/// Since DRAGON-506 it also carries ONE level of a deeper listing, where [`Self::root`] is `None`
/// because a subfolder listing has no root to report and the caller already knows the one it
/// started from. See [`browse_folders`].
#[derive(Debug, Clone, Default)]
pub struct FolderSetup {
    /// The account's own app folder, when this provider had to find or create one for it.
    ///
    /// `None` for a provider whose TOKEN is already confined to it (OneDrive's `approot`,
    /// Dropbox's app-folder access type): there the root needs no id, because "no folder chosen"
    /// already means the app folder and cannot mean anything else. `Some` for Google Drive,
    /// which has no such mechanism and gets an ordinary folder made for it, so the id has to be
    /// remembered or an upload would fall back to the whole of My Drive.
    ///
    /// Always `None` for a listing of a SUBFOLDER (DRAGON-506): the root is found once, at the
    /// top of the tree, and re-finding it on every descent would be a wasted round trip per
    /// level.
    pub root: Option<RemoteFolder>,
    /// The folders at this level, sorted for display.
    pub folders: Vec<RemoteFolder>,
}

/// Upload `file` into the account's configured folder. `on_progress` receives
/// 0-100 monotonically; the caller owns presenting it.
///
/// `should_cancel` is checked between chunks (DRAGON-490): a chunked/session upload's per-chunk
/// loop is the natural point, since it already pauses there between one provider request and
/// the next. A single-shot (small file, one request) upload has no such point and is not
/// interrupted mid-flight; that request's own timeout is what bounds it either way. When it
/// answers `true` the upload stops and returns [`is_canceled`]'s `Err`, which `child.rs`
/// recognises before treating the `Err` as an ordinary failure.
///
/// # A dead end: predicting "will this report progress" from the file's size (DRAGON-490
/// follow-up)
///
/// This module used to also carry a per-provider `reports_progress(size)` predicate, so a
/// caller could decide UPFRONT whether to show a real percentage or an indeterminate spinner.
/// It correctly diagnosed the owner's "the bar never moves" report (each provider's own chunk
/// size and its single-request threshold are different values, so a file just over the
/// threshold could still complete in one request and report nothing but 0 and 100), but the
/// owner then asked for the decision to be DYNAMIC instead: start every upload as the spinner
/// and switch to a real percentage the moment one genuinely arrives, rather than predicting it
/// from a byte count. Dynamic detection needs no per-provider threshold at all (it just watches
/// what actually happens, so it is automatically correct for the YouTube provider landing
/// alongside this and any future one), so the size-based predicate was deleted rather than kept
/// as a second, possibly-disagreeing source of truth. See `cloud::child::run_cloud_upload` and
/// `cloud::tray::UploadTray::set_percent` for where the live decision is made now.
pub fn upload(
    acct: &CloudAccount,
    file: &Path,
    on_progress: &mut dyn FnMut(u8),
    should_cancel: &dyn Fn() -> bool,
) -> Result<RemoteFile, String> {
    let ops = ops(acct)?;
    let token = access_token(acct)?;
    on_progress(0);
    ops.upload(acct, &token, file, on_progress, should_cancel)
}

/// Marks an upload's `Err` as a CANCELLATION rather than an ordinary provider failure
/// (DRAGON-490), recognised by [`is_canceled`]. The same shape `oauth::RECONNECT_PREFIX` uses:
/// a fixed prefix on the `Result<T, String>` every fallible call in this app already returns
/// (CLAUDE.md's convention), rather than a second error type. `child::run_cloud_upload`
/// intercepts it before it could ever reach a banner or a log meant for the user; it is not
/// user-facing copy and never needs to be, since the child reports a cancellation through its
/// own wording (`session::UploadState::Canceled`) instead.
const CANCELED_PREFIX: &str = "cck-upload-canceled: ";

/// Whether an upload's `Err` reason is a cancellation, not an ordinary failure. Pure;
/// unit-tested.
pub fn is_canceled(reason: &str) -> bool {
    reason.starts_with(CANCELED_PREFIX)
}

/// The `Err` a provider's chunk loop returns the moment `should_cancel` answers true.
fn canceled_err() -> String {
    format!("{CANCELED_PREFIX}the upload was canceled")
}

/// Create (or fetch the existing) shared link for an uploaded file.
pub fn create_share_link(acct: &CloudAccount, file_id: &str) -> Result<String, String> {
    let ops = ops(acct)?;
    let token = access_token(acct)?;
    ops.create_share_link(&token, file_id)
}

/// ONE level of the account's folder tree (DRAGON-498, extended by DRAGON-506).
///
/// `parent` is the folder to look inside, as that provider addresses one: a Drive file id, a
/// Graph driveItem id, a Dropbox path.
///
/// **`None` is the account's app folder, and it is the only case that provisions anything.**
/// Two steps there, in this order and never the other way round: make sure the account HAS its
/// app folder ([`ProviderOps::ensure_root_folder`], which for two of the three providers is
/// nothing at all), then list what is in it. The caller writes [`FolderSetup::root`] onto the
/// account when the account does not already name a destination, which is what makes "uploads
/// land in the Cosmic Capture Kit folder" true for Drive as well as for the two providers whose
/// token enforces it. A descent already knows which folder it is descending into, so it skips
/// that step rather than paying to re-find the root per level.
///
/// This replaced a bare `list_folders(acct, parent)` in the pinned five-function contract, as
/// `folder_setup(acct)`: every caller of the bare one wanted the pair, and the two could
/// otherwise be called in the wrong order or against different roots. DRAGON-506 gave the level
/// back as an argument, because the setup step's list drills down now, and dropped the
/// no-argument wrapper once every caller had a level to name.
///
/// Every level goes through here, so the drill-down and the first listing cannot sort
/// differently or adopt a different root. A level that no longer exists (the user deleted it in
/// their browser between two listings) comes back as [`is_missing_folder`]'s `Err`, which the
/// setup step answers by walking its breadcrumb up one level rather than failing the dialog.
pub fn browse_folders(acct: &CloudAccount, parent: Option<&str>) -> Result<FolderSetup, String> {
    let ops = ops(acct)?;
    let token = access_token(acct)?;
    let root = match parent {
        None => ops.ensure_root_folder(&token)?,
        Some(_) => None,
    };
    let level = match parent {
        Some(id) => Some(id),
        None => root.as_ref().map(|r| r.id.as_str()),
    };
    let mut folders = ops.list_folders(&token, level)?;
    // Sorted HERE, once, rather than trusting three providers' sort options: Dropbox has
    // none, and Microsoft's `$orderby` is documented differently for personal and business
    // accounts. Case-insensitive, because a picker sorted by ASCII puts every lowercase name
    // after every uppercase one, which reads as unsorted.
    folders.sort_by(|a, b| {
        a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name))
    });
    Ok(FolderSetup { root, folders })
}

/// The folder browser's OPENING view, walked back down to an account's saved destination
/// (DRAGON-517).
///
/// What [`restore_browse`] answers. Every field is what the settings page's browser holds while it
/// is open: the app folder, the levels below it the browser sits inside, and what is in the
/// deepest of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoredBrowse {
    /// The account's app folder, when this provider had to find or create one (Google Drive).
    /// `None` where the token root already IS that folder, exactly as [`FolderSetup::root`] is.
    pub root: Option<RemoteFolder>,
    /// The levels below the app folder the browser opens INSIDE, outermost first. Empty means it
    /// opens on the app folder itself, which is where a brand new account starts and where a walk
    /// that could resolve nothing ends up.
    pub trail: Vec<RemoteFolder>,
    /// The folders at the deepest level reached, sorted for display by [`browse_folders`].
    pub folders: Vec<RemoteFolder>,
    /// Why the walk stopped short of the saved destination, when a provider REFUSED a level. The
    /// raw reason; the caller classifies it like any other listing failure. `None` when the walk
    /// arrived, and `None` when a level was simply not there any more (see [`restore_browse_with`]
    /// for why those two answer differently).
    pub note: Option<String>,
}

/// Re-open an account's folder browser INSIDE the destination it saved (DRAGON-517).
///
/// **Why a walk.** An account remembers its destination as a PATH of display names below the app
/// folder ("Screenshots/2026"), plus the destination folder's own provider-side id. The browser's
/// breadcrumb needs an id for EVERY level, because that is what a press on a crumb lists next, and
/// only the last one is on disk. So the levels are resolved the only way they can be: list a
/// level, find the segment in it, descend.
///
/// `segments` is the stored path already split (`cloud::folder_path_segments`, the one splitter
/// the account's own row reads too), and `destination` is the account's `folder_id`.
///
/// One provider round trip per level, in ONE call, so the page spins its refresh glyph once and
/// lands once however deep the saved folder is. A brand new account has no segments at all, so its
/// walk is a single listing of the app folder, which is exactly what opening the step used to do.
pub fn restore_browse(
    acct: &CloudAccount,
    segments: &[&str],
    destination: Option<&str>,
) -> Result<RestoredBrowse, String> {
    restore_browse_with(segments, destination, |parent| browse_folders(acct, parent))
}

/// [`restore_browse`] with its level lister injected. Pure given `list`; unit-tested.
///
/// The repo's `foo_with` seam: the walk is all of the interesting behaviour and none of the
/// network, so it is tested here against a fake drive rather than left to a live provider.
///
/// **Three endings, and they are deliberately different:**
///
/// * It arrives. The browser opens inside the saved folder, which is the whole point.
/// * A segment is NOT THERE. The folder was deleted elsewhere since it was chosen, which is not an
///   error: the walk stops at the deepest level that does exist, and the breadcrumb says where
///   that is. This is the same answer the page's `listing_failure` already gives a level that
///   vanishes while the step is open, applied before the fact.
/// * A provider REFUSES a level. Everything resolved so far is discarded and the browser opens at
///   the app folder, carrying the reason as [`RestoredBrowse::note`]. A refusal says the account or
///   the connection is in trouble, and the app folder is the one level that always exists; a
///   half-walked trail built on top of a failure would be a guess presented as a place.
///
/// The FIRST listing failing is the one case that answers `Err`, because there is then no level at
/// all to show.
///
/// `destination` is offered to the LAST segment only, and it wins over the name there: it is the
/// one level whose id is actually known, and two sibling folders CAN share a name (Google Drive
/// allows it; this app's own create refuses it, but a user arranging their drive elsewhere is not
/// bound by that). Names are matched case-INSENSITIVELY, for the reason every other comparison in
/// this module is: Dropbox stores a lowercased path and OneDrive treats names that way.
pub fn restore_browse_with(
    segments: &[&str],
    destination: Option<&str>,
    mut list: impl FnMut(Option<&str>) -> Result<FolderSetup, String>,
) -> Result<RestoredBrowse, String> {
    let first = list(None)?;
    let root = first.root;
    let at_root = first.folders;
    let mut trail: Vec<RemoteFolder> = Vec::new();
    let mut folders = at_root.clone();
    for (index, name) in segments.iter().enumerate() {
        // The stored id names the DESTINATION, so it can only ever identify the last segment.
        let want = if index + 1 == segments.len() { destination } else { None };
        let Some(found) = restore_step(&folders, name, want) else { break };
        let step = folders[found].clone();
        match list(Some(&step.id)) {
            Ok(next) => {
                trail.push(step);
                folders = next.folders;
            }
            Err(reason) => {
                return Ok(RestoredBrowse {
                    root,
                    trail: Vec::new(),
                    folders: at_root,
                    note: Some(reason),
                });
            }
        }
    }
    Ok(RestoredBrowse { root, trail, folders, note: None })
}

/// Which listed folder a stored path segment names, or `None` when it is not there any more.
/// Pure; unit-tested.
///
/// See [`restore_browse_with`] for why `want_id` is offered for one segment only and why it beats
/// the name.
fn restore_step(listed: &[RemoteFolder], name: &str, want_id: Option<&str>) -> Option<usize> {
    if let Some(want) = want_id.map(str::trim).filter(|id| !id.is_empty())
        && let Some(index) = listed.iter().position(|folder| folder.id == want)
    {
        return Some(index);
    }
    let name = name.trim();
    listed.iter().position(|folder| folder.name.trim().eq_ignore_ascii_case(name))
}

/// Make a folder called `name` inside `parent` (DRAGON-506).
///
/// `parent` is addressed exactly as [`browse_folders`] addresses it, `None` meaning the account's
/// app folder, so the level a user is looking at is the level the folder appears in. **Resolving
/// that `None` is the provider's job and it is not optional**; see [`create_parent_with`], which
/// is where DRAGON-520 put the one decision two of the three providers were getting wrong.
///
/// The name is validated through [`remote_folder_name`] BEFORE any request, because every
/// provider treats it as a path component and a name carrying a separator would silently create
/// something other than what was typed. A provider that already has a folder of that name is
/// asked to REFUSE rather than to make a second one or to rename ours: the caller is a picker,
/// and two folders with one name in it is worse than a message saying the name is taken.
pub fn create_folder(
    acct: &CloudAccount,
    parent: Option<&str>,
    name: &str,
) -> Result<RemoteFolder, String> {
    let ops = ops(acct)?;
    let name = remote_folder_name(name)?;
    let token = access_token(acct)?;
    let folder = ops.create_folder(&token, parent, &name)?;
    log::debug!("cloud {}: created a folder", acct.provider);
    Ok(folder)
}

/// The folder a create must be ADDRESSED to, with the app folder resolved when the browser is
/// standing at the top level (DRAGON-520). Pure given `resolve_root`; unit-tested.
///
/// # The bug this exists to make impossible
///
/// [`browse_folders`] reads a `None` parent as "the account's app folder" and RESOLVES it before
/// it lists anything ([`ProviderOps::ensure_root_folder`] for Drive, the `approot` alias for
/// Graph, the token's own root for Dropbox). A create was left to interpret the same `None` on its
/// own, and two of the three providers interpreted it differently from the listing beside them:
///
/// * Google Drive omitted `parents` entirely, which Drive documents as "the file is placed
///   directly in the user's My Drive folder". So the folder was really made, with a 200 and an id,
///   at the TOP of My Drive: outside the app folder the listing then re-read, and outside the
///   sandbox this app promises to stay inside. Nothing failed, so nothing was reported, and the
///   user saw a folder that had not appeared. That is DRAGON-520's silent Drive symptom, and it
///   was a wrong-place SUCCESS rather than a swallowed error.
/// * OneDrive posted at `/me/drive/special/approot/children`, the collection its LISTING reads.
///   Graph's own createFolder reference documents only `/me/drive/items/{parent-item-id}/children`
///   and `/me/drive/root/children` as the request forms, and the app folder's real item id was
///   never fetched. The request came back refused, which is the error string the owner saw.
///
/// Both are now the same decision, made here once: a create ALWAYS addresses a real folder. A
/// provider injects the round trip that answers "which one" and never sees the choice.
///
/// `missing` is what to tell the user when a provider cannot name its own app folder. Per
/// provider, because it is user copy and the three app folders are different things. It is a
/// hard `Err` and not a fallback to the drive's root, which is exactly the guess that made the
/// Drive symptom silent.
fn create_parent_with(
    parent: Option<&str>,
    resolve_root: impl FnOnce() -> Result<Option<String>, String>,
    missing: &str,
) -> Result<String, String> {
    if let Some(id) = parent.map(str::trim).filter(|id| !id.is_empty()) {
        return Ok(id.to_string());
    }
    resolve_root()?
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| missing.to_string())
}

/// Delete a folder, and with it everything inside it (DRAGON-506).
///
/// `folder` is a folder's own id or path, as [`RemoteFolder::id`] carries it. There is no
/// recursive walk and no per-file delete: all three providers delete a folder's contents with
/// it, which is exactly why the caller has to have asked first. Whether the user can put it back
/// afterwards is `ProviderCaps::delete_recoverable`, and it is the confirmation's copy that has
/// to say so.
///
/// The app folder itself is never a legal argument here. That is refused in the UI, before this
/// is reached, because "delete the folder this app uploads to" is a decision no confirmation
/// should be able to talk a user into.
pub fn delete_folder(acct: &CloudAccount, folder: &str) -> Result<(), String> {
    let ops = ops(acct)?;
    let token = access_token(acct)?;
    ops.delete_folder(&token, folder)?;
    log::debug!("cloud {}: deleted a folder", acct.provider);
    Ok(())
}

/// Delete a previously uploaded file.
///
/// Two callers, and both are the user taking an upload back in the moment rather than a
/// deadline coming due: the cancel that arrives after the provider has already committed
/// (`child::delete_remote_best_effort`, DRAGON-496) and the undo offered during the upload
/// meter's finish-hold (`app::preview::share::undo_upload`, DRAGON-507). Neither consults a
/// capability flag; every connectable provider implements this.
pub fn delete_file(acct: &CloudAccount, file_id: &str) -> Result<(), String> {
    let ops = ops(acct)?;
    let token = access_token(acct)?;
    ops.delete_file(&token, file_id)
}

/// Every photo ALBUM this account has, with the app's own album found or created first
/// (DRAGON-485).
///
/// The album axis's [`browse_folders`], and it is deliberately the same SHAPE: one call that
/// provisions and then lists, so the Photos tab cannot show a list from one place and save a
/// destination in another, and so an account that has never chosen an album still has one to
/// fall back to. [`FolderSetup::root`] carries that default album, exactly as it carries the app
/// folder on the files side; there is no level below it, so there is no `parent` argument and
/// no descent.
pub fn browse_albums(acct: &CloudAccount) -> Result<FolderSetup, String> {
    let ops = ops(acct)?;
    let token = access_token(acct)?;
    // **ONE listing, and what to do about the default is decided from it.** The obvious shape
    // (ask the provider to ensure the album, then list) costs a SECOND listing, because finding
    // an album by name is itself a listing; and this provider's tool is a ~118 MB binary whose
    // start-up dominates a small call, so a redundant one is a visible pause on a dialog someone
    // is looking at. The decision is [`app_album`], in the shared tree and unit-tested; the only
    // thing left for the provider is the create.
    let mut albums = ops.list_albums(&token)?;
    let found = app_album(&albums, super::APP_FOLDER).map(|index| albums[index].clone());
    let root = match found {
        Some(album) => Some(album),
        // **Listing does not provision** (DRAGON-522). This used to create the app's own album
        // whenever it was absent, which meant opening the Photos tab, refreshing it, or editing
        // an account whose album is something else all made one, on a tab the user may have been
        // only looking at. Browsing writes nothing, the same rule DRAGON-517 settled for the
        // folder side. The one case that still provisions HERE is an account that already
        // uploads to albums and names none, because that account's destination genuinely
        // resolves to the default and its next capture needs it to exist.
        None if needs_default_album(crate::cloud::uploads_to_album(acct), acct.folder_id.as_deref()) => {
            let made = ops.create_album(&token, super::APP_FOLDER)?;
            // Pushed onto the list rather than left for the next refresh: the album the caller
            // is about to select has to be IN the set it selects from, or the reconciliation
            // that checks a chosen album still exists would drop it on the spot.
            albums.push(made.clone());
            Some(made)
        }
        // No default, and no reason to make one. The picker shows what is there, and its empty
        // state already says to add one with `+`.
        None => None,
    };
    log::debug!("cloud {}: listed {} albums", acct.provider, albums.len());
    Ok(FolderSetup { root, folders: albums })
}

/// Whether LISTING an account's albums should also provision the app's own. Pure; unit-tested
/// (DRAGON-522).
///
/// The lazy rule, in one predicate: provision only when the account's destination genuinely
/// RESOLVES to the default album. That is true for an account already set to Photos with no album
/// recorded, whose uploads fall back to the app's own album by name
/// (`providers::proton::account_album`), and false for everything else, including every account
/// that is merely being browsed.
///
/// `photos` is whether the stored destination is the album axis, `saved` the album id the account
/// carries. A blank id is no id: a hand-edited accounts file, or a destination cleared by a
/// migration, both mean "wherever the default is".
///
/// **Not a question about the DIALOG.** The tab a user happens to be looking at is not a
/// destination until Done writes it, which is why the tab is not an argument here: confirming
/// Photos with nothing chosen provisions through its own path
/// (`CloudSettingsMsg::DefaultAlbumMade`), where a user has actually asked for it.
pub fn needs_default_album(photos: bool, saved: Option<&str>) -> bool {
    photos && saved.is_none_or(|id| id.trim().is_empty())
}

/// Which listed album is the app's own, by name. Pure; unit-tested.
///
/// The album side's find-half of find-or-create. Matched by NAME, and only here: everything
/// downstream addresses an album by its id, but a name is all this app knows about an album it
/// may have created on a previous run, and there is nowhere else to look it up.
///
/// Case-INSENSITIVE and trimmed, the same rule [`restore_step`] matches folder names by, so a
/// user who renamed the album's case in Proton's own app does not get a second one made beside
/// it on the next open.
fn app_album(albums: &[RemoteFolder], name: &str) -> Option<usize> {
    let want = name.trim();
    albums.iter().position(|album| album.name.trim().eq_ignore_ascii_case(want))
}

/// Make a new album called `name` (DRAGON-485).
///
/// [`create_folder`]'s sibling, and it applies the SAME name rules
/// ([`remote_folder_name`]) before any request: a picker that accepts a name for an album and
/// refuses it for a folder is a picker with two rule sets nobody can learn. There is no parent
/// to resolve, which is the one thing [`create_folder`] does that this does not have to.
pub fn create_album(acct: &CloudAccount, name: &str) -> Result<RemoteFolder, String> {
    let ops = ops(acct)?;
    let name = remote_folder_name(name)?;
    let token = access_token(acct)?;
    let album = ops.create_album(&token, &name)?;
    log::debug!("cloud {}: created an album", acct.provider);
    Ok(album)
}

/// Delete an album (DRAGON-485).
///
/// The album this account uploads to is refused in the UI, before this is reached, for the same
/// reason [`delete_folder`]'s app folder is: "delete the place this app uploads to" is a
/// decision no confirmation should be able to talk a user into.
pub fn delete_album(acct: &CloudAccount, album: &str) -> Result<(), String> {
    let ops = ops(acct)?;
    let token = access_token(acct)?;
    ops.delete_album(&token, album)?;
    log::debug!("cloud {}: deleted an album", acct.provider);
    Ok(())
}

/// Revoke the account's tokens at the provider, best effort, before local deletion.
///
/// A token that cannot be renewed is treated as ALREADY revoked, and succeeds. That is the
/// honest reading: the provider has forgotten this authorization, which is exactly what this
/// call was going to ask for. Failing here instead would block a disconnect on a dead
/// account, which is the one account a user most wants to remove.
pub fn revoke(acct: &CloudAccount) -> Result<(), String> {
    let ops = ops(acct)?;
    // **`access_token`, not `ensure_fresh`** (DRAGON-485), which is the same swap the other five
    // entry points made and matters MOST here. An external-tool account has no stored token, so
    // `ensure_fresh` fails with a reconnect-shaped message, which the arm below reads as "already
    // revoked" and returns success from without ever calling the provider. The tool would then
    // stay signed in after a disconnect, and the next account connected would silently inherit
    // that session, which is exactly the eviction the one-account cap exists to prevent.
    match access_token(acct) {
        Ok(token) => ops.revoke(&token),
        Err(e) if super::oauth::needs_reconnect(&e) => {
            log::debug!("cloud {}: nothing to revoke, the authorization is already gone", acct.provider);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// What one provider can do, with the token already resolved.
///
/// Not public: the five functions above are the contract, and this exists only so [`ops`] can
/// be the single dispatch. Every method takes the access token as an argument, so no provider
/// file ever touches [`super::oauth`] or [`super::secrets`].
trait ProviderOps {
    fn upload(
        &self,
        acct: &CloudAccount,
        token: &str,
        file: &Path,
        on_progress: &mut dyn FnMut(u8),
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<RemoteFile, String>;

    fn create_share_link(&self, token: &str, file_id: &str) -> Result<String, String>;

    /// Every folder directly inside `parent` (`None` = the account's app folder).
    ///
    /// **Paged to the end, up to [`MAX_LIST_PAGES`]** (DRAGON-506). Each provider follows its
    /// own cursor (Drive's `nextPageToken`, Graph's `@odata.nextLink`, Dropbox's
    /// `list_folder/continue`), because a first page is not a folder listing: a user whose
    /// destination is the 130th folder could not see it at all.
    fn list_folders(&self, token: &str, parent: Option<&str>) -> Result<Vec<RemoteFolder>, String>;

    /// Make a folder called `name` inside `parent` (`None` = the account's app folder).
    ///
    /// `name` has already been through [`remote_folder_name`]. A provider that already has a
    /// folder of that name must REFUSE rather than rename or duplicate; see [`create_folder`].
    fn create_folder(
        &self,
        token: &str,
        parent: Option<&str>,
        name: &str,
    ) -> Result<RemoteFolder, String>;

    /// Delete a folder and everything in it.
    ///
    /// Separate from [`Self::delete_file`] although two of the three providers issue the same
    /// request for both: the operation NAME rides into [`api_failure`]'s message, and a user
    /// told "the cloud service could not delete that file" after pressing Delete on a folder
    /// would reasonably think something else had happened.
    fn delete_folder(&self, token: &str, folder_id: &str) -> Result<(), String>;

    /// Make sure this account has the one folder this app writes to, and say where it is
    /// (DRAGON-498).
    ///
    /// The default is `Ok(None)`, which is the RIGHT answer for a provider whose token is
    /// already confined to that folder: OneDrive addresses it as `special/approot` with no id
    /// to fetch, and a Dropbox app-folder token's own root IS it. Only a provider that has to
    /// make an ordinary folder and remember its id (Google Drive) overrides this.
    ///
    /// Called before every folder listing, so it must be cheap when there is nothing to do and
    /// must never create a second folder when one already exists.
    fn ensure_root_folder(&self, _token: &str) -> Result<Option<RemoteFolder>, String> {
        Ok(None)
    }

    fn delete_file(&self, token: &str, file_id: &str) -> Result<(), String>;

    fn revoke(&self, token: &str) -> Result<(), String>;

    /// Every photo ALBUM this account has (DRAGON-485).
    ///
    /// The album axis's answer to [`Self::list_folders`], and it takes no parent, because that
    /// is the whole difference between the two: albums do not nest, so there is no level to
    /// list the inside of. See `cloud::proton::ALBUMS` for the model and for the instruction
    /// not to fold the two together.
    ///
    /// The default `Err` is the right answer for every provider whose only destination is a
    /// folder, and it is unreachable from the UI: [`super::has_albums`] is what decides whether
    /// the Photos tab exists at all, and it reads the same registry row this refusal describes.
    fn list_albums(&self, _token: &str) -> Result<Vec<RemoteFolder>, String> {
        Err(NO_ALBUMS.to_string())
    }

    /// Make a new album called `name`. `name` has already been through
    /// [`remote_folder_name`].
    ///
    /// There is deliberately no `ensure_album` beside this, unlike the folder side's
    /// [`Self::ensure_root_folder`]: finding an album by name is just a listing, so the
    /// find-or-create is decided in [`browse_albums`] from the listing it was going to make
    /// anyway, and a provider that implemented its own would be paying for a second one.
    fn create_album(&self, _token: &str, _name: &str) -> Result<RemoteFolder, String> {
        Err(NO_ALBUMS.to_string())
    }

    /// Delete an album, by its [`RemoteFolder::id`].
    ///
    /// Separate from [`Self::delete_folder`] for the same reason that one is separate from
    /// [`Self::delete_file`]: the operation's NAME reaches the user in a failure message, and
    /// the two are not the same act. What happens to the PHOTOS in a deleted album is the
    /// provider's decision and belongs in the confirmation's copy; see
    /// `pages::cloud::delete_confirm_body`.
    fn delete_album(&self, _token: &str, _album_id: &str) -> Result<(), String> {
        Err(NO_ALBUMS.to_string())
    }
}

/// What a provider with no album axis says when asked about one (DRAGON-485).
///
/// One sentence rather than four, because it describes a state no screen can reach: the Photos
/// tab is drawn only where [`super::has_albums`] is true. It exists so the trait's defaults are
/// honest rather than silently answering "no albums" with an empty list, which would draw an
/// empty picker on a provider that has no albums to draw.
const NO_ALBUMS: &str = "This cloud service does not keep photo albums.";

/// The access token to hand a provider's ops, or an empty one for a provider that has none.
///
/// **Why this is not just [`super::oauth::ensure_fresh`] any more** (DRAGON-485). Four of the
/// five providers keep OAuth tokens in [`super::secrets`], and refreshing them in ONE place is
/// what stops any provider forgetting to. The fifth, Proton, keeps no token at all: its session
/// lives inside Proton's own CLI, in the OS secret store under the tool's identity, and this app
/// never sees or holds it. Asking `ensure_fresh` for a token that was never stored would fail
/// every Proton call before it started.
///
/// So the token is fetched exactly when there is one to fetch, decided from the registry rather
/// than from a provider id, and an external-tool provider's ops receive an empty string they
/// are documented to ignore.
fn access_token(acct: &CloudAccount) -> Result<String, String> {
    let external = acct.spec().is_some_and(|spec| spec.auth.is_external_tool());
    if external {
        return Ok(String::new());
    }
    super::oauth::ensure_fresh(&acct.id)
}

/// **THE dispatch point.** The only place in the app that matches on a provider id.
fn ops(acct: &CloudAccount) -> Result<&'static dyn ProviderOps, String> {
    match acct.provider.as_str() {
        "gdrive" => Ok(&gdrive::GDrive),
        "onedrive" => Ok(&onedrive::OneDrive),
        "dropbox" => Ok(&dropbox::Dropbox),
        "youtube" => Ok(&youtube::YouTube),
        "proton" => Ok(&proton::Proton),
        // Either an account written by a newer build, or one of the two providers with no
        // public API. The message distinguishes them, because "we cannot" and "nobody can"
        // are different answers.
        other => Err(match super::provider(other) {
            Some(spec) => format!(
                "{} has no public API for other apps to upload through.",
                spec.display_name
            ),
            None => "This build does not know that cloud service.".to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Shared plumbing.
// ---------------------------------------------------------------------------

/// The budget for a metadata call: a listing, a share link, a delete, a session start.
pub const META_BUDGET: Duration = Duration::from_secs(30);

/// How many pages of ONE folder listing this app will follow (DRAGON-506).
///
/// **Why there is a cap at all.** All three providers page a listing with a cursor, and all
/// three decide when the cursor stops: a provider that keeps handing one back (a bug, a shared
/// folder that keeps changing under a Dropbox cursor, a hostile answer) would otherwise be an
/// unbounded loop, which is the one thing CLAUDE.md does not allow a wait in this app to be.
///
/// **Why TEN.** It has to be large enough that the cap is never the reason a user cannot see
/// their folder, and small enough that the worst case is a bounded wait rather than an
/// afternoon. The page sizes here are 100 (Drive), 200 (Graph) and 1000 (Dropbox), so ten pages
/// is at least a thousand folders at ONE level of ONE app folder; a destination picker that
/// cannot show the 1001st subfolder is not the problem anybody has. The other half of the
/// accounting is time: each page is one request bounded by [`META_BUDGET`], so the whole listing
/// cannot outlive `MAX_LIST_PAGES * META_BUDGET`, and the assert below is what keeps that
/// product inside five minutes if either number is ever changed. The work is detached and its
/// reply is generation-stamped, so a user who gives up simply closes the dialog.
pub const MAX_LIST_PAGES: u32 = 10;

const _: () = assert!(
    MAX_LIST_PAGES as u64 * META_BUDGET.as_secs() <= 300,
    "DRAGON-506: following a folder listing's pages must stay inside a five-minute worst case, \
     or a provider that keeps handing back a cursor becomes an unbounded wait"
);
const _: () = assert!(
    MAX_LIST_PAGES >= 2,
    "DRAGON-506: a listing that cannot follow even one cursor is not paginated at all"
);

/// Whether a listing may ask for another page, having already taken `pages_done` of them.
/// Pure; unit-tested.
///
/// The whole of the cap's enforcement, in one place, so three provider loops cannot each get the
/// off-by-one differently. `pages_done` counts pages ALREADY fetched, so the first page is
/// always allowed and the cap counts requests rather than cursors.
pub fn may_fetch_another_page(pages_done: u32) -> bool {
    pages_done < MAX_LIST_PAGES
}

/// Marks a folder listing's `Err` as THE FOLDER IS NOT THERE, rather than an ordinary failure
/// (DRAGON-506).
///
/// The same shape `oauth::RECONNECT_PREFIX` and [`CANCELED_PREFIX`] use: a fixed prefix on the
/// `Result<T, String>` every fallible call in this app already returns, rather than a second
/// error type for one case. It exists because the setup step has to tell "this folder is gone"
/// (walk the breadcrumb up one level and carry on) from "the listing failed" (say so and leave
/// the step where it is), and the only other way to know is to match on user-facing copy, which
/// changes.
///
/// Produced ONLY for a subfolder listing, never for the app folder itself: the root always
/// exists (a provider that has forgotten it will make it again), and a 404 there means something
/// else. Stripped before display by [`missing_folder_reason`].
const MISSING_PREFIX: &str = "cck-folder-missing: ";

/// Whether a listing's `Err` reason says the folder itself is gone. Pure; unit-tested.
pub fn is_missing_folder(reason: &str) -> bool {
    reason.starts_with(MISSING_PREFIX)
}

/// The `Err` a provider returns when the folder it was asked to list is not there.
///
/// Public so the SETTINGS page's own tests can build a marked reason to feed
/// `pages::cloud::listing_failure`, rather than typing the marker out a second time where it
/// could drift from this one.
pub fn missing_folder_err() -> String {
    format!("{MISSING_PREFIX}that folder is no longer there")
}

/// The reason without its marker, for anything that has to SHOW one. Pure; unit-tested.
///
/// The marker is machinery, not copy, so nothing user-facing may carry it. A reason that was
/// never marked comes back unchanged, so a caller can pass any message through this.
pub fn missing_folder_reason(reason: &str) -> &str {
    reason.strip_prefix(MISSING_PREFIX).unwrap_or(reason)
}

/// The longest a new folder's name may be, in characters.
///
/// The tightest of the three providers' documented limits: Dropbox rejects a path component over
/// 255 characters, Graph documents 400 for a whole path, and Drive states no folder-name limit at
/// all. One number rather than three, because a picker that accepts a name on one provider and
/// refuses it on another is a picker whose rules nobody can learn. Characters and not bytes,
/// since that is how the limits are written and how a user counts them.
pub const FOLDER_NAME_MAX: usize = 255;

/// Every https host the provider APIs reach, on top of the OAuth hosts.
///
/// [`super::registry_hosts`] covers the endpoints in the provider table, which are only the
/// OAuth ones. The API hosts are declared as constants in the provider files and folded in
/// here, so the allowlist stays a compile-time list of hosts this app knows about, exactly the
/// property the registry-derived version has. Unit-tested.
pub fn api_hosts() -> Vec<&'static str> {
    let mut hosts = super::registry_hosts();
    for extra in [gdrive::HOSTS, onedrive::HOSTS, dropbox::HOSTS, youtube::HOSTS].concat() {
        if !hosts.contains(&extra) {
            hosts.push(extra);
        }
    }
    hosts
}

/// Start a request to a provider API, allowlisted and budgeted.
fn api(method: Method, url: &str, budget: Duration) -> Result<CurlReq, String> {
    CurlReq::new(method, url, &api_hosts(), budget)
}

/// Add the bearer token. One helper so no provider hand-writes the header.
fn bearer(req: CurlReq, token: &str) -> CurlReq {
    req.header("Authorization", &format!("Bearer {token}"))
}

/// How long a request carrying `bytes` of body may take. Pure; unit-tested.
///
/// A fixed timeout cannot serve both a 40 KB screenshot and a 200 MB screen recording: short
/// enough to fail fast on the first is far too short for the second. So the budget is a floor
/// plus an allowance computed from a deliberately PESSIMISTIC throughput, low enough that a
/// slow hotel connection still finishes, and a ceiling so a stalled transfer cannot hang a
/// session forever.
pub fn upload_budget(bytes: u64) -> Duration {
    /// Enough for the handshake and a small body even on a bad link.
    const FLOOR_SECS: u64 = 60;
    /// Half an hour. Past this, something is wrong rather than slow.
    const CEILING_SECS: u64 = 1800;
    /// 20 KiB/s. Roughly a bad mobile connection, not a good one.
    const ASSUMED_BYTES_PER_SEC: u64 = 20 * 1024;
    let secs = FLOOR_SECS.saturating_add(bytes / ASSUMED_BYTES_PER_SEC);
    Duration::from_secs(secs.clamp(FLOOR_SECS, CEILING_SECS))
}

/// Upload progress as a percentage of BYTES sent. Pure; unit-tested.
///
/// Monotone whenever `sent` is, which is the property the UI depends on: a progress bar that
/// goes backwards reads as a bug even when the upload is fine. Computed in `u128` so a large
/// file cannot overflow the multiply, and clamped so a provider that acknowledges more bytes
/// than we sent cannot produce 101.
pub fn progress_percent(sent: u64, total: u64) -> u8 {
    if total == 0 {
        return 100;
    }
    let pct = (u128::from(sent) * 100) / u128::from(total);
    pct.min(100) as u8
}

/// Split a file into upload chunks. Pure; unit-tested.
///
/// Returns `(start, len)` pairs covering `total` exactly, with only the LAST chunk allowed to
/// be short. Every provider here requires that: a short chunk in the middle is what breaks
/// Microsoft's 320 KiB rule and Dropbox's 4 MiB one.
pub fn chunk_spans(total: u64, chunk: u64) -> Vec<(u64, u64)> {
    if total == 0 || chunk == 0 {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut start = 0u64;
    while start < total {
        let len = chunk.min(total - start);
        spans.push((start, len));
        start += len;
    }
    spans
}

/// The `Content-Range` header for one chunk. Pure; unit-tested.
///
/// `bytes <first>-<last>/<total>`, with `last` INCLUSIVE, which is the shape both Google and
/// Microsoft specify. An off-by-one here is rejected by the provider on the first chunk, or,
/// worse, corrupts the file on the last one.
pub fn content_range(start: u64, len: u64, total: u64) -> String {
    let last = start.saturating_add(len).saturating_sub(1);
    format!("bytes {start}-{last}/{total}")
}

/// The name a file should have in the provider. Pure; unit-tested.
///
/// The local file name, with anything that could be read as a PATH refused rather than
/// sanitized. Refusing is the right call: every provider treats the name as a path component,
/// so a name containing a separator would silently upload somewhere other than the folder the
/// user chose, and there is no correct guess about what they meant.
pub fn remote_name(file: &Path) -> Result<String, String> {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if path_like(&name) {
        // The NAME is not quoted: it is the user's content and this message can reach a log.
        return Err("That file's name cannot be used in a cloud drive.".to_string());
    }
    Ok(name)
}

/// Whether a name could be read as a PATH rather than as one component of one. Pure;
/// unit-tested.
///
/// The rule [`remote_name`] has always applied, named so [`remote_folder_name`] applies exactly
/// the same one: an empty name, the two directory aliases, either separator, or a NUL. Every
/// provider here treats the name as one path component, so any of these would address something
/// other than what was asked for.
fn path_like(name: &str) -> bool {
    name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
}

/// The name a NEW FOLDER may carry at a provider (DRAGON-506). Pure; unit-tested.
///
/// [`remote_name`]'s rules, extended for a name a user TYPED rather than one taken off a file
/// this app just wrote:
///
/// * Surrounding whitespace is trimmed, and the trimmed name is what is created. A trailing
///   space is invisible in the field and would be the only difference between two folders; two
///   of the three providers silently trim it anyway, so accepting it as typed would mean the
///   folder came back under a name the picker had not stored.
/// * A control character is refused. A newline pasted out of another window is not a folder
///   name, and one that reached the provider would come back into the list as a row that draws
///   over its neighbours.
/// * The length is capped at [`FOLDER_NAME_MAX`], so the refusal happens in the dialog rather
///   than as an API error the user has to translate.
///
/// The refusals are SENTENCES a dialog can show, unlike `remote_name`'s single message, because
/// this one is answering something the user can fix by typing something else. The name itself is
/// never quoted back: it is the user's content and these strings can reach a log.
pub fn remote_folder_name(typed: &str) -> Result<String, String> {
    let name = typed.trim();
    if name.is_empty() {
        return Err("Type a name for the new folder.".to_string());
    }
    if path_like(name) {
        return Err("A folder name cannot contain a slash or be a dot on its own.".to_string());
    }
    if name.chars().any(|c| c.is_control()) {
        return Err("A folder name cannot contain line breaks or control characters.".to_string());
    }
    if name.chars().count() > FOLDER_NAME_MAX {
        return Err(format!("A folder name can be at most {FOLDER_NAME_MAX} characters."));
    }
    Ok(name.to_string())
}

/// The size of the file about to be uploaded.
fn file_size(file: &Path) -> Result<u64, String> {
    std::fs::metadata(file).map(|m| m.len()).map_err(|e| {
        format!("The file to upload could not be read ({}): {e}", crate::diag::path_shape(file))
    })
}

/// A copy of one byte range of a file, on disk, removed when it goes out of scope.
///
/// Chunked uploads need to send a RANGE of a file, and [`super::http::Body`] offers a whole
/// file or an in-memory string. A string cannot carry binary (it is escaped into curl's
/// config), and the config travels down a pipe that must stay small, so the range is written
/// to its own small file and sent with `--data-binary @path`. That is also what keeps the
/// upload payload out of the stdin pipe entirely, which the transport's module doc calls for.
///
/// Removed on drop, including on the error paths, so a failed upload does not leave chunks
/// behind in the runtime directory.
struct TempChunk {
    path: PathBuf,
}

impl TempChunk {
    /// A fresh, empty temp file plus the handle to fill it.
    ///
    /// The GUARD is returned first and constructed before anything is written, so every early
    /// return past this point still removes the file.
    ///
    /// # Why the mode and `create_new` (DRAGON-482)
    ///
    /// A chunk is a slice of the user's capture sitting on disk for the length of one request.
    /// `File::create` would give it the umask's mode, world-readable on a default umask, so
    /// anyone else on the machine could read the capture out of the runtime directory while it
    /// uploads. `create_new` is the other half: it refuses if the path exists at all, symlink
    /// included, so nothing that can write to that directory can plant a link and have us copy
    /// the capture through it. A stale file left by a previous process that reused our pid is
    /// removed first; `create_new` still refuses anything that reappears in between, so the
    /// removal cannot open a hole.
    ///
    /// Unix only for the mode. On Windows the file inherits the per-user runtime directory's
    /// ACL, which is not world-readable, so `create_new` alone is the whole job there.
    fn create() -> Result<(TempChunk, std::fs::File), String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        /// Makes each temp file name unique within a process, so two concurrent uploads
        /// cannot land on one path.
        static SEQ: AtomicU64 = AtomicU64::new(0);

        let path = PathBuf::from(crate::util::runtime_dir()).join(format!(
            "cck-cloud-chunk-{}-{}.bin",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let file = opts.open(&path).map_err(|e| {
            format!("A temporary upload file could not be created ({}): {e}", crate::diag::path_shape(&path))
        })?;
        Ok((TempChunk { path }, file))
    }

    /// Copy `len` bytes of `source`, starting at `start`, into a fresh file.
    fn write(source: &Path, start: u64, len: u64) -> Result<TempChunk, String> {
        use std::io::{Read as _, Seek as _, Write as _};
        let mut input = std::fs::File::open(source).map_err(|e| {
            format!("The file to upload could not be opened ({}): {e}", crate::diag::path_shape(source))
        })?;
        input
            .seek(std::io::SeekFrom::Start(start))
            .map_err(|e| format!("The file to upload could not be read from: {e}"))?;
        let (chunk, mut output) = TempChunk::create()?;
        let mut reader = input.take(len);
        let copied = std::io::copy(&mut reader, &mut output)
            .map_err(|e| format!("The file to upload could not be copied: {e}"))?;
        if copied != len {
            // Short of what the plan said: the file was truncated under us, and continuing
            // would upload a chunk whose Content-Range lies about its length.
            return Err("The file changed while it was being uploaded.".to_string());
        }
        output
            .flush()
            .map_err(|e| format!("A temporary upload file could not be written: {e}"))?;
        Ok(chunk)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempChunk {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Turn a failed API response into a message, given a sentence that names the operation.
///
/// The provider's own error text is never shown: it is written for a developer, it can be an
/// HTML page from something in the middle, and it can name internal ids. The STATUS is
/// logged, which is what actually diagnoses one of these.
fn api_failure(who: &str, what: &str, response: &CurlResponse) -> String {
    log::warn!("cloud {who}: {what} came back {}", response.status);
    match response.status {
        401 | 403 => super::oauth::reconnect_message(&format!(
            "the cloud service refused this account's permission to {what}."
        )),
        404 => format!("The cloud service could not find what it needed to {what}."),
        413 => "That file is larger than the cloud service accepts.".to_string(),
        429 => "The cloud service is asking for fewer requests. Try again in a few minutes."
            .to_string(),
        507 => "There is not enough room left in that cloud drive.".to_string(),
        _ => format!("The cloud service could not {what}."),
    }
}

/// Read one JSON field out of a response body, or say the reply was unreadable.
fn json_field(body: &str, field: &str, what: &str) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get(field).and_then(|f| f.as_str()).map(str::to_string))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("The cloud service's reply after {what} could not be read."))
}

#[cfg(test)]
mod album_tests {
    use super::*;

    fn album(name: &str, uid: &str) -> RemoteFolder {
        RemoteFolder { id: format!("/albums/{uid}"), name: name.to_string() }
    }

    /// The find half of find-or-create: the app's own album is recognised so a second one is
    /// never made beside it.
    #[test]
    fn the_apps_own_album_is_found_by_name() {
        let albums = [album("Holidays", "a"), album("Cosmic Capture Kit", "b")];
        assert_eq!(app_album(&albums, "Cosmic Capture Kit"), Some(1));
    }

    /// Case and surrounding space never make it a different album: a user who renamed its case
    /// in Proton's own app must not get a duplicate on the next open.
    #[test]
    fn the_match_ignores_case_and_surrounding_space() {
        let albums = [album("  cosmic capture kit  ", "b")];
        assert_eq!(app_album(&albums, "Cosmic Capture Kit"), Some(0));
        assert_eq!(app_album(&albums, "  Cosmic Capture Kit"), Some(0));
    }

    /// Nothing matching means there is something to create, which is the other half.
    #[test]
    fn an_absent_album_reports_nothing_to_find() {
        assert_eq!(app_album(&[], "Cosmic Capture Kit"), None);
        assert_eq!(app_album(&[album("Holidays", "a")], "Cosmic Capture Kit"), None);
        // A name that merely CONTAINS ours is a different album; this is not a prefix match.
        assert_eq!(app_album(&[album("Cosmic Capture Kit 2", "a")], "Cosmic Capture Kit"), None);
    }

    /// **The DRAGON-522 report: listing must not create.** Opening the Photos tab, refreshing it
    /// or editing an account whose album is something else all list, and a listing that
    /// provisioned left a "Cosmic Capture Kit" album behind every time. The one case that still
    /// provisions on a listing is the account whose destination genuinely resolves to the
    /// default: Photos, with no album of its own, which is what an upload would fall back to.
    #[test]
    fn only_an_account_that_resolves_to_the_default_provisions_on_a_listing() {
        // The account the report was about: on Photos, with a different album saved.
        assert!(!needs_default_album(true, Some("/albums/vol~something-else")));
        // Browsing an account that uploads to FILES, whichever tab is on screen: the tab is not
        // a destination until Done writes it.
        assert!(!needs_default_album(false, None));
        assert!(!needs_default_album(false, Some("/albums/vol~a")));
        // The one that does: already on the album axis, naming no album.
        assert!(needs_default_album(true, None));
        // A blank id is no id: a hand-edited file, or one a migration cleared.
        for blank in ["", "   ", "\n"] {
            assert!(needs_default_album(true, Some(blank)), "{blank:?}");
        }
    }

    /// A provider with no album axis refuses rather than answering "no albums" with an empty
    /// list, which would draw an empty picker on a provider that has none to draw.
    #[test]
    fn a_folders_only_provider_refuses_the_album_calls() {
        let ops: &dyn ProviderOps = &gdrive::GDrive;
        assert!(ops.list_albums("t").is_err());
        assert!(ops.create_album("t", "Holidays").is_err());
        assert!(ops.delete_album("t", "/albums/a").is_err());
        // And the refusal is the same sentence wherever it comes from.
        assert_eq!(ops.list_albums("t").unwrap_err(), NO_ALBUMS);
    }

    /// The registry and the implementations agree: the provider that declares an album root is
    /// the provider whose ops answer album calls, and no other. A row added with one and not the
    /// other would draw a Photos tab whose every control failed.
    #[test]
    fn only_the_provider_with_an_album_root_implements_the_album_calls() {
        for spec in super::super::registry() {
            let account = CloudAccount {
                id: "0123456789abcdef0123456789abcdef".to_string(),
                provider: spec.id.to_string(),
                label: String::new(),
                folder_id: None,
                folder_name: None,
                added_at: String::new(),
                visibility: None,
                destination: None,
            };
            let Ok(ops) = ops(&account) else { continue };
            let refuses = ops.list_albums("t").as_ref().err().map(String::as_str) == Some(NO_ALBUMS);
            assert_eq!(
                super::super::has_albums(spec),
                !refuses,
                "`{}`'s album root and its ops disagree",
                spec.id
            );
        }
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    fn account(provider: &str) -> CloudAccount {
        CloudAccount {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            provider: provider.to_string(),
            label: String::new(),
            folder_id: None,
            folder_name: None,
            added_at: String::new(),
            visibility: None,
            destination: None,
        }
    }

    /// Every connectable provider in the registry has an implementation here. A provider row
    /// added without one would compile and then fail at runtime for the user who connects it.
    #[test]
    fn every_connectable_provider_has_ops() {
        for spec in super::super::registry() {
            let has_ops = ops(&account(spec.id)).is_ok();
            assert_eq!(
                has_ops,
                spec.auth.is_connectable(),
                "{} is connectable={} but has_ops={has_ops}",
                spec.id,
                spec.auth.is_connectable()
            );
        }
    }

    /// The two answers a caller can get for an unusable provider are different sentences,
    /// because "this build is too old" and "no such API exists" need different reactions.
    #[test]
    fn an_unusable_provider_says_which_kind_it_is() {
        // `.map(drop)` because a `&dyn ProviderOps` has no `Debug` for `expect_err` to print.
        let unofficial = ops(&account("icloud")).map(drop).expect_err("icloud has no API");
        assert!(unofficial.contains("iCloud Drive"), "{unofficial}");
        // Proton moved OUT of that group in DRAGON-485: it has ops now, reached through
        // Proton's own tool rather than through an API of ours.
        assert!(ops(&account("proton")).is_ok(), "proton must dispatch to its own ops");
        let unknown = ops(&account("a-drive-from-the-future")).map(drop).expect_err("unknown");
        assert!(unknown.contains("does not know"), "{unknown}");
    }
}

#[cfg(test)]
mod create_target_tests {
    use super::*;

    /// The `missing` copy a provider would inject, stood in for here so this tests the DECISION
    /// and not one provider's wording.
    const MISSING: &str = "That drive could not say where this app's own folder is.";

    /// **A level that names itself is used as it stands, and costs no lookup** (DRAGON-520).
    ///
    /// This is the depth that always worked: the browser is inside a folder whose id the listing
    /// already handed over, so a create has everything it needs.
    #[test]
    fn a_level_with_an_id_is_the_parent() {
        let used = create_parent_with(
            Some("1SubAaa"),
            || panic!("a level with an id must not cost a round trip"),
            MISSING,
        );
        assert_eq!(used.as_deref(), Ok("1SubAaa"));
        // Trimmed, because an id that arrived with whitespace around it would address nothing.
        assert_eq!(
            create_parent_with(Some("  1SubAaa \n"), || panic!("no lookup"), MISSING).as_deref(),
            Ok("1SubAaa")
        );
    }

    /// **The top level RESOLVES the app folder, and refuses when it cannot.**
    ///
    /// The whole of DRAGON-520 in one assertion pair: an unresolvable root is an `Err` the dialog
    /// puts on its notice line, never a create at whatever the provider's default parent happens
    /// to be. Drive's default is the top of My Drive, where the scoped listing cannot see it, so
    /// the "harmless" fallback is exactly the silent failure the owner reported.
    #[test]
    fn the_top_level_resolves_the_app_folder_or_refuses() {
        let refused = Err(MISSING.to_string());
        let resolved = create_parent_with(None, || Ok(Some("1AppFolder".to_string())), MISSING);
        assert_eq!(resolved.as_deref(), Ok("1AppFolder"));
        // Both shapes of "there isn't one" are refusals, not defaults.
        assert_eq!(create_parent_with(None, || Ok(None), MISSING), refused);
        assert_eq!(create_parent_with(None, || Ok(Some("   ".to_string())), MISSING), refused);
        // An empty parent means the same thing as no parent: the top level.
        assert_eq!(
            create_parent_with(Some(""), || Ok(Some("1App".into())), MISSING).as_deref(),
            Ok("1App")
        );
        assert_eq!(create_parent_with(Some("  "), || Ok(None), MISSING), refused);
    }

    /// A lookup that FAILED keeps its own reason. The provider's `missing` copy answers "there is
    /// no such folder", which is a different thing from "we could not ask", and a signed-out
    /// account must still be able to say so (it is what carries `oauth`'s reconnect marker).
    #[test]
    fn a_failed_lookup_keeps_its_own_reason() {
        let reason = super::super::oauth::reconnect_message("this account needs to sign in again.");
        let failed = create_parent_with(None, || Err(reason.clone()), MISSING).expect_err("failed");
        assert_eq!(failed, reason);
        assert!(super::super::oauth::needs_reconnect(&failed), "the marker was flattened away");
    }
}

#[cfg(test)]
mod allowlist_tests {
    use super::*;

    /// The allowlist covers every host the provider files actually call, and carries no
    /// duplicates. A missing host is a request the transport refuses; a wrong one is a host
    /// this app should never reach.
    #[test]
    fn the_api_allowlist_covers_every_provider_host() {
        let hosts = api_hosts();
        for expected in [
            // OAuth, from the registry.
            "accounts.google.com",
            "oauth2.googleapis.com",
            "login.microsoftonline.com",
            "www.dropbox.com",
            "api.dropboxapi.com",
            // The APIs, from the provider files.
            "www.googleapis.com",
            "graph.microsoft.com",
            "content.dropboxapi.com",
        ] {
            assert!(hosts.contains(&expected), "{expected} is missing from the allowlist");
        }
        let mut sorted = hosts.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), hosts.len(), "the allowlist has a duplicate");
        for host in &hosts {
            assert!(!host.is_empty(), "an empty host would allow anything");
        }
    }

    /// The allowlist is not a wildcard. A host that merely looks like a provider's is not
    /// one, which is what [`super::super::http::check_url`] enforces on top of this list.
    #[test]
    fn a_lookalike_host_is_not_allowed() {
        let hosts = api_hosts();
        for bad in [
            "www.googleapis.com.evil.test",
            "graph.microsoft.com.evil.test",
            "evil.test",
            "googleapis.com",
        ] {
            assert!(!hosts.contains(&bad), "{bad} must not be allowlisted");
        }
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;

    /// Build a response the way the transport would.
    fn refused(status: u16, body: &str) -> CurlResponse {
        CurlResponse { status, body: body.as_bytes().to_vec(), headers: Vec::new() }
    }

    /// **The scope-change path** (DRAGON-498). An account connected before OneDrive's scope
    /// became `Files.ReadWrite.AppFolder` keeps a refresh token that still works: Microsoft
    /// documents that a refresh is "valid for all permissions that your client has already
    /// received consent for", so the token renews happily and simply does not carry the new
    /// scope. The first Graph call then comes back 403, and this is what has to turn that into
    /// the RECONNECT the settings row can act on, rather than a shrug the user cannot.
    ///
    /// The body is Graph's own documented error envelope. It is never parsed (the message is
    /// documented as unstable and is written for a developer); the STATUS is what decides.
    #[test]
    fn a_graph_insufficient_scope_refusal_asks_for_a_reconnect() {
        let body = r#"{
            "error": {
                "code": "accessDenied",
                "message": "Either scp or roles claim need to be present in the token.",
                "innerError": {"request-id": "1a2b3c4d", "date": "2026-08-03T09:21:55"}
            }
        }"#;
        let message = api_failure("onedrive", "list folders", &refused(403, body));
        assert!(
            super::super::oauth::needs_reconnect(&message),
            "an insufficient-scope refusal must offer Reconnect: {message}"
        );
        // A dead or malformed token is the same answer from the user's side.
        let expired = r#"{"error":{"code":"InvalidAuthenticationToken","message":"Access token has expired."}}"#;
        assert!(super::super::oauth::needs_reconnect(&api_failure(
            "onedrive",
            "upload that file",
            &refused(401, expired)
        )));
        // And the provider's own words never reach the user, on either.
        assert!(!message.contains("scp"), "{message}");
        assert!(!message.contains("accessDenied"), "{message}");
    }

    /// A refusal that is NOT about permission stays an ordinary failure, or the row would
    /// offer Reconnect for a full drive and a rate limit.
    #[test]
    fn an_ordinary_refusal_is_not_a_reconnect() {
        for status in [404, 413, 429, 500, 507] {
            let message = api_failure("gdrive", "upload that file", &refused(status, "{}"));
            assert!(
                !super::super::oauth::needs_reconnect(&message),
                "status {status} must not read as a dead sign-in: {message}"
            );
        }
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    /// The cancellation sentinel is recognised by its own checker and by nothing that merely
    /// happens to contain similar words — the same care `oauth::needs_reconnect` takes with
    /// `RECONNECT_PREFIX`.
    #[test]
    fn the_canceled_sentinel_is_recognised_and_nothing_else_is() {
        let err = canceled_err();
        assert!(is_canceled(&err), "{err}");
        assert!(!is_canceled("The cloud service could not upload that file."));
        assert!(!is_canceled("canceled"), "the bare word alone is not the sentinel");
        assert!(!is_canceled(""));
    }
}

#[cfg(test)]
mod pagination_tests {
    use super::*;

    /// The cap counts REQUESTS, so the first page is always allowed and exactly
    /// [`MAX_LIST_PAGES`] of them can be made. One place decides this, because three provider
    /// loops asking it separately is three chances to be off by one: one too few silently drops
    /// a page of folders, one too many is the bound not holding.
    #[test]
    fn a_listing_follows_its_cursor_up_to_the_cap_and_no_further() {
        assert!(may_fetch_another_page(0), "the first page is never capped");
        assert!(may_fetch_another_page(MAX_LIST_PAGES - 1), "the last allowed page");
        assert!(!may_fetch_another_page(MAX_LIST_PAGES), "one past the cap");
        assert!(!may_fetch_another_page(u32::MAX));
        // Counted, not open-ended: the number of pages a loop can take is exactly the cap.
        let mut pages = 0u32;
        while may_fetch_another_page(pages) {
            pages += 1;
        }
        assert_eq!(pages, MAX_LIST_PAGES);
    }

    /// **The time half of the accounting.** Each page is one request bounded by
    /// [`META_BUDGET`], so the listing's own worst case is their product, and it has to stay a
    /// wait a user could sit through rather than one they would have to kill the app to end.
    /// The compile-time assert beside the constant pins the same relation; this states the
    /// number the assert is protecting.
    #[test]
    fn the_page_cap_bounds_a_whole_listing_in_time() {
        let worst = META_BUDGET * MAX_LIST_PAGES;
        assert_eq!(worst, Duration::from_secs(300), "a folder listing must not run past 5 minutes");
        // And the cap is worth having: a thousand folders at one level is far past what a
        // destination picker can usefully show. Drive's is the smallest page of the three.
        assert!(MAX_LIST_PAGES as u64 * 100 >= 1000, "too few pages would hide real folders");
    }
}

#[cfg(test)]
mod folder_naming_tests {
    use super::*;

    /// A typed folder name is TRIMMED and then held to the same path rules an uploaded file's
    /// name is: the separators are what would create something other than what was typed.
    #[test]
    fn a_typed_folder_name_is_trimmed_and_refused_when_it_is_a_path() {
        assert_eq!(remote_folder_name("Screenshots"), Ok("Screenshots".to_string()));
        assert_eq!(remote_folder_name("  Work notes  "), Ok("Work notes".to_string()));
        // Unicode and punctuation are ordinary folder names, and stay accepted.
        assert_eq!(remote_folder_name("Réunions 2026"), Ok("Réunions 2026".to_string()));
        for bad in ["", "   ", ".", "..", "a/b", "a\\b", "a\0b"] {
            assert!(remote_folder_name(bad).is_err(), "{bad:?} must be refused");
        }
        // A dot inside a name is fine; only a name that IS a dot alias is not.
        assert_eq!(remote_folder_name("v1.2"), Ok("v1.2".to_string()));
    }

    /// A pasted line break is not a folder name, and a name longer than any provider accepts is
    /// refused HERE rather than as an API error the user has to translate.
    #[test]
    fn a_folder_name_refuses_control_characters_and_overlong_input() {
        assert!(remote_folder_name("two\nlines").is_err());
        assert!(remote_folder_name("tab\there").is_err());
        let longest = "x".repeat(FOLDER_NAME_MAX);
        assert_eq!(remote_folder_name(&longest), Ok(longest.clone()));
        assert!(remote_folder_name(&"x".repeat(FOLDER_NAME_MAX + 1)).is_err());
        // Counted in CHARACTERS, not bytes: a name of accented letters is not half as long.
        let accented = "é".repeat(FOLDER_NAME_MAX);
        assert_eq!(remote_folder_name(&accented), Ok(accented));
    }

    /// The refusals are sentences a dialog can show, and none of them quotes the name back:
    /// it is the user's content and these strings can reach a log.
    #[test]
    fn a_folder_name_refusal_is_a_sentence_that_quotes_nothing() {
        for bad in ["", "secret project/x", "with\na break", &"x".repeat(FOLDER_NAME_MAX + 1)] {
            let err = remote_folder_name(bad).expect_err("refused");
            assert!(err.ends_with('.'), "{err}");
            assert!(!err.contains("secret"), "{err}");
            assert!(!err.contains(bad.trim()) || bad.trim().is_empty(), "{err}");
        }
    }

    /// The file rule and the folder rule are ONE rule about paths, applied to the same strings.
    ///
    /// They differ only in WHERE the string comes from, and the difference is worth stating:
    /// [`remote_name`] is handed a whole path and takes its last component first, so `a/b` is
    /// the perfectly good name `b` there, while a folder name is what a user typed into one
    /// field and a slash in it means they meant something this dialog cannot do.
    #[test]
    fn the_file_and_folder_rules_agree_about_paths() {
        for bad in ["", ".", "..", "a/b", "a\\b", "a\0b"] {
            assert!(path_like(bad), "{bad:?}");
            assert!(remote_folder_name(bad).is_err(), "{bad:?}");
        }
        assert!(!path_like("Screenshots"));
        // The one input the two answer differently, and why.
        assert_eq!(remote_name(Path::new("a/b")), Ok("b".to_string()));
        assert!(remote_folder_name("a/b").is_err());
    }
}

#[cfg(test)]
mod missing_folder_tests {
    use super::*;

    /// The marker is recognised by its own checker and by nothing that merely reads like it,
    /// the same care [`is_canceled`] takes, and it is STRIPPED before anything shows the
    /// reason: the prefix is machinery, never copy.
    #[test]
    fn the_missing_marker_is_recognised_and_never_shown() {
        let err = missing_folder_err();
        assert!(is_missing_folder(&err), "{err}");
        assert!(!missing_folder_reason(&err).contains("cck-"), "the marker reached the user");
        assert!(!is_missing_folder("The cloud service could not find what it needed."));
        assert!(!is_missing_folder("folder missing"), "the bare words are not the marker");
        assert!(!is_missing_folder(""));
        // Anything unmarked passes through untouched, so a caller can filter every message.
        let ordinary = "The cloud service could not list folders.";
        assert_eq!(missing_folder_reason(ordinary), ordinary);
    }

    /// The two sentinels this module carries stay distinct: a cancelled upload is not a
    /// missing folder, and neither is a dead sign-in.
    #[test]
    fn the_sentinels_do_not_overlap() {
        assert!(!is_canceled(&missing_folder_err()));
        assert!(!is_missing_folder(&canceled_err()));
        let reconnect = super::super::oauth::reconnect_message("the sign-in is gone.");
        assert!(!is_missing_folder(&reconnect));
    }
}

#[cfg(test)]
mod folder_restore_tests {
    use super::*;
    use std::collections::HashMap;

    fn folder(id: &str, name: &str) -> RemoteFolder {
        RemoteFolder { id: id.to_string(), name: name.to_string() }
    }

    /// A fake drive: one entry per level, keyed by the parent id (`""` being the app folder).
    /// A level that is not in the map answers with the `Err` a provider would give for it.
    struct Drive {
        root: Option<RemoteFolder>,
        levels: HashMap<&'static str, Vec<RemoteFolder>>,
        /// Every parent the walk asked for, in order, so a test can pin how many round trips a
        /// restore actually costs.
        asked: Vec<Option<String>>,
    }

    impl Drive {
        fn new(root: Option<RemoteFolder>, levels: &[(&'static str, &[(&str, &str)])]) -> Self {
            let levels = levels
                .iter()
                .map(|(parent, folders)| {
                    (*parent, folders.iter().map(|(id, name)| folder(id, name)).collect())
                })
                .collect();
            Drive { root, levels, asked: Vec::new() }
        }

        fn list(&mut self, parent: Option<&str>) -> Result<FolderSetup, String> {
            self.asked.push(parent.map(str::to_string));
            let key = parent.unwrap_or("");
            match self.levels.get(key) {
                Some(folders) => Ok(FolderSetup {
                    // Only a root listing reports a root, exactly as `browse_folders` does.
                    root: parent.is_none().then(|| self.root.clone()).flatten(),
                    folders: folders.clone(),
                }),
                None => Err("The cloud service could not list folders.".to_string()),
            }
        }
    }

    fn sample_drive() -> Drive {
        Drive::new(
            Some(folder("root-id", "Cosmic Capture Kit")),
            &[
                ("", &[("w", "Work"), ("s", "Screenshots")]),
                ("w", &[("c", "Clients"), ("y", "2026")]),
                ("c", &[("a", "Acme")]),
                ("s", &[]),
                ("y", &[]),
                ("a", &[]),
            ],
        )
    }

    fn walk(
        drive: &mut Drive,
        segments: &[&str],
        destination: Option<&str>,
    ) -> Result<RestoredBrowse, String> {
        restore_browse_with(segments, destination, |parent| drive.list(parent))
    }

    /// **The whole ticket.** An account whose destination is three levels down re-opens INSIDE it:
    /// the trail carries an id per level (which is what the breadcrumb's presses need, and what no
    /// account has ever stored), and the list shows that level's own children.
    #[test]
    fn a_saved_destination_reopens_inside_itself() {
        let mut drive = sample_drive();
        let view = walk(&mut drive, &["Work", "Clients", "Acme"], Some("a")).expect("a view");
        assert_eq!(
            view.trail,
            vec![folder("w", "Work"), folder("c", "Clients"), folder("a", "Acme")]
        );
        assert_eq!(view.root, Some(folder("root-id", "Cosmic Capture Kit")));
        assert!(view.folders.is_empty(), "Acme has nothing in it");
        assert_eq!(view.note, None, "arriving is not worth a warning");
        // One round trip per level plus the app folder, and not one more: the walk lists each
        // level exactly once, in order.
        assert_eq!(
            drive.asked,
            vec![None, Some("w".to_string()), Some("c".to_string()), Some("a".to_string())]
        );
    }

    /// A brand new account has no stored path at all, so the walk is a single listing of the app
    /// folder. This is the shape the setup step had before the walk existed, and it must not have
    /// grown a round trip.
    #[test]
    fn an_account_with_no_destination_opens_at_the_app_folder() {
        let mut drive = sample_drive();
        let view = walk(&mut drive, &[], None).expect("a view");
        assert!(view.trail.is_empty(), "nothing to walk down to");
        assert_eq!(view.folders, vec![folder("w", "Work"), folder("s", "Screenshots")]);
        assert_eq!(view.root, Some(folder("root-id", "Cosmic Capture Kit")));
        assert_eq!(drive.asked, vec![None], "one listing, as before the walk existed");
    }

    /// **A folder that is gone is not an error** (the DRAGON-506 rule, applied before the fact):
    /// the walk stops at the deepest level that still exists and lets the breadcrumb say so,
    /// rather than failing a dialog the user then has no way out of.
    #[test]
    fn a_level_deleted_elsewhere_stops_the_walk_where_it_still_exists() {
        let mut drive = sample_drive();
        // "Work/Gone/Deeper": the first level resolves, the second is not in the listing.
        let view = walk(&mut drive, &["Work", "Gone", "Deeper"], Some("nope")).expect("a view");
        assert_eq!(view.trail, vec![folder("w", "Work")]);
        assert_eq!(view.folders, vec![folder("c", "Clients"), folder("y", "2026")]);
        assert_eq!(view.note, None, "a missing folder is not a provider failure");
        // The whole path being gone lands back at the app folder, which always exists.
        let mut fresh = sample_drive();
        let view = walk(&mut fresh, &["Nowhere"], Some("nope")).expect("a view");
        assert!(view.trail.is_empty());
        assert_eq!(view.folders, vec![folder("w", "Work"), folder("s", "Screenshots")]);
    }

    /// **A provider that refuses a level sends the browser back to the app folder**, carrying the
    /// reason. A half-walked trail built on top of a refusal would be a guess presented as a
    /// place, and the app folder is the one level every account has.
    #[test]
    fn a_refused_level_falls_back_to_the_app_folder_with_the_reason() {
        // "Work/2026" exists, but listing the INSIDE of a folder the fake drive does not know
        // fails the way a provider would.
        let mut drive = Drive::new(
            Some(folder("root-id", "Cosmic Capture Kit")),
            &[("", &[("w", "Work")]), ("w", &[("y", "2026")])],
        );
        let view = walk(&mut drive, &["Work", "2026"], Some("y")).expect("a view");
        assert!(view.trail.is_empty(), "everything resolved so far is discarded");
        assert_eq!(view.folders, vec![folder("w", "Work")], "the app folder's own listing");
        assert_eq!(view.note.as_deref(), Some("The cloud service could not list folders."));
    }

    /// The FIRST listing failing is the one ending with nothing to show at all, and it is the
    /// existing "the folder list could not be read" path rather than a view with an empty list.
    #[test]
    fn a_provider_that_cannot_list_anything_answers_err() {
        let mut drive = Drive::new(None, &[]);
        assert!(walk(&mut drive, &["Work"], Some("w")).is_err());
        assert!(walk(&mut drive, &[], None).is_err(), "even with nothing to walk");
    }

    /// **The stored id beats the stored name, for the destination only.** Two sibling folders can
    /// share a name (Drive allows it), and the destination is the one level whose id an account
    /// actually remembers, so it is the one level that can be identified exactly.
    #[test]
    fn the_destinations_own_id_decides_between_two_folders_of_one_name() {
        let mut drive = Drive::new(
            None,
            &[("", &[("first", "2026"), ("second", "2026")]), ("first", &[]), ("second", &[])],
        );
        let view = walk(&mut drive, &["2026"], Some("second")).expect("a view");
        assert_eq!(view.trail, vec![folder("second", "2026")], "the id picked the right one");
        // With no id to go on, the name is all there is, and the first match is taken.
        let mut unnamed = Drive::new(
            None,
            &[("", &[("first", "2026"), ("second", "2026")]), ("first", &[]), ("second", &[])],
        );
        let view = walk(&mut unnamed, &["2026"], None).expect("a view");
        assert_eq!(view.trail, vec![folder("first", "2026")]);
    }

    /// Names are matched case-insensitively and with the surrounding space trimmed, the same way
    /// every other name comparison in this app is: Dropbox stores a lowercased path, OneDrive
    /// treats names case-insensitively, and the stored path came off a TOML file a user may have
    /// hand-edited.
    #[test]
    fn a_stored_name_matches_its_folder_whatever_the_case() {
        let listed = [folder("w", "Work"), folder("s", " Screenshots ")];
        assert_eq!(restore_step(&listed, "work", None), Some(0));
        assert_eq!(restore_step(&listed, "  WORK  ", None), Some(0));
        assert_eq!(restore_step(&listed, "screenshots", None), Some(1));
        assert_eq!(restore_step(&listed, "Workshop", None), None, "a prefix is not a match");
        assert_eq!(restore_step(&listed, "", None), None);
        // A blank or unknown id falls through to the name rather than failing the segment.
        assert_eq!(restore_step(&listed, "Work", Some("   ")), Some(0));
        assert_eq!(restore_step(&listed, "Work", Some("not-here")), Some(0));
        assert_eq!(restore_step(&[], "Work", Some("w")), None);
    }

    /// A provider whose token root already IS the app folder reports no root, and the walk must
    /// carry that through untouched: a root invented here would be written onto the account as a
    /// destination id on a provider that stores none.
    #[test]
    fn a_provider_with_no_root_of_its_own_reports_none() {
        let mut drive = Drive::new(None, &[("", &[("s", "Screenshots")]), ("s", &[])]);
        let view = walk(&mut drive, &["Screenshots"], Some("s")).expect("a view");
        assert_eq!(view.root, None);
        assert_eq!(view.trail, vec![folder("s", "Screenshots")]);
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    /// **The property the UI depends on**: progress never goes backwards, and never exceeds
    /// 100. Swept over a real-sized file rather than spot-checked.
    #[test]
    fn progress_is_monotone_and_bounded() {
        let total: u64 = 137 * 1024 * 1024;
        let mut last = 0u8;
        for (start, len) in chunk_spans(total, 10 * 1024 * 1024) {
            let pct = progress_percent(start + len, total);
            assert!(pct >= last, "progress went backwards: {last} -> {pct}");
            assert!(pct <= 100);
            last = pct;
        }
        assert_eq!(last, 100, "a finished upload must read 100");
    }

    #[test]
    fn progress_handles_the_edges() {
        assert_eq!(progress_percent(0, 100), 0);
        assert_eq!(progress_percent(50, 100), 50);
        assert_eq!(progress_percent(100, 100), 100);
        // An empty file is finished the moment it starts.
        assert_eq!(progress_percent(0, 0), 100);
        // A provider that acknowledges more than we sent cannot produce 101.
        assert_eq!(progress_percent(200, 100), 100);
        // A size large enough that `sent * 100` would overflow a u64. Widening to u128 is
        // what makes this an answer rather than a panic; 49 rather than 50 is ordinary
        // truncation, since half of an odd number is just under half.
        assert_eq!(progress_percent(u64::MAX / 2, u64::MAX), 49);
        assert_eq!(progress_percent(u64::MAX, u64::MAX), 100);
    }
}

#[cfg(test)]
mod chunking_tests {
    use super::*;

    /// Chunks cover the file exactly, and only the last one is short. A short chunk in the
    /// middle is what breaks Microsoft's 320 KiB rule and Dropbox's 4 MiB one.
    #[test]
    fn chunks_cover_the_file_with_only_the_last_one_short() {
        let total = 25u64;
        let spans = chunk_spans(total, 10);
        assert_eq!(spans, vec![(0, 10), (10, 10), (20, 5)]);
        // Contiguous, complete, and no gaps.
        let mut expected_start = 0;
        for (start, len) in &spans {
            assert_eq!(*start, expected_start);
            expected_start += len;
        }
        assert_eq!(expected_start, total);
        // Every chunk but the last is exactly the chunk size.
        for (_, len) in &spans[..spans.len() - 1] {
            assert_eq!(*len, 10);
        }
    }

    #[test]
    fn chunking_handles_the_edges() {
        assert_eq!(chunk_spans(0, 10), Vec::new(), "an empty file has no chunks");
        assert_eq!(chunk_spans(10, 0), Vec::new(), "a zero chunk size cannot make progress");
        assert_eq!(chunk_spans(10, 10), vec![(0, 10)], "an exact fit is one chunk");
        assert_eq!(chunk_spans(5, 10), vec![(0, 5)], "smaller than a chunk is one short chunk");
        assert_eq!(chunk_spans(20, 10), vec![(0, 10), (10, 10)], "an exact multiple has no tail");
    }

    /// `Content-Range` is inclusive at both ends. An off-by-one is rejected on the first
    /// chunk or, worse, truncates the file on the last.
    #[test]
    fn the_content_range_is_inclusive() {
        assert_eq!(content_range(0, 26, 128), "bytes 0-25/128");
        assert_eq!(content_range(26, 102, 128), "bytes 26-127/128");
        assert_eq!(content_range(0, 1, 1), "bytes 0-0/1");
        // The whole file in one range.
        assert_eq!(content_range(0, 128, 128), "bytes 0-127/128");
    }

    /// A real chunk is copied byte-for-byte, and the temp file is gone once it is dropped.
    #[test]
    fn a_chunk_is_copied_exactly_and_cleaned_up() {
        let source = std::env::temp_dir().join(format!("cck-chunk-src-{}.bin", std::process::id()));
        let bytes: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        std::fs::write(&source, &bytes).expect("write the source");
        let path = {
            let chunk = TempChunk::write(&source, 250, 500).expect("copy a chunk");
            let got = std::fs::read(chunk.path()).expect("read the chunk back");
            assert_eq!(got, bytes[250..750], "the chunk is not the bytes it should be");
            chunk.path().to_path_buf()
        };
        assert!(!path.exists(), "the temp chunk outlived its guard");
        // A trailing chunk shorter than the request is refused rather than silently short.
        assert!(TempChunk::write(&source, 900, 500).is_err(), "a short read must be refused");
        let _ = std::fs::remove_file(&source);
    }

    /// **The permissions case.** A chunk is a slice of the user's capture on disk. It must not
    /// be readable by anyone else on the machine for the length of the request, and the mode
    /// is set AT OPEN so there is no window where it is not.
    #[cfg(unix)]
    #[test]
    fn a_chunk_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let (chunk, _file) = TempChunk::create().expect("a temp chunk");
        let mode = std::fs::metadata(chunk.path()).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "an upload chunk must be owner read/write only");
    }
}

#[cfg(test)]
mod naming_and_budget_tests {
    use super::*;

    /// A name that could be read as a path is refused, not sanitized: every provider treats
    /// it as a path component, so a guess would upload somewhere the user did not choose.
    #[test]
    fn a_name_that_could_be_a_path_is_refused() {
        assert_eq!(remote_name(Path::new("/home/u/Pictures/shot.png")), Ok("shot.png".to_string()));
        assert_eq!(remote_name(Path::new("clip.mp4")), Ok("clip.mp4".to_string()));
        // A name with spaces and unicode is fine: providers accept it.
        assert_eq!(remote_name(Path::new("/tmp/Screen shot é.png")), Ok("Screen shot é.png".to_string()));
        for bad in ["/", "..", ".", ""] {
            assert!(remote_name(Path::new(bad)).is_err(), "{bad:?} must be refused");
        }
        // A trailing separator is NOT a refusal: `Path::file_name` normalizes it away, so
        // `/tmp/` names `tmp`. A directory handed here is caught when its size is read, which
        // is the check that can actually tell a directory from a file.
        assert_eq!(remote_name(Path::new("/tmp/")), Ok("tmp".to_string()));
    }

    /// The refusal must not quote the file name: it is the user's content and this reaches a
    /// log.
    #[test]
    fn the_name_refusal_quotes_nothing() {
        let err = remote_name(Path::new("/")).expect_err("refused");
        assert!(!err.contains('/'));
    }

    /// The budget grows with the file but stays inside both bounds, so one timeout can serve
    /// a 40 KB screenshot and a 200 MB recording.
    #[test]
    fn the_upload_budget_scales_between_its_bounds() {
        assert_eq!(upload_budget(0), Duration::from_secs(60), "the floor");
        assert_eq!(upload_budget(40 * 1024), Duration::from_secs(62));
        assert!(upload_budget(200 * 1024 * 1024) > Duration::from_secs(600));
        assert_eq!(upload_budget(u64::MAX), Duration::from_secs(1800), "the ceiling");
        // Monotone: a bigger file never gets less time.
        let mut last = Duration::ZERO;
        for mb in [0u64, 1, 10, 50, 200, 1000, 5000] {
            let budget = upload_budget(mb * 1024 * 1024);
            assert!(budget >= last, "{mb} MB got less time than the size below it");
            last = budget;
        }
    }
}
