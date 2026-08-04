//! OneDrive, through Microsoft Graph v1.0 (DRAGON-482, stage A2).
//!
//! Verified against `learn.microsoft.com/graph/api/driveitem-createuploadsession`,
//! `driveitem-put-content`, `driveitem-createlink`, `driveitem-list-children`,
//! `driveitem-delete`, and `learn.microsoft.com/graph/use-the-api`.
//!
//! This is the provider with a complete chunked upload, so it is the one to read first when
//! following how [`super::chunk_spans`], [`super::content_range`] and [`super::TempChunk`]
//! fit together.
//!
//! # Three things about it that are not obvious
//!
//! **The upload URL is a credential, and it is not on our host.** `createUploadSession`
//! returns an `uploadUrl` in its response BODY, pointing at a host Microsoft chooses: a
//! `*.up.1drv.com` shard for a personal account, the tenant's own `*.sharepoint.com` for a
//! work or school one. That URL carries its own `tempauth` token, which is why Microsoft says
//! outright: "If you include the `Authorization` header when issuing the PUT call, it might
//! result in an `HTTP 401 Unauthorized` response. … Don't include it when issuing the PUT
//! call." So chunks go out with NO bearer token, and the URL is treated as a secret: it is
//! never logged, not even in part, and [`is_upload_session_host`] is what keeps a
//! surprising host from receiving our bytes.
//!
//! **Chunks must be a multiple of 320 KiB.** Not a suggestion: "the size of each byte range
//! MUST be a multiple of 320 KiB (327,680 bytes)", and "using a fragment size that doesn't
//! divide evenly by 320 KiB results in errors committing some files", i.e. the failure lands
//! at the END of a long upload. [`CHUNK_BYTES`] is pinned to that by a compile-time assert.
//!
//! **`$filter` does not work here.** OneDrive's documented query options for a children
//! listing are `$expand`, `$select`, `$skipToken`, `$top` and `$orderby`; filtering is listed
//! as "not available". So folders are separated from files CLIENT-side by looking for the
//! `folder` facet, which is also the only way that works on both personal and business
//! accounts.
//!
//! # The app folder, and why nothing here can address the rest of the drive (DRAGON-498)
//!
//! The account is authorized with `Files.ReadWrite.AppFolder`, so every path in this file is
//! rooted at the SPECIAL FOLDER Graph provisions for this app registration, addressed as
//! `approot`. That name is Microsoft's, it is documented as CASE SENSITIVE, and Graph creates
//! the folder itself the first time the app touches it ("Special folders are automatically
//! created the first time an application attempts to write to one"), under
//! `Apps/{the registration's display name}`. There is no create-the-folder call to make and no
//! folder id to remember: `/me/drive/special/approot` IS the root, which is why
//! [`super::ProviderOps::ensure_root_folder`] has nothing to do here.
//!
//! Two request shapes come out of that, both straight from the app-folder page:
//! `/me/drive/special/approot/children` for a listing, and the colon form
//! `/me/drive/special/approot:/NAME:/content` (or `:/createUploadSession`) for a file. Once a
//! SUBFOLDER inside it has been chosen, its item id addresses it exactly as before
//! (`/me/drive/items/ID/...`), because an id is already relative to nothing.
//!
//! **A listing can 404 before the folder exists.** Graph documents that a request for a special
//! folder "may fail with a `404 Not Found`" when it has not been created yet, which is precisely
//! the state a freshly connected account is in: the setup step lists folders before anything has
//! ever been uploaded. So [`OneDrive::list_folders`] reads a 404 on the approot listing as the
//! EMPTY listing it actually is, rather than as a failure (see the arm's own comment).
//!
//! **A work or school account is the open question.** Microsoft's own documentation disagrees
//! with itself: the OneDrive permission-scope reference says `Files.ReadWrite.AppFolder` is
//! "only valid for personal accounts", while the newer Graph app-folder concept page says the
//! app folder "works across OneDrive for work or school and OneDrive for home". This app signs
//! in through the `common` authority, which accepts both kinds, so a business account may or may
//! not be able to reach `approot`. It is left as-is deliberately: the alternative is asking every
//! user for `Files.ReadWrite` over their entire drive to cover a case that may not need it, and a
//! business account that cannot reach the app folder gets a 403, which is already the RECONNECT
//! message rather than a silent failure.

use std::path::Path;

use super::{
    META_BUDGET, ProviderOps, RemoteFile, RemoteFolder, TempChunk, api, api_failure, bearer,
    chunk_spans, content_range, file_size, json_field, progress_percent, remote_name,
    upload_budget,
};
use crate::cloud::accounts::CloudAccount;
use crate::cloud::http::{Body, CurlReq, Method, host_of};

/// Every https host this provider reaches for an API call.
///
/// The upload-session host is NOT here and cannot be: Microsoft picks it per account at
/// runtime. [`is_upload_session_host`] is the gate for that one.
pub(super) const HOSTS: &[&str] = &["graph.microsoft.com"];

/// The Graph base.
const API: &str = "https://graph.microsoft.com/v1.0";

/// The largest file sent in one PUT.
///
/// Microsoft's own docs disagree here: the OneDrive page says a simple upload "only supports
/// files up to 250 MB in size", while the platform-wide page says "Write requests in the
/// Microsoft Graph API have a size limit of 4 MB" and fail with HTTP 413. The smaller number
/// is used, because being wrong in that direction costs one extra round trip and being wrong
/// in the other costs the whole upload. Anything above this goes through an upload session,
/// which Microsoft recommends past 10 MiB anyway.
pub const SIMPLE_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The unit a non-final chunk must be a multiple of: 320 KiB.
pub const CHUNK_UNIT: u64 = 320 * 1024;

/// The chunk size. 10 MiB, which is 32 units exactly, and the size Microsoft calls "optimal"
/// for stable high speed connections while staying inside its 60 MiB per-request ceiling.
pub const CHUNK_BYTES: u64 = 32 * CHUNK_UNIT;

const _: () = assert!(
    CHUNK_BYTES.is_multiple_of(CHUNK_UNIT),
    "DRAGON-482: a Graph upload chunk must be a multiple of 320 KiB, or a long upload fails \
     when it is committed rather than when the bad chunk is sent"
);
const _: () = assert!(
    CHUNK_BYTES <= 60 * 1024 * 1024,
    "DRAGON-482: Graph refuses a single byte range above 60 MiB"
);
const _: () = assert!(
    SIMPLE_MAX_BYTES < CHUNK_BYTES,
    "DRAGON-482: a file that is too big for one PUT must be worth chunking, or the session \
     path pays a second round trip to send exactly one chunk"
);

/// How many children one listing asks for. Graph pages at 200 by default and documents no
/// ceiling, so this asks for the page size it already uses.
const CHILD_PAGE_SIZE: u32 = 200;

pub(super) struct OneDrive;

impl ProviderOps for OneDrive {
    fn upload(
        &self,
        acct: &CloudAccount,
        token: &str,
        file: &Path,
        on_progress: &mut dyn FnMut(u8),
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<RemoteFile, String> {
        let name = remote_name(file)?;
        let size = file_size(file)?;
        if size <= SIMPLE_MAX_BYTES {
            // One request, no per-chunk pause to check between: `should_cancel` is not
            // consulted here (see `providers::upload`'s doc for why).
            return simple_upload(acct, token, file, &name, size, on_progress);
        }
        session_upload(acct, token, file, &name, size, on_progress, should_cancel)
    }

    fn create_share_link(&self, token: &str, file_id: &str) -> Result<String, String> {
        let url = format!("{API}/me/drive/items/{}/createLink", esc(file_id));
        let response = bearer(api(Method::Post, &url, META_BUDGET)?, token)
            .header("Content-Type", "application/json")
            .body(Body::Text(r#"{"type":"view","scope":"anonymous"}"#.to_string()))
            .send()?;
        // 201 for a new link, 200 when this app already made one for the item. Both are the
        // answer we wanted, which is also what makes the call idempotent.
        if !response.is_success() {
            return Err(api_failure("onedrive", "share that file", &response));
        }
        parse_link(&response.text())
    }

    /// Every folder inside `parent`, following `@odata.nextLink` to the end (DRAGON-506).
    ///
    /// The next link is a WHOLE URL Graph builds, not a token to append, so each page after the
    /// first is fetched by following it. It comes off the network, so its host is checked
    /// against Graph's own before it is followed ([`is_graph_url`]); see that function for why
    /// the shared allowlist is the wrong check here.
    fn list_folders(&self, token: &str, parent: Option<&str>) -> Result<Vec<RemoteFolder>, String> {
        let base = format!("{API}{}", children_path(parent));
        // `$orderby=name` is the one sort documented for BOTH personal and business accounts;
        // the final ordering is imposed by `providers::browse_folders` regardless.
        let mut url = format!("{base}?$select=id,name,folder&$top={CHILD_PAGE_SIZE}&$orderby=name");
        let mut folders: Vec<RemoteFolder> = Vec::new();
        let mut pages = 0u32;
        while super::may_fetch_another_page(pages) {
            let response = bearer(api(Method::Get, &url, META_BUDGET)?, token).send()?;
            // **An app folder nobody has written to yet has no listing, and that is not an
            // error.** Graph creates `approot` on first use and documents that asking for a
            // special folder "may fail with a 404 Not Found" until then, which is exactly where
            // a freshly connected account stands when the setup step asks what is inside it.
            // Reported as the empty listing it is; the first upload creates the folder
            // (DRAGON-498). Only for the ROOT: a 404 on a SUBFOLDER id means that folder is
            // gone, which is a real answer and since DRAGON-506 a named one, so the setup step
            // can walk its breadcrumb up a level instead of failing.
            if response.status == 404 {
                if parent.filter(|p| !p.is_empty()).is_none() {
                    log::debug!("cloud onedrive: the app folder does not exist yet, so it is empty");
                    return Ok(Vec::new());
                }
                log::debug!("cloud onedrive: a folder being listed is no longer there");
                return Err(super::missing_folder_err());
            }
            if !response.is_success() {
                return Err(api_failure("onedrive", "list folders", &response));
            }
            let body = response.text();
            folders.extend(parse_folders(&body)?);
            pages += 1;
            let Some(next) = next_link(&body) else {
                return Ok(folders);
            };
            url = next;
        }
        log::warn!("cloud onedrive: stopped following a folder listing after {pages} pages");
        Ok(folders)
    }

    /// `POST /me/drive/items/{parent-item-id}/children` with the folder facet.
    ///
    /// **Always the ITEM form, never the special-folder collection** (DRAGON-520). This used to
    /// post at whatever [`children_path`] built, so a create at the top level went to
    /// `/me/drive/special/approot/children` and came back refused. Graph's createFolder reference
    /// documents exactly two request forms, `/me/drive/items/{parent-item-id}/children` and
    /// `/me/drive/root/children`, and the second is the user's whole drive, which the app-folder
    /// scope cannot reach. So the app folder's own item id is fetched first ([`approot_id`]) and
    /// the create addresses an item like every other write in this file does. A subfolder already
    /// had a real id and always took this path, which is why only the top level was broken.
    ///
    /// `conflictBehavior: fail` on purpose, and NOT the `rename` an upload uses: an upload is a
    /// capture the user has already asked to keep, so a taken name must not lose it, while a
    /// folder is something they just NAMED. Renaming it to "Screenshots 1" behind their back
    /// would answer a different question than the one they asked.
    fn create_folder(
        &self,
        token: &str,
        parent: Option<&str>,
        name: &str,
    ) -> Result<RemoteFolder, String> {
        let parent =
            super::create_parent_with(parent, || approot_id(token).map(Some), ROOT_UNKNOWN)?;
        let url = format!("{API}{}", create_children_path(&parent));
        let response = bearer(api(Method::Post, &url, META_BUDGET)?, token)
            .header("Content-Type", "application/json")
            .body(Body::Text(create_folder_request(name)))
            // Not idempotent: with `fail` a retry after a create that landed comes back 409 and
            // would read as "that name is taken" for a folder we had just made ourselves.
            .no_retry()
            .send()?;
        if response.status == 409 {
            return Err("That folder name is already taken here.".to_string());
        }
        if !response.is_success() {
            return Err(api_failure("onedrive", "create that folder", &response));
        }
        parse_created_folder(&response.text(), name)
    }

    fn delete_file(&self, token: &str, file_id: &str) -> Result<(), String> {
        let response =
            bearer(api(Method::Delete, &delete_url(file_id), META_BUDGET)?, token).send()?;
        if !response.is_success() {
            return Err(api_failure("onedrive", "delete that file", &response));
        }
        Ok(())
    }

    /// `DELETE /me/drive/items/{id}`, which Graph documents as moving the item to the RECYCLE
    /// BIN rather than removing it: "Note: Deleting items using this method will move the items
    /// to the recycle bin instead of permanently deleting the item." That is the whole reason
    /// `ProviderCaps::delete_recoverable` is true here alone, and why this provider's
    /// confirmation can offer the user a way back.
    ///
    /// One id, at every depth: a folder listed inside the app folder carries a real driveItem id
    /// exactly as one three levels down does, so a delete needed none of DRAGON-520's root-level
    /// resolution. The app folder itself is refused in the UI and never reaches this.
    fn delete_folder(&self, token: &str, folder_id: &str) -> Result<(), String> {
        let response =
            bearer(api(Method::Delete, &delete_url(folder_id), META_BUDGET)?, token).send()?;
        if !response.is_success() {
            return Err(api_failure("onedrive", "delete that folder", &response));
        }
        Ok(())
    }

    fn revoke(&self, _token: &str) -> Result<(), String> {
        // **There is nothing to call.** The Microsoft identity platform v2.0 publishes no
        // OAuth 2.0 token-revocation endpoint (RFC 7009) for third-party clients: its own
        // metadata document at
        // login.microsoftonline.com/common/v2.0/.well-known/openid-configuration advertises
        // none, and RFC 8414 requires one to be advertised if it exists. The two mechanisms
        // Microsoft does document are not this: `end_session_endpoint` ends a BROWSER session
        // and takes no token, and Graph's `revokeSignInSessions` needs the privileged
        // `User.RevokeSessions.All` scope, is "Not supported" for personal Microsoft
        // accounts, and would invalidate every application's tokens for that user rather than
        // only ours.
        //
        // So disconnecting a OneDrive account is local-only: the tokens are deleted from the
        // keyring (`secrets::delete`) and the refresh token is left to expire on its own,
        // which Microsoft documents as 90 days of inactivity. Ok(()) is the honest answer;
        // an error here would block a disconnect over something no client can do.
        log::debug!("cloud onedrive: no revocation endpoint exists; the local tokens are dropped");
        Ok(())
    }
}

/// One PUT, for a file small enough that Graph takes it whole.
fn simple_upload(
    acct: &CloudAccount,
    token: &str,
    file: &Path,
    name: &str,
    size: u64,
    on_progress: &mut dyn FnMut(u8),
) -> Result<RemoteFile, String> {
    let url = format!("{API}{}", item_path(acct.folder_id.as_deref(), name, "content"));
    let response = bearer(api(Method::Put, &url, upload_budget(size))?, token)
        .header("Content-Type", "application/octet-stream")
        .body(Body::File(file.to_path_buf()))
        // Not idempotent enough to repeat blindly: a retry after a request that actually
        // landed would either duplicate or overwrite depending on the conflict behaviour.
        .no_retry()
        .send()?;
    if !response.is_success() {
        return Err(api_failure("onedrive", "upload that file", &response));
    }
    let uploaded = parse_item(&response.text())?;
    on_progress(100);
    log::debug!("cloud onedrive: uploaded a file of {size} bytes in one request");
    Ok(uploaded)
}

/// createUploadSession, then one ranged PUT per chunk.
fn session_upload(
    acct: &CloudAccount,
    token: &str,
    file: &Path,
    name: &str,
    size: u64,
    on_progress: &mut dyn FnMut(u8),
    should_cancel: &dyn Fn() -> bool,
) -> Result<RemoteFile, String> {
    let url = format!("{API}{}", item_path(acct.folder_id.as_deref(), name, "createUploadSession"));
    let start = bearer(api(Method::Post, &url, META_BUDGET)?, token)
        .header("Content-Type", "application/json")
        .body(Body::Text(session_request(name)))
        .no_retry()
        .send()?;
    if !start.is_success() {
        return Err(api_failure("onedrive", "start that upload", &start));
    }
    let upload_url = parse_upload_session(&start.text())?;
    // The HOST is logged, never the URL: it carries its own auth token in the query.
    let host = host_of(&upload_url)
        .ok_or_else(|| "OneDrive returned an upload address this app cannot use.".to_string())?;
    if !is_upload_session_host(host) {
        // The HOST, alone, and never the URL around it (that one carries a `tempauth` token in
        // its query). This line is the whole diagnosis for the next time Microsoft moves the
        // upload address: it names the exact string to add to `is_upload_session_host`, which
        // is how DRAGON-514's own `.microsoftpersonalcontent.com` should have been found
        // without a round of guessing.
        log::warn!(
            "cloud onedrive: refused an upload session on the unrecognised host {host}; add it \
             to is_upload_session_host if it is genuinely Microsoft's"
        );
        return Err("OneDrive returned an upload address this app does not recognise.".to_string());
    }
    log::debug!("cloud onedrive: uploading {size} bytes to {host} in chunks");

    let spans = chunk_spans(size, CHUNK_BYTES);
    for (offset, len) in spans.iter() {
        // Checked BETWEEN chunks (DRAGON-490), the natural pause: the loop already stops
        // here between one request and the next.
        if should_cancel() {
            // Graph documents cancelling an upload session as a DELETE on the session's own
            // URL, so this one provider can hand the session back immediately instead of
            // leaving it to expire (DRAGON-495). Best effort: the session expires by itself
            // anyway, and a cancel must not be able to fail.
            cancel_session(&upload_url, host);
            return Err(super::canceled_err());
        }
        let chunk = TempChunk::write(file, *offset, *len)?;
        let response = put_chunk(&upload_url, host, &chunk, *offset, *len, size)?;
        on_progress(progress_percent(offset + len, size));
        // 202 means "accepted, send the next one". A 2xx on the LAST chunk carries the
        // finished item, which is the only place the file's id comes from.
        if response.status == 202 {
            continue;
        }
        if response.is_success() {
            let uploaded = parse_item(&response.text())?;
            log::debug!("cloud onedrive: finished a {size} byte upload in {} chunks", spans.len());
            return Ok(uploaded);
        }
        return Err(api_failure("onedrive", "upload part of that file", &response));
    }
    // Every chunk was accepted and none carried the item. Graph does not do this, but saying
    // so beats returning a file id we do not have.
    Err("OneDrive accepted the whole file but did not confirm it was saved.".to_string())
}

/// Cancel an upload session at the provider: `DELETE` on the session's own URL (DRAGON-495).
///
/// Graph specifies exactly this for abandoning a resumable upload, and the session URL is
/// itself pre-authorized, so it needs no bearer token (sending one can get the request
/// rejected, the same rule [`put_chunk`] follows).
///
/// Best effort and DELIBERATELY unreported: an upload session that is never deleted expires on
/// its own, holds no file the user can see, and this runs on the path where the user has just
/// asked for everything to stop. A failure is one debug line about BEHAVIOUR; the URL is never
/// logged, because it carries its own auth token in the query.
fn cancel_session(upload_url: &str, host: &str) {
    let result = CurlReq::new(Method::Delete, upload_url, &[host], META_BUDGET)
        .and_then(|req| req.no_retry().send());
    match result {
        Ok(response) if response.is_success() || response.status == 404 => {
            // 404: already gone, which is the same end state and not worth distinguishing.
            log::debug!("cloud onedrive: released a cancelled upload session");
        }
        Ok(response) => {
            log::debug!(
                "cloud onedrive: the provider kept a cancelled upload session (status {})",
                response.status
            );
        }
        Err(_) => {
            log::debug!("cloud onedrive: a cancelled upload session could not be released");
        }
    }
}

/// Send one chunk, with one retry on a transport failure.
///
/// The retry is safe HERE and nowhere else in an upload: a byte range is addressed by its
/// `Content-Range`, so re-sending the same range writes the same bytes to the same place.
/// Graph even documents the duplicate case (`416 Requested Range Not Satisfiable`), which is
/// what a range we already delivered looks like.
fn put_chunk(
    upload_url: &str,
    host: &str,
    chunk: &TempChunk,
    offset: u64,
    len: u64,
    total: u64,
) -> Result<crate::cloud::http::CurlResponse, String> {
    let attempt = || -> Result<crate::cloud::http::CurlResponse, String> {
        // The allowlist for this ONE request is the host we just vetted. It cannot come from
        // the compile-time list, because Microsoft chooses it per account.
        CurlReq::new(Method::Put, upload_url, &[host], upload_budget(len))?
            // NO Authorization header: the URL is itself pre-authorized, and sending one can
            // get the chunk rejected with a 401.
            .header("Content-Range", &content_range(offset, len, total))
            .body(Body::File(chunk.path().to_path_buf()))
            .no_retry()
            .send()
    };
    let first = attempt()?;
    if first.status == 0 || first.status >= 500 {
        log::debug!("cloud onedrive: a chunk came back {}; sending it once more", first.status);
        return attempt();
    }
    Ok(first)
}

/// Percent-encode a value for a URL path segment.
fn esc(value: &str) -> String {
    crate::cloud::oauth::percent_encode(value)
}

/// How the app folder itself is addressed: Microsoft's own special-folder name, which its
/// documentation calls out as CASE SENSITIVE.
///
/// A named constant rather than three literals, because `approot` written `appRoot` anywhere
/// would be a 404 that reads like a missing folder rather than a typo.
const APPROOT: &str = "approot";

/// The Graph path for an item being created by name under a parent. Pure; unit-tested.
///
/// Graph addresses "a new child called NAME" with its colon syntax:
/// `/me/drive/special/approot:/NAME:/content` at the top of the app folder (DRAGON-498, where
/// it used to be `/me/drive/root:/...`, the whole drive), or
/// `/me/drive/items/PARENT:/NAME:/content` under a chosen subfolder. The colons are structural
/// and stay literal; the NAME is percent-encoded, since it is user content and can hold
/// anything.
pub fn item_path(folder_id: Option<&str>, name: &str, action: &str) -> String {
    match folder_id.filter(|f| !f.is_empty()) {
        Some(folder) => format!("/me/drive/items/{}:/{}:/{action}", esc(folder), esc(name)),
        None => format!("/me/drive/special/{APPROOT}:/{}:/{action}", esc(name)),
    }
}

/// The Graph path for LISTING a folder's children. Pure; unit-tested.
///
/// `None` is the app folder, `/me/drive/special/approot/children`, which is the documented
/// special-folder listing form and NOT the colon syntax [`item_path`] uses (that one addresses
/// an item BY NAME; this one addresses a collection). A subfolder is listed by its item id, the
/// same as any other driveItem.
///
/// **Reading only** (DRAGON-520). A create used to POST at whatever this built, and the
/// special-folder arm is not a documented createFolder form; it now goes through
/// [`create_children_path`], which addresses the app folder by its real item id. `GET` on the
/// special collection stays exactly as it was, because that form IS documented and works.
pub fn children_path(parent: Option<&str>) -> String {
    match parent.filter(|p| !p.is_empty()) {
        Some(id) => format!("/me/drive/items/{}/children", esc(id)),
        None => format!("/me/drive/special/{APPROOT}/children"),
    }
}

/// What a create says when Graph would not name this app's own folder. User copy, so it names
/// WHAT could not happen as well as why.
const ROOT_UNKNOWN: &str = "OneDrive could not say where this app's own folder is, so the new \
                            folder had nowhere to go.";

/// The Graph path a create POSTs to (DRAGON-520). Pure; unit-tested.
///
/// Always the documented item form, never [`children_path`]'s special-folder collection. The id
/// is percent-encoded for the reason [`item_path`]'s is: an id that came off the network must not
/// be able to add a path segment.
pub fn create_children_path(parent_id: &str) -> String {
    format!("/me/drive/items/{}/children", esc(parent_id))
}

/// Where an item is deleted, file or folder. Pure; unit-tested.
///
/// One builder for both, because Graph makes no distinction: `DELETE /me/drive/items/{id}` moves
/// whichever it is to the recycle bin.
pub fn delete_url(id: &str) -> String {
    format!("{API}/me/drive/items/{}", esc(id))
}

/// Where the app folder's own metadata is read. Pure; unit-tested.
pub fn approot_url() -> String {
    format!("{API}/me/drive/special/{APPROOT}?$select=id")
}

/// The app folder's own driveItem id (DRAGON-520).
///
/// `GET /me/drive/special/approot`, which Graph documents as answering 200 with the driveItem, and
/// which is ALSO how the folder gets made: "Special folders are automatically created the first
/// time an application attempts to write to one, if it doesn't already exist." The documented 404
/// / 403 caveat there is for an app with READ-ONLY permission, which `Files.ReadWrite.AppFolder`
/// is not, so a brand new account that has never uploaded anything is provisioned by this call
/// rather than refused by it.
///
/// One extra round trip, paid only by a create at the TOP level. A create inside a subfolder
/// already knows a real item id and never calls this. Deliberately NOT folded into
/// [`super::ProviderOps::ensure_root_folder`], which would publish the id as
/// [`super::FolderSetup::root`] and make the settings page store it as the account's destination:
/// this provider's uploads address the app folder through the `approot` alias precisely so a
/// folder the user renames or moves keeps working, and an id written onto the account would give
/// that up for a call the browser makes once per created folder.
fn approot_id(token: &str) -> Result<String, String> {
    let response = bearer(api(Method::Get, &approot_url(), META_BUDGET)?, token).send()?;
    if !response.is_success() {
        return Err(api_failure("onedrive", "find this app's folder", &response));
    }
    parse_root_id(&response.text())
}

/// Read the app folder's id out of a special-folder reply. Pure; unit-tested.
pub fn parse_root_id(body: &str) -> Result<String, String> {
    serde_json::from_str::<DriveItem>(body)
        .ok()
        .and_then(|item| item.id)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ROOT_UNKNOWN.to_string())
}

/// The `createUploadSession` body. Pure; unit-tested.
///
/// `rename` rather than the default `fail`, so two captures made in the same second do not
/// collide and lose the second one. The user asked for the file to be uploaded; refusing
/// because a name is taken would be the wrong answer.
pub fn session_request(name: &str) -> String {
    serde_json::json!({
        "item": {
            "@microsoft.graph.conflictBehavior": "rename",
            "name": name,
        }
    })
    .to_string()
}

/// Whether a host may receive upload-session chunks. Pure; unit-tested.
///
/// Microsoft calls the upload URL "opaque" and picks the host per account, so this cannot be
/// an exact list. It is a SUFFIX allowlist, and each suffix carries its leading dot so a
/// lookalike registered as `evilsharepoint.com` cannot match `.sharepoint.com`.
///
/// Every entry is a whole registrable domain Microsoft owns. The source for each, because a
/// list like this is only as good as the reason each line is on it:
///
/// * `.microsoftpersonalcontent.com` — the MODERN personal-account upload host, and the one
///   this list was missing (DRAGON-514: the owner's photos uploaded and their VIDEOS did not,
///   because only a file over [`SIMPLE_MAX_BYTES`] takes the session path this gate stands on).
///   Not in any Graph reference, which still shows only the shard below; it is what personal
///   accounts on the SharePoint-backed consumer stack actually return, reported with the
///   literal `uploadUrl` on Microsoft's own Q&A more than once
///   (`learn.microsoft.com/answers/questions/2150509`, `.../1691263`, both 2025) and served as
///   `my.microsoftpersonalcontent.com`.
/// * `.onedrive.com` — `https://api.onedrive.com/rup/…`, the OTHER address personal accounts
///   hand back in those same reports, and a documented consumer endpoint
///   (`learn.microsoft.com/sharepoint/required-urls-and-ports` lists `*.onedrive.com`).
/// * `.1drv.com` — the consumer shard family: `sn3302.up.1drv.com` is the Graph reference's own
///   createUploadSession example, and the endpoint list above names `*.files.1drv.com` beside
///   it. Widened from the `.up.1drv.com` this used to pin, because the SHARD label is exactly
///   the part Microsoft varies; the leading dot still refuses `evil1drv.com`.
/// * `.sharepoint.com` — the work/school host, `<tenant>-my.sharepoint.com`.
/// * `.svc.ms` — a OneDrive/SharePoint service domain Microsoft lists in its own network
///   endpoint table, included because these URLs are explicitly not contractual.
/// * `graph.microsoft.com` in case Graph ever hands back one of its own.
///
/// The national clouds' SharePoint domains (`.sharepoint.us`, `.sharepoint.cn`) are
/// deliberately NOT here: this app authenticates through the global `common` authority against
/// `graph.microsoft.com`, so an account in one of those deployments cannot sign in at all, and
/// a suffix nobody can reach is a widened allowlist bought for nothing.
///
/// A host outside the set is REFUSED rather than allowed with a warning. The chunks are the
/// user's capture, and an unrecognised destination is exactly the thing not to be relaxed
/// about; the host is logged so a legitimate new one can be added deliberately.
pub fn is_upload_session_host(host: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        ".1drv.com",
        ".microsoftpersonalcontent.com",
        ".onedrive.com",
        ".sharepoint.com",
        ".svc.ms",
    ];
    let host = host.to_ascii_lowercase();
    host == "graph.microsoft.com" || SUFFIXES.iter().any(|suffix| host.ends_with(suffix))
}

/// One driveItem, as much of it as we use.
#[derive(serde::Deserialize)]
struct DriveItem {
    id: Option<String>,
    name: Option<String>,
    #[serde(rename = "webUrl")]
    web_url: Option<String>,
    /// Present, and non-null, exactly when the item is a folder.
    folder: Option<serde_json::Value>,
}

/// A children listing.
#[derive(serde::Deserialize)]
struct ItemList {
    #[serde(default)]
    value: Vec<DriveItem>,
    /// The whole URL of the next page, present only while there is one.
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

/// The `POST /children` body for a new folder (DRAGON-506). Pure; unit-tested.
///
/// The empty `folder` object IS what makes the new item a folder; Graph's own example sends
/// exactly `"folder": { }`. `conflictBehavior` is `fail` rather than the `rename` an upload
/// uses; see [`OneDrive::create_folder`] for why the two differ.
pub fn create_folder_request(name: &str) -> String {
    serde_json::json!({
        "name": name,
        "folder": {},
        "@microsoft.graph.conflictBehavior": "fail",
    })
    .to_string()
}

/// Read the upload URL out of a `createUploadSession` reply. Pure; unit-tested.
///
/// The failure message must not echo the body: on the success path this field IS a
/// credential.
pub fn parse_upload_session(body: &str) -> Result<String, String> {
    json_field(body, "uploadUrl", "starting an upload")
}

/// Read an uploaded item. Pure; unit-tested.
pub fn parse_item(body: &str) -> Result<RemoteFile, String> {
    let parsed: DriveItem = serde_json::from_str(body)
        .map_err(|_| "OneDrive's reply after the upload could not be read.".to_string())?;
    let id = parsed
        .id
        .filter(|i| !i.is_empty())
        .ok_or_else(|| "OneDrive did not say where it put the file.".to_string())?;
    Ok(RemoteFile { web_url: parsed.web_url.filter(|u| !u.is_empty()), id })
}

/// Read the shared link out of a `createLink` reply. Pure; unit-tested.
pub fn parse_link(body: &str) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("link")?.get("webUrl")?.as_str().map(str::to_string))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "OneDrive's reply after sharing a file could not be read.".to_string())
}

/// Read the FOLDERS out of a children listing. Pure; unit-tested.
///
/// Filtered here rather than in the request, because OneDrive documents `$filter` as "not
/// available" on a children collection. An item is a folder when it carries a non-null
/// `folder` facet, which is the test that works on personal and business accounts alike.
pub fn parse_folders(body: &str) -> Result<Vec<RemoteFolder>, String> {
    let parsed: ItemList = serde_json::from_str(body)
        .map_err(|_| "OneDrive's folder list could not be read.".to_string())?;
    Ok(parsed
        .value
        .into_iter()
        .filter(|item| !matches!(item.folder, None | Some(serde_json::Value::Null)))
        .filter_map(|item| {
            let id = item.id.filter(|i| !i.is_empty())?;
            let name = item.name.filter(|n| !n.is_empty())?;
            Some(RemoteFolder { id, name })
        })
        .collect())
}

/// The URL of the listing's NEXT page, when Graph says there is one (DRAGON-506). Pure;
/// unit-tested.
///
/// `@odata.nextLink` is a complete URL, opaque by Graph's own description, and it is absent on
/// the last page. It comes off the network and decides where the next request goes, so a link
/// that is not Graph's is DROPPED (the listing simply ends) rather than followed: see
/// [`is_graph_url`].
pub fn next_link(body: &str) -> Option<String> {
    let next = serde_json::from_str::<ItemList>(body).ok()?.next_link.filter(|l| !l.is_empty())?;
    if !is_graph_url(&next) {
        log::warn!("cloud onedrive: ignored a folder listing's next page on an unexpected host");
        return None;
    }
    Some(next)
}

/// Whether a listing's next-page URL belongs to Graph. Pure; unit-tested.
///
/// The same reasoning as [`is_upload_session_host`], applied to the other value this provider
/// takes off the network and then makes a request to. The shared allowlist
/// (`super::api_hosts`) is the WRONG check: it is the union of every provider's hosts, so it
/// would happily let a Graph reply steer the next page at Google's API, with this account's
/// bearer token attached. Graph's own paging links are on `graph.microsoft.com`, exactly, which
/// is the one host this provider's API lives on.
pub fn is_graph_url(url: &str) -> bool {
    crate::cloud::http::host_of(url)
        .map(str::to_ascii_lowercase)
        .is_some_and(|host| HOSTS.contains(&host.as_str()))
}

/// Read the folder a `POST /children` just made. Pure; unit-tested.
///
/// The NAME falls back to what was asked for, for the reason Drive's does: the id is the only
/// field the caller cannot do without.
pub fn parse_created_folder(body: &str, requested: &str) -> Result<RemoteFolder, String> {
    let parsed: DriveItem = serde_json::from_str(body)
        .map_err(|_| "OneDrive's reply after making the folder could not be read.".to_string())?;
    let id = parsed
        .id
        .filter(|i| !i.is_empty())
        .ok_or_else(|| "OneDrive did not say where it made the folder.".to_string())?;
    let name = parsed.name.filter(|n| !n.is_empty()).unwrap_or_else(|| requested.to_string());
    Ok(RemoteFolder { id, name })
}

#[cfg(test)]
mod path_tests {
    use super::*;

    /// Graph's colon syntax, with the colons structural and the name encoded. Getting this
    /// wrong uploads to a path the user did not choose, or fails with a Graph parse error.
    ///
    /// **The rootless root** (DRAGON-498): with no folder chosen, an upload addresses the APP
    /// FOLDER (`special/approot`), never `/me/drive/root`, which is the user's whole drive and
    /// is not even reachable with the scope this app now asks for.
    #[test]
    fn the_item_path_uses_the_colon_syntax_under_the_app_folder() {
        assert_eq!(
            item_path(None, "shot.png", "content"),
            "/me/drive/special/approot:/shot.png:/content"
        );
        assert_eq!(
            item_path(Some("01ABCDEF"), "shot.png", "createUploadSession"),
            "/me/drive/items/01ABCDEF:/shot.png:/createUploadSession"
        );
        assert_eq!(
            item_path(Some(""), "a.png", "content"),
            "/me/drive/special/approot:/a.png:/content"
        );
        for path in [item_path(None, "a.png", "content"), children_path(None)] {
            assert!(!path.contains("/me/drive/root"), "{path} addresses the whole drive");
            // Microsoft documents the special-folder name as case sensitive, so the exact
            // spelling is the difference between the app folder and a 404.
            assert!(path.contains("approot"), "{path}");
            assert!(!path.contains("appRoot") && !path.contains("AppRoot"), "{path}");
        }
    }

    /// A listing addresses a COLLECTION, so it takes the plain `/children` form rather than
    /// the colon syntax that addresses an item by name.
    #[test]
    fn a_listing_is_the_app_folders_children() {
        assert_eq!(children_path(None), "/me/drive/special/approot/children");
        assert_eq!(children_path(Some("")), "/me/drive/special/approot/children");
        assert_eq!(children_path(Some("01DOCS")), "/me/drive/items/01DOCS/children");
        // No colon syntax here: `special/approot:/x:/children` addresses a named child, which
        // is a different request from "what is in this folder".
        assert!(!children_path(None).contains(":/"));
    }

    /// A name with a space, a colon or a slash must not change the path's structure.
    #[test]
    fn a_hostile_name_cannot_reshape_the_path() {
        let path = item_path(None, "Screen shot: 1/2.png", "content");
        assert_eq!(path, "/me/drive/special/approot:/Screen%20shot%3A%201%2F2.png:/content");
        // Exactly two structural colons remain, and no bare slash inside the name.
        assert_eq!(path.matches(":/").count(), 2);
        // A folder id is encoded too, so it cannot carry the path out of the app folder. The
        // DOTS survive (they are unreserved, and harmless on their own); the SEPARATORS do not,
        // which is what makes the whole thing one path segment Graph looks up as an item id.
        let escaped = item_path(Some("../../root"), "a.png", "content");
        assert_eq!(escaped, "/me/drive/items/..%2F..%2Froot:/a.png:/content");
        assert_eq!(
            escaped.matches('/').count(),
            item_path(Some("01ABCDEF"), "a.png", "content").matches('/').count(),
            "a hostile id added a path segment: {escaped}"
        );
    }

    /// The session request asks for a rename on collision, so a second capture in the same
    /// second is not lost.
    #[test]
    fn the_session_request_renames_on_collision() {
        let body = session_request("clip.mp4");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(parsed["item"]["name"], "clip.mp4");
        assert_eq!(parsed["item"]["@microsoft.graph.conflictBehavior"], "rename");
    }
}

#[cfg(test)]
mod upload_host_tests {
    use super::*;

    /// The hosts Microsoft documents for an upload session are accepted.
    #[test]
    fn a_real_upload_host_is_accepted() {
        for host in [
            "sn3302.up.1drv.com",
            "contoso-my.sharepoint.com",
            "contoso.sharepoint.com",
            "abc.svc.ms",
            "graph.microsoft.com",
            // Hostnames are case-insensitive.
            "SN3302.UP.1DRV.COM",
        ] {
            assert!(is_upload_session_host(host), "{host} must be accepted");
        }
    }

    /// **The DRAGON-514 case.** A personal account's videos cross [`SIMPLE_MAX_BYTES`] into
    /// the session path, and the host Microsoft hands back for one today is not the shard the
    /// Graph reference still shows. Each of these is a host a personal account has been
    /// reported to return; refusing any of them is the bug that made photos upload and videos
    /// fail.
    #[test]
    fn a_modern_personal_upload_host_is_accepted() {
        for host in [
            // The one the owner's account actually got.
            "my.microsoftpersonalcontent.com",
            // Same family, any shard label: the part Microsoft varies.
            "eu-my.microsoftpersonalcontent.com",
            "MY.MICROSOFTPERSONALCONTENT.COM",
            // `https://api.onedrive.com/rup/…`, the other reported personal address.
            "api.onedrive.com",
            // The consumer shard family, beyond the `up.` label this list used to pin.
            "sn3302.files.1drv.com",
            "dm2301.up.1drv.com",
        ] {
            assert!(is_upload_session_host(host), "{host} must be accepted");
        }
    }

    /// **The lookalike case.** Each suffix carries its leading dot, so a host that merely
    /// ends with the same letters is not a match. These are the shapes an attacker would
    /// register.
    #[test]
    fn a_lookalike_upload_host_is_refused() {
        for host in [
            "evilsharepoint.com",
            "notsvc.ms",
            "up.1drv.com.evil.test",
            "sharepoint.com.evil.test",
            "graph.microsoft.com.evil.test",
            "evil.test",
            "",
            // The bare registrable domains are not upload hosts either.
            "sharepoint.com",
            "1drv.com",
            // DRAGON-514's new families, in the shapes an attacker would register: the leading
            // dot is what separates a subdomain of Microsoft's from a domain merely ending in
            // the same letters.
            "evilmicrosoftpersonalcontent.com",
            "microsoftpersonalcontent.com",
            "my.microsoftpersonalcontent.com.evil.test",
            "evil1drv.com",
            "notonedrive.com",
            "onedrive.com",
            "api.onedrive.com.evil.test",
        ] {
            assert!(!is_upload_session_host(host), "{host} must be refused");
        }
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    /// The documented `createUploadSession` reply, with an invented URL.
    #[test]
    fn an_upload_session_reply_parses() {
        let body = r#"{
            "uploadUrl": "https://sn3302.up.1drv.com/up/fe6987415ace7X4e1eF866337",
            "expirationDateTime": "2026-08-02T09:21:55.523Z"
        }"#;
        let url = parse_upload_session(body).expect("parse");
        assert_eq!(url, "https://sn3302.up.1drv.com/up/fe6987415ace7X4e1eF866337");
        assert!(is_upload_session_host(host_of(&url).expect("a host")));
    }

    /// A reply with no URL is a failure, and the message must not echo the body: on the
    /// success path that field is a credential.
    #[test]
    fn a_session_reply_without_a_url_is_refused_quietly() {
        let err = parse_upload_session(r#"{"uploadUrl":"","other":"tempauth=SECRETVALUE"}"#)
            .expect_err("no url");
        assert!(!err.contains("SECRETVALUE"), "the reply leaked into the message: {err}");
        assert!(parse_upload_session("{}").is_err());
        assert!(parse_upload_session("not json").is_err());
    }

    /// A real driveItem reply shape, with invented ids.
    #[test]
    fn an_uploaded_item_parses() {
        let body = r#"{
            "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#items/$entity",
            "id": "01BYE5RZ6QN3ZWBTUFOFD3GSPGOHDJD36K",
            "name": "Screenshot 2026-08-02.png",
            "size": 118273,
            "webUrl": "https://contoso-my.sharepoint.com/personal/a_contoso_com/Documents/Screenshot.png",
            "file": {"mimeType": "image/png"}
        }"#;
        let item = parse_item(body).expect("parse");
        assert_eq!(item.id, "01BYE5RZ6QN3ZWBTUFOFD3GSPGOHDJD36K");
        assert!(item.web_url.is_some());
        assert!(parse_item(r#"{"name":"a.png"}"#).is_err(), "no id is a failed upload");
    }

    /// The documented `createLink` reply, personal-account form.
    #[test]
    fn a_share_link_reply_parses() {
        let body = r#"{
            "id": "123ABC",
            "roles": ["read"],
            "link": {
                "type": "view",
                "scope": "anonymous",
                "webUrl": "https://1drv.ms/A6913278E564460AA616C71B28AD6EB6"
            }
        }"#;
        assert_eq!(
            parse_link(body).expect("parse"),
            "https://1drv.ms/A6913278E564460AA616C71B28AD6EB6"
        );
        // The link has to be nested where Graph puts it; a flat webUrl is the ITEM's address,
        // not the share link, and returning it would hand the user a URL nobody else can open.
        assert!(parse_link(r#"{"webUrl":"https://example.test/x"}"#).is_err());
        assert!(parse_link(r#"{"link":{}}"#).is_err());
        assert!(parse_link("not json").is_err());
    }

    /// **The folder filter.** `$filter` is unavailable on this collection, so files are
    /// separated from folders here, by the `folder` facet.
    #[test]
    fn only_folders_come_out_of_a_children_listing() {
        let body = r#"{
            "value": [
                {"id": "01FILE", "name": "myfile.jpg", "size": 2048, "file": {"mimeType": "image/jpeg"}},
                {"id": "01DOCS", "name": "Documents", "folder": {"childCount": 4}},
                {"id": "01PICS", "name": "Photos", "folder": {"childCount": 203}},
                {"id": "01NULL", "name": "Odd", "folder": null},
                {"id": "", "name": "No id", "folder": {"childCount": 0}},
                {"id": "01NONAME", "folder": {"childCount": 0}}
            ]
        }"#;
        let folders = parse_folders(body).expect("parse");
        let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Documents", "Photos"]);
        assert_eq!(folders[0].id, "01DOCS");
        // An empty drive is a valid answer.
        assert!(parse_folders(r#"{"value":[]}"#).expect("parse").is_empty());
        assert!(parse_folders("{}").expect("parse").is_empty());
        assert!(parse_folders("not json").is_err());
    }
}

#[cfg(test)]
mod chunk_rule_tests {
    use super::*;

    /// **The 320 KiB rule, end to end.** Microsoft fails a badly sized upload when it is
    /// COMMITTED, so a wrong chunk size looks like a corrupt file at the end of a long
    /// upload rather than an error at the start.
    #[test]
    fn every_non_final_chunk_is_a_multiple_of_320_kib() {
        // A size that is deliberately not a multiple of anything.
        let total = 137 * 1024 * 1024 + 12_345;
        let spans = chunk_spans(total, CHUNK_BYTES);
        assert!(spans.len() > 10, "this should be a genuinely chunked upload");
        for (_, len) in &spans[..spans.len() - 1] {
            assert_eq!(*len % CHUNK_UNIT, 0, "a middle chunk broke the 320 KiB rule");
            assert!(*len <= 60 * 1024 * 1024, "a chunk exceeded the 60 MiB ceiling");
        }
        // The ranges cover the file exactly and are contiguous, which is what Graph checks.
        let (last_start, last_len) = spans[spans.len() - 1];
        assert_eq!(last_start + last_len, total);
        assert_eq!(content_range(last_start, last_len, total), format!("bytes {}-{}/{total}", last_start, total - 1));
    }

    /// The smallest file that reaches the chunked path is still one chunk, which is the
    /// boundary the compile-time assert above is about. Pinned at runtime too, because the
    /// assert proves the constants relate and this proves the SPLIT does. (DRAGON-490
    /// follow-up: this exact gap, a file just over the simple-upload cap that STILL completes
    /// in one session chunk, is why "will this report progress" cannot be predicted from
    /// `size` alone — see `cloud::child::run_cloud_upload`'s dynamic detection instead.)
    #[test]
    fn the_smallest_chunked_upload_is_a_single_chunk() {
        assert_eq!(chunk_spans(SIMPLE_MAX_BYTES + 1, CHUNK_BYTES).len(), 1);
        assert_eq!(chunk_spans(CHUNK_BYTES + 1, CHUNK_BYTES).len(), 2);
    }
}

#[cfg(test)]
mod folder_manager_tests {
    use super::*;

    /// The `POST /children` body: the empty `folder` object is what makes the new item a
    /// folder, and `fail` is the conflict behaviour, NOT the `rename` an upload uses. A folder
    /// is something the user just named, so a taken name is a question to put back to them
    /// rather than something to answer with "Screenshots 1".
    #[test]
    fn a_new_folder_asks_graph_to_refuse_a_taken_name() {
        let body = create_folder_request("Screenshots");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        assert_eq!(parsed["name"], serde_json::json!("Screenshots"));
        assert_eq!(parsed["folder"], serde_json::json!({}), "the facet is what makes it a folder");
        assert_eq!(
            parsed["@microsoft.graph.conflictBehavior"],
            serde_json::json!("fail"),
            "a folder must never be silently renamed"
        );
        // The two requests deliberately disagree: an upload keeps the capture, a folder keeps
        // the name.
        let upload: serde_json::Value =
            serde_json::from_str(&session_request("shot.png")).expect("valid json");
        assert_eq!(upload["item"]["@microsoft.graph.conflictBehavior"], serde_json::json!("rename"));
    }

    /// **A create posts at an ITEM, at both depths** (DRAGON-520).
    ///
    /// `/me/drive/items/{parent-item-id}/children` is the form Graph's createFolder reference
    /// documents; the special-folder collection beside it is a LISTING form, and posting there is
    /// what came back refused with "The cloud service could not create that folder". The two
    /// depths differ only in whose id it is: the app folder's, fetched once, or the subfolder's,
    /// which the listing already handed over.
    #[test]
    fn a_create_posts_at_an_item_at_every_depth() {
        // The TOP level, with the app folder's own item id resolved first.
        assert_eq!(
            create_children_path("01APPROOTID"),
            "/me/drive/items/01APPROOTID/children"
        );
        // One level deep, with the subfolder's id.
        assert_eq!(create_children_path("01SUBFOLDER"), "/me/drive/items/01SUBFOLDER/children");
        // Never the special-folder collection, which is the listing's form and not a create's.
        for path in [create_children_path("01APPROOTID"), create_children_path("01SUBFOLDER")] {
            assert!(!path.contains("special"), "{path} posts at the listing's collection");
            assert!(!path.contains("/me/drive/root"), "{path} addresses the whole drive");
        }
        // An id off the network cannot add a path segment.
        let hostile = create_children_path("../../root");
        assert_eq!(hostile, "/me/drive/items/..%2F..%2Froot/children");
        assert_eq!(hostile.matches('/').count(), create_children_path("01X").matches('/').count());
    }

    /// Where the app folder's id is read from, and what a reply that will not name it does
    /// (DRAGON-520).
    ///
    /// The GET is documented to answer the driveItem AND to create the special folder on an
    /// account that has never had one, which is why a brand new connection can make a folder
    /// before it has ever uploaded anything.
    #[test]
    fn the_app_folders_own_id_is_read_from_the_special_folder() {
        assert_eq!(
            approot_url(),
            "https://graph.microsoft.com/v1.0/me/drive/special/approot?$select=id"
        );
        // Microsoft documents the special-folder name as case sensitive.
        assert!(!approot_url().contains("appRoot") && !approot_url().contains("AppRoot"));
        let body = r#"{
            "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#drives('me')/special/$entity",
            "id": "01APPROOTID",
            "name": "Cosmic Capture Kit",
            "specialFolder": {"name": "approot"}
        }"#;
        assert_eq!(parse_root_id(body).as_deref(), Ok("01APPROOTID"));
        // A reply with no usable id is a SENTENCE the dialog shows, never a create somewhere
        // else. The unreadable case must not echo the body back at the user either.
        let refused = Err(ROOT_UNKNOWN.to_string());
        assert_eq!(parse_root_id(r#"{"id":""}"#), refused);
        assert_eq!(parse_root_id("{}"), refused);
        let unreadable = parse_root_id("<html>SECRET</html>").expect_err("unreadable");
        assert!(!unreadable.contains("SECRET"), "{unreadable}");
    }

    /// A delete names the item itself, so it is one request shape at every depth (DRAGON-520).
    /// The app folder is never a legal argument: the UI refuses that before this is reached.
    #[test]
    fn a_delete_addresses_the_item_at_every_depth() {
        assert_eq!(
            delete_url("01TOPLEVELFOLDER"),
            "https://graph.microsoft.com/v1.0/me/drive/items/01TOPLEVELFOLDER"
        );
        assert_eq!(
            delete_url("01DEEPFOLDER"),
            "https://graph.microsoft.com/v1.0/me/drive/items/01DEEPFOLDER"
        );
        let hostile = delete_url("../../root");
        assert_eq!(hostile, "https://graph.microsoft.com/v1.0/me/drive/items/..%2F..%2Froot");
        assert_eq!(hostile.matches('/').count(), delete_url("01X").matches('/').count());
    }

    /// A real children listing carrying `@odata.nextLink`, then its last page. The link is a
    /// WHOLE URL Graph builds, not a token to append.
    #[test]
    fn a_listing_follows_its_next_link_until_there_is_none() {
        let page_one = r#"{
            "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#items",
            "@odata.nextLink": "https://graph.microsoft.com/v1.0/me/drive/special/approot/children?$skiptoken=UGFnZTI",
            "value": [
                {"id": "01ABCFOLDER", "name": "Screenshots", "folder": {"childCount": 3}},
                {"id": "01ABCFILE", "name": "note.txt", "file": {"mimeType": "text/plain"}}
            ]
        }"#;
        assert_eq!(
            next_link(page_one).as_deref(),
            Some("https://graph.microsoft.com/v1.0/me/drive/special/approot/children?$skiptoken=UGFnZTI")
        );
        // The FILE in the same page is still filtered out, so paging only appends folders.
        let folders = parse_folders(page_one).expect("parse");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "Screenshots");
        let last = r#"{"value":[{"id":"01B","name":"Recordings","folder":{}}]}"#;
        assert_eq!(next_link(last), None, "the last page ends the listing");
        assert_eq!(next_link(r#"{"@odata.nextLink":"","value":[]}"#), None);
        assert_eq!(next_link("not json"), None);
    }

    /// **A next link is a request this app is about to make with the account's token on it.**
    /// A reply that points somewhere other than Graph is dropped, so a Graph answer can never
    /// steer the next page at another provider's API. The SHARED allowlist would not catch
    /// that: it is the union of every provider's hosts.
    #[test]
    fn a_next_link_off_graph_is_not_followed() {
        for good in [
            "https://graph.microsoft.com/v1.0/me/drive/items/01A/children?$skiptoken=x",
            "https://GRAPH.MICROSOFT.COM/v1.0/me/drive/root/children",
        ] {
            assert!(is_graph_url(good), "{good}");
        }
        for bad in [
            "https://www.googleapis.com/drive/v3/files",
            "https://graph.microsoft.com.evil.test/v1.0/me/drive/root/children",
            "https://evil.test/v1.0/me/drive/root/children",
            "not a url",
        ] {
            assert!(!is_graph_url(bad), "{bad}");
            let body = format!(r#"{{"@odata.nextLink":"{bad}","value":[]}}"#);
            assert_eq!(next_link(&body), None, "{bad} must not be followed");
        }
        // An upload session host is a DIFFERENT question and stays a different answer: a
        // per-account shard may take chunks, and must not take a paging request.
        assert!(is_upload_session_host("sn3302.up.1drv.com"));
        assert!(!is_graph_url("https://sn3302.up.1drv.com/v1.0/me/drive/root/children"));
    }

    /// A real `POST /children` reply. The id is required; the name falls back to what was asked
    /// for, so a reply missing it cannot cost the user the folder.
    #[test]
    fn a_created_folder_parses_and_falls_back_to_the_name_asked_for() {
        let body = r#"{
            "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#items/$entity",
            "id": "01NEWFOLDERABCDEF",
            "name": "Screenshots",
            "folder": {"childCount": 0},
            "webUrl": "https://onedrive.live.com/?id=01NEWFOLDERABCDEF"
        }"#;
        let made = parse_created_folder(body, "Screenshots").expect("parse");
        assert_eq!(
            made,
            RemoteFolder { id: "01NEWFOLDERABCDEF".to_string(), name: "Screenshots".to_string() }
        );
        assert_eq!(parse_created_folder(r#"{"id":"01N"}"#, "Asked").expect("parse").name, "Asked");
        assert!(parse_created_folder(r#"{"name":"Screenshots"}"#, "x").is_err());
        let err = parse_created_folder("<html>SECRET</html>", "x").expect_err("unreadable");
        assert!(!err.contains("SECRET"), "{err}");
    }
}
