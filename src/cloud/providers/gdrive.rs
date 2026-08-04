//! Google Drive, through the Drive API v3 (DRAGON-482, stage A2).
//!
//! Verified against `developers.google.com/workspace/drive/api/guides/manage-uploads`, the
//! `files` / `permissions` REST references, and
//! `developers.google.com/identity/protocols/oauth2/native-app` for revocation.
//!
//! # Two honest limits, both worth reading before changing anything here
//!
//! **1. `list_folders` can only ever see folders THIS APP created.** The account is
//! authorized with `drive.file`, which Google describes as access to files "that you open with
//! an app or that the user shares with an app while using the Google Picker API". A
//! `files.list` under that scope does not return the user's existing Drive folders, and no
//! query makes it. Widening to `.../auth/drive` would fix it and is refused: that is a
//! RESTRICTED scope, it grants read access to the user's entire Drive, and it needs a Google
//! verification review. A screenshot uploader should not hold it. The documented alternative,
//! the Google Picker, is a browser JavaScript component with no native equivalent, so it is
//! not reachable from here either. The listing is therefore honest but usually short, and the
//! settings page should present the folder choice as "where this app puts things".
//!
//! Since DRAGON-498 that limit is turned into the FEATURE the other two providers get from
//! their app folders. Drive has no `approot` equivalent (`drive.appdata` is the hidden
//! application-data folder, which the user cannot open, so it is the wrong tool for files a
//! user is meant to find), so this provider makes its own: [`GDrive::ensure_root_folder`]
//! finds or creates one ordinary folder named [`crate::cloud::APP_FOLDER`] in My Drive, stores
//! its id on the account, and the setup step lists INSIDE it. A folder this app created is a
//! folder `drive.file` can see, so the picker's contents and this app's reach become the same
//! set by construction.
//!
//! **The find must come first, and it must not depend on anything undocumented.** The lookup
//! query asks for the name, the folder mime type and `trashed = false`, and deliberately does
//! NOT add `'root' in parents`, even though that clause is valid v3 syntax and the folder is
//! created there. Google documents which files `drive.file` lets a `files.list` RETURN, but
//! nowhere documents that a parent relationship stays queryable under it; a clause that
//! silently matched nothing would make every connect create ANOTHER "Cosmic Capture Kit"
//! folder, which is the one failure this whole path exists to avoid. The scope already
//! restricts the result set to this app's own files, so the narrower clause buys nothing and
//! risks everything.
//!
//! **2. There are two upload paths, and the threshold is Google's, not ours.** Google caps
//! simple and multipart uploads at 5 MB, so a file up to [`MULTIPART_MAX_BYTES`] goes in one
//! `multipart/related` request and anything larger goes through a RESUMABLE session.
//!
//! The resumable path is why [`crate::cloud::http::CurlReq::capture_headers`] exists.
//! Starting a session is a POST whose reply carries the session URI in the `Location`
//! RESPONSE HEADER and nowhere else ("In addition, it includes a `Location` header that
//! specifies the resumable session URI"; the body is empty), and each accepted chunk reports
//! how far the server actually got in a `Range` header. Neither is in a body, so this is the
//! one provider that reads response headers at all.
//!
//! The session URI is a CREDENTIAL: anyone holding it can write to the user's drive for as
//! long as it lives. It is never logged, not even in part, and the header dump curl writes it
//! into is deleted on every path out of a send.
//!
//! Progress is driven by the server's own `Range` rather than by what we believe we sent, so
//! a partially accepted chunk resumes from where Google actually got to instead of leaving a
//! hole. That is also why the loop is written around a moving `offset` rather than a
//! precomputed span list.

use std::path::Path;

use super::{
    META_BUDGET, ProviderOps, RemoteFile, RemoteFolder, TempChunk, api, api_failure, bearer,
    content_range, file_size, json_field, progress_percent, remote_name, upload_budget,
};
use crate::cloud::accounts::CloudAccount;
use crate::cloud::http::{Body, CurlResponse, Method};

/// Every https host this provider reaches. Folded into [`super::api_hosts`].
pub(super) const HOSTS: &[&str] = &["www.googleapis.com", "oauth2.googleapis.com"];

/// The Drive metadata API.
const API: &str = "https://www.googleapis.com/drive/v3";

/// The Drive upload API. A different path prefix from [`API`], same host.
const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3/files";

/// Where a token is revoked. Google revokes the matching refresh token along with an access
/// token, so handing it the access token is enough.
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";

/// What Drive calls a folder.
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// The largest file sent as one `multipart/related` request, and Google's documented cap on
/// a simple or multipart upload: "5 MB or less". Above it a resumable session is mandatory.
pub const MULTIPART_MAX_BYTES: u64 = 5 * 1024 * 1024;

/// The status Google returns for "chunk accepted, send the next one".
///
/// Named because 308 is the one status in this file that is neither success nor failure, and
/// a bare `308` at the match arm reads like a redirect, which is what that code means
/// everywhere else in HTTP.
const RESUME_INCOMPLETE: u16 = 308;

/// The chunk size the resumable path uses.
///
/// Google: "Create chunks in multiples of 256 KB (256 x 1024 bytes) in size, except for the
/// final chunk that completes the upload. Keep the chunk size as large as possible so that the
/// upload is efficient." 8 MiB is 32 such units and a reasonable balance between round trips
/// and how much has to be re-sent after a failure.
pub const RESUMABLE_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// The unit Google requires a non-final chunk to be a multiple of.
pub const RESUMABLE_CHUNK_UNIT: u64 = 256 * 1024;

// A chunk size that is not a multiple of the unit is accepted for a while and then fails on
// commit, which reads as a corrupt upload rather than a bad constant.
const _: () = assert!(
    RESUMABLE_CHUNK_BYTES.is_multiple_of(RESUMABLE_CHUNK_UNIT),
    "DRAGON-482: a Drive resumable chunk must be a multiple of 256 KiB, or the upload fails \
     when it is committed rather than when it is sent"
);
const _: () = assert!(
    MULTIPART_MAX_BYTES < RESUMABLE_CHUNK_BYTES,
    "DRAGON-482: a file too big for one multipart request must be worth a resumable \
     session, or the session path pays three round trips to send a single chunk"
);

/// How many folders one listing asks for. Drive coerces anything above 1000 to 1000; 100 is
/// plenty for a destination picker and keeps the reply small.
const FOLDER_PAGE_SIZE: u32 = 100;

pub(super) struct GDrive;

impl ProviderOps for GDrive {
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
        let metadata = create_metadata(&name, acct.folder_id.as_deref());
        if size <= MULTIPART_MAX_BYTES {
            // One request, no per-chunk pause to check between: `should_cancel` is not
            // consulted here (see `providers::upload`'s doc for why).
            return multipart_upload(token, file, &metadata, &name, size, on_progress);
        }
        resumable_upload(token, file, &metadata, &name, size, on_progress, should_cancel)
    }

    fn create_share_link(&self, token: &str, file_id: &str) -> Result<String, String> {
        // Two calls, because Drive separates "who may see this" from "what is its address".
        let permission = bearer(
            api(Method::Post, &format!("{API}/files/{}/permissions", esc(file_id)), META_BUDGET)?,
            token,
        )
        .header("Content-Type", "application/json")
        .body(Body::Text(
            // `allowFileDiscovery: false` is what makes this a LINK, not a public listing:
            // with it true the file turns up in other people's Drive searches.
            r#"{"role":"reader","type":"anyone","allowFileDiscovery":false}"#.to_string(),
        ))
        .send()?;
        if !permission.is_success() {
            return Err(api_failure("gdrive", "share that file", &permission));
        }

        let url = format!("{API}/files/{}?fields=webViewLink", esc(file_id));
        let response = bearer(api(Method::Get, &url, META_BUDGET)?, token).send()?;
        if !response.is_success() {
            return Err(api_failure("gdrive", "read that file's link", &response));
        }
        json_field(&response.text(), "webViewLink", "sharing a file")
    }

    /// Find the account's [`crate::cloud::APP_FOLDER`] folder, or create it (DRAGON-498).
    ///
    /// The FIND comes first and its failure is fatal, because the alternative to knowing is
    /// creating, and creating when one already exists leaves the user with two folders of the
    /// same name and their captures split between them. A reconnect therefore costs one extra
    /// `files.list` and changes nothing.
    fn ensure_root_folder(&self, token: &str) -> Result<Option<RemoteFolder>, String> {
        let name = crate::cloud::APP_FOLDER;
        let url = format!(
            "{API}/files?q={}&fields=files(id,name,mimeType)&pageSize={FOLDER_PAGE_SIZE}\
             &orderBy=createdTime",
            esc(&root_query(name))
        );
        let found = bearer(api(Method::Get, &url, META_BUDGET)?, token).send()?;
        if !found.is_success() {
            return Err(api_failure("gdrive", "find this app's folder", &found));
        }
        if let Some(id) = find_root_folder(&found.text(), name)? {
            log::debug!("cloud gdrive: this account already has its own folder");
            return Ok(Some(RemoteFolder { id, name: name.to_string() }));
        }
        let created = bearer(api(Method::Post, &format!("{API}/files?fields=id,name"), META_BUDGET)?, token)
            .header("Content-Type", "application/json")
            .body(Body::Text(root_metadata(name)))
            // Not idempotent: a retry after a create that actually landed is the duplicate
            // folder this whole function exists to prevent.
            .no_retry()
            .send()?;
        if !created.is_success() {
            return Err(api_failure("gdrive", "create this app's folder", &created));
        }
        let id = super::json_field(&created.text(), "id", "creating this app's folder")?;
        log::debug!("cloud gdrive: created this account's own folder");
        Ok(Some(RemoteFolder { id, name: name.to_string() }))
    }

    /// Every folder inside `parent`, following `nextPageToken` to the end (DRAGON-506).
    ///
    /// **Drive cannot report a missing parent here.** `files.list` with a `'<id>' in parents`
    /// clause answers a folder that no longer exists with an empty list and a 200, not a 404,
    /// so a level deleted in the user's browser reads exactly like a level with nothing in it.
    /// That is left as it is rather than papered over: distinguishing the two would mean an
    /// extra `files.get` on the parent for EVERY listing, and the breadcrumb above the list is
    /// already the way back out of an empty level. The other two providers do report it, and
    /// [`super::is_missing_folder`] carries their answer.
    fn list_folders(&self, token: &str, parent: Option<&str>) -> Result<Vec<RemoteFolder>, String> {
        let query = folder_query(parent);
        let mut folders: Vec<RemoteFolder> = Vec::new();
        let mut page_token: Option<String> = None;
        let mut pages = 0u32;
        while super::may_fetch_another_page(pages) {
            let mut url = format!(
                "{API}/files?q={}&fields=nextPageToken,files(id,name)\
                 &pageSize={FOLDER_PAGE_SIZE}&orderBy=name",
                esc(&query)
            );
            if let Some(token) = page_token.as_deref() {
                url.push_str(&format!("&pageToken={}", esc(token)));
            }
            let response = bearer(api(Method::Get, &url, META_BUDGET)?, token).send()?;
            if !response.is_success() {
                return Err(api_failure("gdrive", "list folders", &response));
            }
            let body = response.text();
            folders.extend(parse_folders(&body)?);
            pages += 1;
            page_token = next_page_token(&body);
            if page_token.is_none() {
                return Ok(folders);
            }
        }
        log::warn!("cloud gdrive: stopped following a folder listing after {pages} pages");
        Ok(folders)
    }

    /// One `files.create` with the folder mime type, parented at the level being viewed.
    ///
    /// **The parent is always a real folder id, including at the top level** (DRAGON-520). Drive
    /// has no root ALIAS this app may use: with no `parents` at all the new folder lands at the
    /// top of My Drive, which is a 200 with an id, outside the app folder, and invisible to the
    /// `drive.file` listing that re-reads the level a moment later. So the app folder is resolved
    /// first, through the same [`GDrive::ensure_root_folder`] a listing goes through, and the
    /// create cannot be issued at all until it is known. See [`super::create_parent_with`].
    ///
    /// **Drive allows two folders with one name in the same parent**, unlike the other two, and
    /// there is no request flag that refuses. The picker checks the level's own listing before
    /// it gets here, which is what actually stops a second "Screenshots"; this is documented
    /// rather than worked around, because the alternative (a `files.list` per create) would
    /// still be a race and would cost a round trip on every folder anyone makes.
    fn create_folder(
        &self,
        token: &str,
        parent: Option<&str>,
        name: &str,
    ) -> Result<RemoteFolder, String> {
        let parent = super::create_parent_with(
            parent,
            || Ok(self.ensure_root_folder(token)?.map(|root| root.id)),
            ROOT_UNKNOWN,
        )?;
        let response = bearer(api(Method::Post, &create_url(), META_BUDGET)?, token)
            .header("Content-Type", "application/json")
            .body(Body::Text(folder_metadata(name, &parent)))
            // Not idempotent: a retry after a create that actually landed is a second
            // folder of the same name, which is the thing this whole path avoids.
            .no_retry()
            .send()?;
        if !response.is_success() {
            return Err(api_failure("gdrive", "create that folder", &response));
        }
        parse_created_folder(&response.text(), name)
    }

    fn delete_file(&self, token: &str, file_id: &str) -> Result<(), String> {
        let response = bearer(api(Method::Delete, &delete_url(file_id), META_BUDGET)?, token).send()?;
        if !response.is_success() {
            return Err(api_failure("gdrive", "delete that file", &response));
        }
        Ok(())
    }

    /// `files.delete`, which Drive documents as PERMANENT: it "permanently deletes a file owned
    /// by the user without moving it to the trash", and deleting a folder takes its contents
    /// with it. `ProviderCaps::delete_recoverable` is false for exactly this reason, and the
    /// confirmation the user sees says so before this is ever reached.
    ///
    /// One id, at every depth: a folder listed at the top level and one listed three levels down
    /// are both ordinary Drive files, so there is no root-level special case here of the kind
    /// [`GDrive::create_folder`] needs (DRAGON-520). The app folder itself is refused in the UI
    /// and never reaches this.
    fn delete_folder(&self, token: &str, folder_id: &str) -> Result<(), String> {
        let response = bearer(api(Method::Delete, &delete_url(folder_id), META_BUDGET)?, token).send()?;
        if !response.is_success() {
            return Err(api_failure("gdrive", "delete that folder", &response));
        }
        Ok(())
    }

    fn revoke(&self, token: &str) -> Result<(), String> {
        // "The token can be an access token or a refresh token. If the token is an access
        // token and it has a corresponding refresh token, the refresh token will also be
        // revoked." So one call with the access token drops the whole grant.
        let response = api(Method::Post, REVOKE_URL, META_BUDGET)?
            .form_field("token", token)
            .no_retry()
            .send()?;
        if !response.is_success() {
            return Err(api_failure("gdrive", "sign this account out", &response));
        }
        Ok(())
    }
}

/// One `multipart/related` request carrying the metadata AND the bytes.
///
/// One request, so the file is never briefly present under Drive's placeholder name. The
/// alternative (a media upload followed by a rename) leaves exactly that window, and leaves a
/// stray "Untitled" file behind if the second call fails.
fn multipart_upload(
    token: &str,
    file: &Path,
    metadata: &str,
    name: &str,
    size: u64,
    on_progress: &mut dyn FnMut(u8),
) -> Result<RemoteFile, String> {
    let boundary = multipart_boundary()?;
    let envelope = write_multipart(file, metadata, &boundary, content_type_for(name))?;

    let url = format!("{UPLOAD_API}?uploadType=multipart&fields=id,name,webViewLink");
    let response = bearer(api(Method::Post, &url, upload_budget(size))?, token)
        .header("Content-Type", &format!("multipart/related; boundary={boundary}"))
        .body(Body::File(envelope.path().to_path_buf()))
        // Not idempotent: a retried upload that actually succeeded the first time leaves the
        // user with two copies.
        .no_retry()
        .send()?;
    if !response.is_success() {
        return Err(api_failure("gdrive", "upload that file", &response));
    }
    let uploaded = parse_file(&response.text())?;
    on_progress(100);
    log::debug!("cloud gdrive: uploaded a file of {size} bytes in one request");
    Ok(uploaded)
}

/// Start a resumable session, then feed it chunks until Drive commits the file.
///
/// The loop is driven by a moving `offset` that the SERVER decides, not by a precomputed
/// span list: after each `308` Google reports what it actually holds in a `Range` header, and
/// resuming from anywhere else would leave a hole in the file or re-send bytes it already
/// has.
fn resumable_upload(
    token: &str,
    file: &Path,
    metadata: &str,
    name: &str,
    size: u64,
    on_progress: &mut dyn FnMut(u8),
    should_cancel: &dyn Fn() -> bool,
) -> Result<RemoteFile, String> {
    let session = begin_session(token, metadata, name, size)?;
    log::debug!("cloud gdrive: uploading {size} bytes in a resumable session");

    let mut offset = 0u64;
    let mut stalled = 0u32;
    while offset < size {
        // Checked BETWEEN chunks (DRAGON-490), the natural pause: the loop already stops
        // here between one request and the next.
        // Nothing to tell Google: an abandoned resumable session expires on its own (about a
        // week), holds no file the user can see, and costs no quota, so there is no cleanup
        // call worth making here (DRAGON-495; OneDrive is the one provider that documents a
        // session DELETE, and does it).
        if should_cancel() {
            return Err(super::canceled_err());
        }
        let len = RESUMABLE_CHUNK_BYTES.min(size - offset);
        let chunk = TempChunk::write(file, offset, len)?;
        let response = put_chunk(&session, token, &chunk, offset, len, size)?;

        // The LAST chunk answers with the committed file, which is the only place its id
        // comes from.
        if response.is_success() {
            let uploaded = parse_file(&response.text())?;
            on_progress(100);
            log::debug!("cloud gdrive: finished a {size} byte resumable upload");
            return Ok(uploaded);
        }
        if response.status != RESUME_INCOMPLETE {
            return Err(api_failure("gdrive", "upload part of that file", &response));
        }

        // Believe the server over ourselves. No `Range` at all means it holds nothing yet,
        // which is a legitimate answer to the very first chunk.
        let confirmed = response.header("Range").and_then(parse_range_end);
        let next = match confirmed {
            Some(end) => end.saturating_add(1).min(size),
            None => offset,
        };
        if next <= offset {
            // Nothing advanced. Bounded rather than trusted: a server that keeps answering
            // "I have nothing" would otherwise spin here forever, and nothing in this app is
            // allowed to wait unboundedly.
            stalled += 1;
            if stalled >= MAX_STALLED_ROUNDS {
                log::warn!("cloud gdrive: the upload stopped advancing at {stalled} rounds");
                return Err("Google Drive stopped accepting this upload part way through. \
                            Try again."
                    .to_string());
            }
        } else {
            stalled = 0;
            offset = next;
            on_progress(progress_percent(offset, size));
        }
    }
    // Every byte was acknowledged and Drive never committed the file. It does not do this,
    // but saying so beats returning a file id we do not have.
    Err("Google Drive accepted the whole file but did not confirm it was saved.".to_string())
}

/// How many times in a row a chunk may fail to advance the offset before giving up.
const MAX_STALLED_ROUNDS: u32 = 3;

const _: () = assert!(
    MAX_STALLED_ROUNDS >= 2 && MAX_STALLED_ROUNDS <= 5,
    "DRAGON-482: one unlucky answer must not end an upload, and a session that never \
     advances must not grind on forever"
);

/// Open a resumable session and return its URI.
///
/// The URI is a CREDENTIAL (it authorizes writing to the user's drive) and is never logged.
/// `X-Upload-Content-Length` lets Drive reject an over-quota upload before a byte is sent
/// rather than at the end.
fn begin_session(token: &str, metadata: &str, name: &str, size: u64) -> Result<String, String> {
    let url = format!("{UPLOAD_API}?uploadType=resumable&fields=id,name,webViewLink");
    let response = bearer(api(Method::Post, &url, META_BUDGET)?, token)
        .header("Content-Type", "application/json; charset=UTF-8")
        .header("X-Upload-Content-Type", content_type_for(name))
        .header("X-Upload-Content-Length", &size.to_string())
        .body(Body::Text(metadata.to_string()))
        // The whole point: the session URI comes back as a header, not in the body.
        .capture_headers()
        .no_retry()
        .send()?;
    if !response.is_success() {
        return Err(api_failure("gdrive", "start that upload", &response));
    }
    let session = response
        .header("Location")
        .filter(|uri| !uri.is_empty())
        .ok_or_else(|| "Google Drive did not return an address to upload to.".to_string())?;
    // The session URI comes from the network, so it is checked against GOOGLE'S OWN hosts
    // before a byte of the user's capture is sent to it (DRAGON-482). `api()` would check it
    // against the UNION of every provider's hosts, which would let a compromised or
    // misconfigured Drive response steer the upload at Microsoft's or Dropbox's API instead.
    let host = crate::cloud::http::host_of(session)
        .ok_or_else(|| "Google Drive returned an upload address this app cannot use.".to_string())?;
    if !is_upload_session_host(host) {
        log::warn!("cloud gdrive: refused an upload session on the unexpected host {host}");
        return Err("Google Drive returned an upload address this app does not recognise."
            .to_string());
    }
    Ok(session.to_string())
}

/// Whether a host may receive resumable session chunks. Pure; unit-tested.
///
/// The mirror of [`super::onedrive::is_upload_session_host`], and separate from it on purpose:
/// a session URI is a CAPABILITY (its `upload_id` authorizes writing to the user's drive), so
/// the question is always "does this belong to the provider that issued it", never "is this
/// somewhere the app talks to". Checking against the shared allowlist would answer the second
/// question and accept a Drive session URI pointing at `graph.microsoft.com`.
///
/// Google's resumable `Location` is on `www.googleapis.com` in every documented example. The
/// `.googleapis.com` suffix is allowed on top of the exact hosts because Google routes some
/// upload traffic through per-service subdomains and does not call the URI contractual; the
/// leading dot is what stops a lookalike registered as `evilgoogleapis.com` from matching.
pub fn is_upload_session_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    HOSTS.iter().any(|h| *h == host) || host.ends_with(".googleapis.com")
}

/// Send one chunk, with one retry when repeating could help.
///
/// Safe to repeat, unlike the one-shot uploads in this file: a chunk is addressed by its
/// `Content-Range`, so re-sending the same range writes the same bytes to the same place, and
/// Google answers a range it already holds with a `308` naming what it has.
fn put_chunk(
    session: &str,
    token: &str,
    chunk: &TempChunk,
    offset: u64,
    len: u64,
    total: u64,
) -> Result<CurlResponse, String> {
    let attempt = || -> Result<CurlResponse, String> {
        // Authorization IS sent here, unlike Microsoft's upload session, which explicitly
        // refuses it. Google's reference omits the header from its example but never says to
        // leave it out, and the session URI is on an allowlisted host we already send this
        // token to, so including it is the safer of the two readings.
        bearer(api(Method::Put, session, upload_budget(len))?, token)
            .header("Content-Range", &content_range(offset, len, total))
            .body(Body::File(chunk.path().to_path_buf()))
            // `Range` says how much Drive actually took.
            .capture_headers()
            .no_retry()
            .send()
    };
    let first = attempt()?;
    if first.status == 0 || first.status >= 500 {
        log::debug!("cloud gdrive: a chunk came back {}; sending it once more", first.status);
        return attempt();
    }
    Ok(first)
}

/// The last byte Drive confirms it holds, from a `308`'s `Range` header. Pure; unit-tested.
///
/// `Range: bytes=0-262143` means "I have through byte 262143", so the next chunk starts at
/// 262144. Google documents this as an INCLUSIVE upper bound, and reading it as exclusive
/// would drop one byte per chunk, which corrupts the file without failing the upload.
pub fn parse_range_end(value: &str) -> Option<u64> {
    let range = value.trim().strip_prefix("bytes=")?;
    let (_first, last) = range.rsplit_once('-')?;
    last.trim().parse::<u64>().ok()
}

/// The media type for a capture, from its name. Pure; unit-tested.
///
/// Drive shows a thumbnail and picks a viewer from this, so a screenshot arriving as
/// `application/octet-stream` is a file the user has to download to look at. Only the types
/// this app can actually produce are listed; anything else is the honest fallback rather
/// than a guess.
pub fn content_type_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "gpm" | "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Percent-encode a value for a URL path or query. Reuses the OAuth encoder so there is one
/// implementation in `cloud/`.
fn esc(value: &str) -> String {
    crate::cloud::oauth::percent_encode(value)
}

/// The `files.create` metadata document. Pure; unit-tested.
///
/// `parents` is omitted entirely when there is no folder, which Drive documents as "the file
/// is placed directly in the user's My Drive folder". An empty array would NOT mean that.
pub fn create_metadata(name: &str, folder_id: Option<&str>) -> String {
    let mut doc = serde_json::Map::new();
    doc.insert("name".to_string(), serde_json::Value::String(name.to_string()));
    if let Some(folder) = folder_id.filter(|f| !f.is_empty()) {
        doc.insert(
            "parents".to_string(),
            serde_json::Value::Array(vec![serde_json::Value::String(folder.to_string())]),
        );
    }
    serde_json::Value::Object(doc).to_string()
}

/// The `q` for a folder listing. Pure; unit-tested.
///
/// `'root'` is Drive's alias for My Drive, so a `None` parent lists the top level. `trashed
/// = false` matters: without it a deleted folder is still offered as a destination, and
/// uploading into it puts the file in the trash.
///
/// The parent is interpolated into a SINGLE-QUOTED Drive query literal, so its own quotes and
/// backslashes are escaped the way Drive's query grammar specifies (`\'` and `\\`). A folder
/// id is minted by Drive and never contains either, but this string is built from a value that
/// reached us over the network and is stored in a file the user can edit, and a query that can
/// be broken out of is a query that eventually is.
pub fn folder_query(parent: Option<&str>) -> String {
    let parent = escape_query_literal(parent.filter(|p| !p.is_empty()).unwrap_or("root"));
    format!("mimeType = '{FOLDER_MIME}' and '{parent}' in parents and trashed = false")
}

/// The `q` that LOOKS FOR this app's own folder. Pure; unit-tested.
///
/// Three clauses and no more: the exact name, the folder mime type, and not in the trash. See
/// the module doc for why `'root' in parents` is deliberately absent, and why adding it would
/// risk a duplicate folder per connect rather than a tighter search.
///
/// `trashed = false` is not optional here either. A user who deleted the folder should get a
/// fresh one, not an id that puts every future upload straight into their trash.
pub fn root_query(name: &str) -> String {
    let name = escape_query_literal(name);
    format!("name = '{name}' and mimeType = '{FOLDER_MIME}' and trashed = false")
}

/// The `files.create` body for this app's own folder. Pure; unit-tested.
///
/// `parents: ["root"]` puts it in My Drive, where the user can actually find it. `root` is
/// Drive's documented alias for that folder ("The alias `root` can be used to refer to the root
/// folder anywhere a file ID is provided"), so no lookup is needed to name it. Omitting
/// `parents` would land it in the same place by default; it is stated because this folder's
/// LOCATION is the point of it, and a default is a worse thing to depend on than a statement.
pub fn root_metadata(name: &str) -> String {
    serde_json::json!({
        "name": name,
        "mimeType": FOLDER_MIME,
        "parents": ["root"],
    })
    .to_string()
}

/// Which existing folder is this app's own, from a [`root_query`] listing. Pure; unit-tested.
///
/// `Ok(Some(id))` reuses it, `Ok(None)` means create one, and `Err` means the reply could not
/// be read, which is NOT the same as "there isn't one": creating on an unreadable answer is how
/// a user ends up with two folders of the same name.
///
/// **The name is re-checked here.** Drive's `name = '...'` is an exact match, so this should
/// never filter anything out, but the id it returns decides where every future upload lands and
/// it comes off the network; matching again locally costs nothing.
///
/// **The FIRST match wins, and the listing is ordered by `createdTime`**, so a user who somehow
/// has two gets the older one every time rather than a different one per launch. Duplicates
/// are not this function's to clean up: deleting a folder with the user's captures in it is not
/// a decision an uploader gets to make quietly.
pub fn find_root_folder(body: &str, name: &str) -> Result<Option<String>, String> {
    let parsed: FileList = serde_json::from_str(body)
        .map_err(|_| "Google Drive's reply about this app's folder could not be read.".to_string())?;
    Ok(parsed.files.into_iter().find_map(|f| {
        (f.name.as_deref() == Some(name)).then(|| f.id.filter(|id| !id.is_empty()))?
    }))
}

/// Escape a value for a single-quoted Drive query literal. Pure; unit-tested.
///
/// Drive's search grammar documents exactly two escapes inside a string literal: `\'` for a
/// quote and `\\` for a backslash. Backslash goes FIRST, or an escaped quote's own backslash
/// would be escaped again.
pub fn escape_query_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// A random multipart boundary.
///
/// Random rather than fixed, because a boundary that appears in the file's bytes would split
/// the upload in the wrong place. 128 bits from the OS CSPRNG makes that impossible in
/// practice, and unlike a fixed string it cannot be provoked on purpose.
fn multipart_boundary() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| "This computer's random number source is unavailable.".to_string())?;
    let mut out = String::from("cck");
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

/// The bytes that precede the file in a `multipart/related` body. Pure; unit-tested.
pub fn multipart_prologue(metadata: &str, boundary: &str, mime: &str) -> String {
    format!(
        "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n\
         --{boundary}\r\nContent-Type: {mime}\r\n\r\n"
    )
}

/// The bytes that follow it. Pure; unit-tested.
pub fn multipart_epilogue(boundary: &str) -> String {
    format!("\r\n--{boundary}--\r\n")
}

/// Build the whole `multipart/related` body on disk.
///
/// On disk rather than in memory because the transport sends a file body with
/// `--data-binary @path`, which keeps the payload out of the stdin pipe the config travels
/// down. Costs one copy of a file that is at most [`MULTIPART_MAX_BYTES`].
fn write_multipart(
    file: &Path,
    metadata: &str,
    boundary: &str,
    mime: &str,
) -> Result<TempChunk, String> {
    use std::io::Write as _;
    let (envelope, mut out) = TempChunk::create()?;
    out.write_all(multipart_prologue(metadata, boundary, mime).as_bytes())
        .map_err(|e| format!("The upload could not be prepared: {e}"))?;
    let mut input = std::fs::File::open(file).map_err(|e| {
        format!("The file to upload could not be opened ({}): {e}", crate::diag::path_shape(file))
    })?;
    std::io::copy(&mut input, &mut out)
        .map_err(|e| format!("The file to upload could not be copied: {e}"))?;
    out.write_all(multipart_epilogue(boundary).as_bytes())
        .map_err(|e| format!("The upload could not be prepared: {e}"))?;
    out.flush().map_err(|e| format!("The upload could not be prepared: {e}"))?;
    Ok(envelope)
}

/// The `files.create` body for a folder the USER asked for (DRAGON-506). Pure; unit-tested.
///
/// [`root_metadata`]'s shape with the parent the picker is looking at, rather than the hardcoded
/// `root` that one uses.
///
/// **`parent` is a required id, not an option** (DRAGON-520). It used to be `Option<&str>`, and
/// the `None` arm omitted `parents`, which is Drive's documented "put it in My Drive": the folder
/// was made outside the app folder, the scoped listing could not see it, and the user was shown
/// nothing at all. The caller resolves the app folder first
/// ([`super::create_parent_with`]) so the wrong-place create is not a state this function can be
/// asked for.
pub fn folder_metadata(name: &str, parent: &str) -> String {
    serde_json::json!({
        "name": name,
        "mimeType": FOLDER_MIME,
        "parents": [parent],
    })
    .to_string()
}

/// What a create says when this app's own Drive folder could not be found or made. User copy, so
/// it names WHAT could not happen as well as why.
const ROOT_UNKNOWN: &str = "Google Drive could not say where this app's own folder is, so the new \
                            folder had nowhere to go.";

/// Where a `files.create` is POSTed. Pure; unit-tested.
///
/// `fields=id,name` so the reply carries what [`parse_created_folder`] needs and nothing else;
/// Drive answers a bare create with the id alone.
pub fn create_url() -> String {
    format!("{API}/files?fields=id,name")
}

/// Where a `files.delete` is sent, for a file or a folder. Pure; unit-tested.
///
/// One builder for both, because Drive makes no distinction: a folder IS a file with the folder
/// mime type, at any depth. The id is percent-encoded, so an id off the network cannot add a path
/// segment.
pub fn delete_url(id: &str) -> String {
    format!("{API}/files/{}", esc(id))
}

/// One `File` resource, as much of it as we use.
#[derive(serde::Deserialize)]
struct DriveFile {
    id: Option<String>,
    name: Option<String>,
    #[serde(rename = "webViewLink")]
    web_view_link: Option<String>,
}

/// The `files.list` reply.
#[derive(serde::Deserialize)]
struct FileList {
    #[serde(default)]
    files: Vec<DriveFile>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

/// Read an uploaded file out of a Drive reply. Pure; unit-tested.
pub fn parse_file(body: &str) -> Result<RemoteFile, String> {
    let parsed: DriveFile = serde_json::from_str(body)
        .map_err(|_| "Google Drive's reply after the upload could not be read.".to_string())?;
    let id = parsed
        .id
        .filter(|i| !i.is_empty())
        .ok_or_else(|| "Google Drive did not say where it put the file.".to_string())?;
    Ok(RemoteFile { web_url: parsed.web_view_link.filter(|u| !u.is_empty()), id })
}

/// Read a folder listing. Pure; unit-tested.
///
/// An entry missing an id or a name is SKIPPED rather than failing the listing: one odd row
/// should not cost the user the folder picker.
pub fn parse_folders(body: &str) -> Result<Vec<RemoteFolder>, String> {
    let parsed: FileList = serde_json::from_str(body)
        .map_err(|_| "Google Drive's folder list could not be read.".to_string())?;
    Ok(parsed
        .files
        .into_iter()
        .filter_map(|f| {
            let id = f.id.filter(|i| !i.is_empty())?;
            let name = f.name.filter(|n| !n.is_empty())?;
            Some(RemoteFolder { id, name })
        })
        .collect())
}

/// The cursor for the NEXT page of a listing, when Drive says there is one (DRAGON-506). Pure;
/// unit-tested.
///
/// `nextPageToken` is absent on the last page, which is how the loop knows to stop. An empty
/// token is treated as absent: Drive does not send one, and a loop that took it would ask for
/// the first page again forever.
///
/// A body that will not parse answers `None` rather than erroring. It cannot be reached
/// (`parse_folders` reads the same body first and reports the failure), and answering "no more
/// pages" is the safe half of a wrong guess either way.
pub fn next_page_token(body: &str) -> Option<String> {
    serde_json::from_str::<FileList>(body).ok()?.next_page_token.filter(|t| !t.is_empty())
}

/// Read the folder a `files.create` just made. Pure; unit-tested.
///
/// The NAME falls back to what was asked for. Drive echoes it, but the id is the only field the
/// caller cannot do without, and a reply that carried an id and no name would otherwise cost the
/// user the folder they just made.
pub fn parse_created_folder(body: &str, requested: &str) -> Result<RemoteFolder, String> {
    let parsed: DriveFile = serde_json::from_str(body)
        .map_err(|_| "Google Drive's reply after making the folder could not be read.".to_string())?;
    let id = parsed
        .id
        .filter(|i| !i.is_empty())
        .ok_or_else(|| "Google Drive did not say where it made the folder.".to_string())?;
    let name = parsed.name.filter(|n| !n.is_empty()).unwrap_or_else(|| requested.to_string());
    Ok(RemoteFolder { id, name })
}

#[cfg(test)]
mod upload_shape_tests {
    use super::super::chunk_spans;
    use super::*;

    /// The metadata is what decides the file's name and where it lands. `parents` is OMITTED
    /// with no folder, because an empty array is not the same as "My Drive".
    #[test]
    fn the_create_metadata_names_the_file_and_its_folder() {
        assert_eq!(create_metadata("shot.png", Some("1AbC")), r#"{"name":"shot.png","parents":["1AbC"]}"#);
        assert_eq!(create_metadata("shot.png", None), r#"{"name":"shot.png"}"#);
        assert_eq!(create_metadata("shot.png", Some("")), r#"{"name":"shot.png"}"#);
    }

    /// A name with a quote or a backslash would break a hand-built JSON document. It is
    /// serialized, not interpolated, so it cannot.
    #[test]
    fn a_hostile_file_name_cannot_break_the_metadata() {
        let doc = create_metadata(r#"a"b\c.png"#, None);
        assert_eq!(doc, r#"{"name":"a\"b\\c.png"}"#);
        let back: serde_json::Value = serde_json::from_str(&doc).expect("still valid JSON");
        assert_eq!(back["name"], r#"a"b\c.png"#);
    }

    /// The envelope is exactly the `multipart/related` shape Drive documents: metadata part,
    /// media part, closing boundary.
    #[test]
    fn the_multipart_envelope_has_both_parts() {
        let prologue = multipart_prologue(r#"{"name":"a.png"}"#, "BOUND", "image/png");
        assert_eq!(
            prologue,
            "--BOUND\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{\"name\":\"a.png\"}\r\n\
             --BOUND\r\nContent-Type: image/png\r\n\r\n"
        );
        assert_eq!(multipart_epilogue("BOUND"), "\r\n--BOUND--\r\n");
    }

    /// Drive picks a viewer and a thumbnail from the media type, so a screenshot arriving as
    /// a generic byte stream is one the user has to download to look at.
    #[test]
    fn a_captures_media_type_is_named() {
        assert_eq!(content_type_for("shot.png"), "image/png");
        assert_eq!(content_type_for("shot.PNG"), "image/png", "an extension is case-insensitive");
        assert_eq!(content_type_for("clip.mp4"), "video/mp4");
        assert_eq!(content_type_for("clip.webm"), "video/webm");
        assert_eq!(content_type_for("photo.jpeg"), "image/jpeg");
        // Anything we do not produce is the honest fallback rather than a guess.
        assert_eq!(content_type_for("archive.zip"), "application/octet-stream");
        assert_eq!(content_type_for("no-extension"), "application/octet-stream");
        assert_eq!(content_type_for(""), "application/octet-stream");
        // A dot in the directory part cannot be mistaken for an extension: only the name is
        // ever passed, and the LAST dot wins.
        assert_eq!(content_type_for("my.screenshot.png"), "image/png");
    }

    /// A boundary is random and long, so it cannot occur in the file being wrapped.
    #[test]
    fn a_boundary_is_random_and_long_enough() {
        let a = multipart_boundary().expect("random");
        let b = multipart_boundary().expect("random");
        assert_ne!(a, b);
        assert_eq!(a.len(), 35, "a 3-character tag plus 128 bits of hex");
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()), "a boundary must be token-safe");
    }

    /// Every chunk but the last is a legal Drive chunk. A size that is not a multiple of
    /// 256 KiB is accepted for a while and then fails when the file is COMMITTED, which
    /// reads as a corrupt upload rather than a bad constant.
    #[test]
    fn every_non_final_chunk_is_a_multiple_of_256_kib() {
        let total = 20 * 1024 * 1024 + 4_321;
        let spans = chunk_spans(total, RESUMABLE_CHUNK_BYTES);
        assert_eq!(spans.len(), 3);
        for (_, len) in &spans[..spans.len() - 1] {
            assert!(len.is_multiple_of(RESUMABLE_CHUNK_UNIT), "a middle chunk broke the rule");
        }
        let (last_start, last_len) = spans[spans.len() - 1];
        assert_eq!(last_start + last_len, total, "the chunks must cover the file exactly");
    }

    /// The split between the two upload paths, at Google's documented multipart cap.
    #[test]
    fn the_upload_path_splits_at_googles_cap() {
        assert_eq!(MULTIPART_MAX_BYTES, 5 * 1024 * 1024);
        // The smallest resumable upload is still a single chunk (DRAGON-490 follow-up: this
        // gap, a file just over the multipart cap that STILL completes in one resumable
        // request, is why "will this report progress" cannot be predicted from `size` alone —
        // see `cloud::child::run_cloud_upload`'s dynamic detection instead).
        assert_eq!(chunk_spans(MULTIPART_MAX_BYTES + 1, RESUMABLE_CHUNK_BYTES).len(), 1);
        assert_eq!(chunk_spans(RESUMABLE_CHUNK_BYTES + 1, RESUMABLE_CHUNK_BYTES).len(), 2);
    }
}

#[cfg(test)]
mod resumable_tests {
    use super::*;
    use crate::cloud::http::{CurlResponse, parse_headers};

    /// Build a response the way the transport would, from a raw header dump.
    fn response(status: u16, dump: &str) -> CurlResponse {
        CurlResponse { status, body: Vec::new(), headers: parse_headers(dump) }
    }

    /// **The header the whole feature exists for.** Drive returns the session URI only in
    /// `Location`, and curl reports a `100 Continue` block ahead of it for a body this size.
    #[test]
    fn the_session_uri_is_read_from_the_location_header() {
        let dump = "HTTP/1.1 100 Continue\r\n\r\n\
                    HTTP/1.1 200 OK\r\n\
                    Location: https://www.googleapis.com/upload/drive/v3/files?uploadType=resumable&upload_id=AEnB2Uo\r\n\
                    Content-Length: 0\r\n\r\n";
        let session = response(200, dump).header("Location").expect("a session URI").to_string();
        assert!(session.contains("upload_id=AEnB2Uo"));
        // And it is checked against GOOGLE'S own hosts, not the union: the URI is a
        // capability, so the question is whose it is.
        let host = crate::cloud::http::host_of(&session).expect("a host");
        assert!(is_upload_session_host(host), "{host} must be accepted");
    }

    /// **The redirected-session case (DRAGON-482).** A `Location` naming another provider's
    /// API, or a lookalike, must not receive the user's capture. The shared allowlist would
    /// accept the first of these, which is exactly why this check is per-provider.
    #[test]
    fn a_session_uri_on_another_providers_host_is_refused() {
        for host in ["www.googleapis.com", "oauth2.googleapis.com", "storage.googleapis.com"] {
            assert!(is_upload_session_host(host), "{host} must be accepted");
        }
        for host in [
            "graph.microsoft.com",       // allowlisted for the app, not for Drive
            "content.dropboxapi.com",    // same
            "evilgoogleapis.com",        // the missing-dot lookalike
            "www.googleapis.com.evil.test",
            "evil.test",
            "",
        ] {
            assert!(!is_upload_session_host(host), "{host} must be refused");
        }
        // Case is not significant in a hostname.
        assert!(is_upload_session_host("WWW.GoogleAPIs.COM"));
    }

    /// **The inclusive upper bound.** `bytes=0-262143` means Drive holds through byte
    /// 262143, so the next chunk starts at 262144. Reading it as exclusive would drop one
    /// byte per chunk and corrupt the file without failing the upload.
    #[test]
    fn the_range_header_gives_the_last_byte_held() {
        assert_eq!(parse_range_end("bytes=0-262143"), Some(262_143));
        assert_eq!(parse_range_end("bytes=0-0"), Some(0), "one byte held");
        assert_eq!(parse_range_end(" bytes=0-8388607 "), Some(8_388_607));
        // The next offset is one past it.
        let next = parse_range_end("bytes=0-8388607").expect("an end") + 1;
        assert_eq!(next, RESUMABLE_CHUNK_BYTES, "a full 8 MiB chunk was taken");
    }

    /// A missing or unusable `Range` is not treated as progress. On the first chunk it
    /// legitimately means "I have nothing yet".
    #[test]
    fn an_unusable_range_is_not_progress() {
        for bad in ["", "0-100", "bytes=", "bytes=0-", "bytes=abc", "items=0-5"] {
            assert_eq!(parse_range_end(bad), None, "{bad:?} must not parse");
        }
        // A 308 with no Range header at all.
        assert_eq!(response(308, "HTTP/1.1 308 Resume Incomplete\r\n\r\n").header("Range"), None);
    }

    /// The three answers a chunk PUT can give are distinguishable, which is what the upload
    /// loop branches on: commit, keep going, or fail.
    #[test]
    fn a_chunk_response_is_classified_correctly() {
        let committed = response(200, "HTTP/1.1 200 OK\r\n\r\n");
        assert!(committed.is_success(), "a 200 commits the file");
        let more = response(RESUME_INCOMPLETE, "HTTP/1.1 308 Resume Incomplete\r\nRange: bytes=0-8388607\r\n\r\n");
        assert!(!more.is_success(), "308 is not success");
        assert_eq!(more.status, RESUME_INCOMPLETE);
        assert_eq!(more.header("Range").and_then(parse_range_end), Some(8_388_607));
        let failed = response(403, "HTTP/1.1 403 Forbidden\r\n\r\n");
        assert!(!failed.is_success() && failed.status != RESUME_INCOMPLETE);
    }

    /// **A partially accepted chunk resumes from where Drive actually got to.** Walking the
    /// offsets the server reports, rather than the ones we planned, is what stops a hole
    /// being left in the file. Replays a session where the second chunk is only half taken.
    #[test]
    fn the_offset_follows_the_server_not_the_plan() {
        let total = 20 * 1024 * 1024;
        // Chunk 1 fully taken, chunk 2 half taken, then the rest.
        let replies = [
            "bytes=0-8388607",  // 8 MiB in
            "bytes=0-12582911", // only 4 MiB more, not the full 8
            "bytes=0-20971519", // the rest
        ];
        let mut offset = 0u64;
        let mut sent = Vec::new();
        for reply in replies {
            let len = RESUMABLE_CHUNK_BYTES.min(total - offset);
            sent.push((offset, len));
            offset = parse_range_end(reply).expect("a range") + 1;
        }
        assert_eq!(offset, total, "the session must end exactly at the file size");
        // The third request re-sends from where the server stopped, not from where we
        // thought chunk 2 ended.
        assert_eq!(sent[1].0, 8 * 1024 * 1024);
        assert_eq!(sent[2].0, 12 * 1024 * 1024, "resumed from the server's offset");
        // Progress stays monotone across the short chunk.
        let percents: Vec<u8> = replies
            .iter()
            .map(|r| progress_percent(parse_range_end(r).expect("range") + 1, total))
            .collect();
        assert_eq!(percents, vec![40, 60, 100]);
        assert!(percents.windows(2).all(|w| w[1] >= w[0]));
    }

    /// A server that never advances is bounded rather than trusted: nothing in this app may
    /// wait forever.
    #[test]
    fn a_stalled_session_gives_up() {
        // The bound itself is pinned by a compile-time assert beside the constant. This is
        // the CONDITION that trips it: a Range that does not move past the current offset.
        let offset = 8 * 1024 * 1024;
        let next = parse_range_end("bytes=0-8388607").expect("range") + 1;
        assert!(next <= offset, "this reply is the no-progress case the counter catches");
    }
}

#[cfg(test)]
mod query_tests {
    use super::*;

    /// The listing must exclude trashed folders: uploading into one puts the file in the
    /// trash, where the user will never look for it.
    #[test]
    fn the_folder_query_asks_for_live_folders_only() {
        assert_eq!(
            folder_query(None),
            "mimeType = 'application/vnd.google-apps.folder' and 'root' in parents and trashed = false"
        );
        assert_eq!(
            folder_query(Some("1AbC")),
            "mimeType = 'application/vnd.google-apps.folder' and '1AbC' in parents and trashed = false"
        );
        // An empty parent is the root, not a folder literally named "".
        assert_eq!(folder_query(Some("")), folder_query(None));
    }

    /// **The injection case.** The parent id is interpolated into a single-quoted Drive query
    /// literal. A bare quote in it would close the literal and let the rest be read as query
    /// SYNTAX, which is how a listing turns into someone else's search.
    #[test]
    fn a_quote_in_the_parent_cannot_escape_the_literal() {
        let injected = "x' or trashed = true or '";
        let query = folder_query(Some(injected));
        // The quotes survive as escaped characters rather than as literal delimiters, so the
        // whole value is still ONE literal.
        assert!(query.contains("'x\\' or trashed = true or \\''"), "{query}");
        // Every apostrophe in the interpolated part is preceded by a backslash.
        assert!(!query.contains("or 'x"), "{query}");
        // Backslash first, so an escaped quote's own backslash is not escaped twice.
        assert_eq!(escape_query_literal(r"a\b"), r"a\\b");
        assert_eq!(escape_query_literal("a'b"), r"a\'b");
        assert_eq!(escape_query_literal(r"a\'b"), r"a\\\'b");
        assert_eq!(escape_query_literal("plain-1AbC"), "plain-1AbC");
    }

    /// The query goes into a URL, so it is percent-encoded. The quotes and spaces that make
    /// it a valid Drive query would otherwise end the parameter.
    #[test]
    fn the_query_survives_url_encoding() {
        let encoded = esc(&folder_query(None));
        assert!(!encoded.contains(' '), "a raw space would end the query parameter");
        assert!(!encoded.contains('\''));
        assert!(encoded.contains("%20") && encoded.contains("%27"));
    }
}

#[cfg(test)]
mod root_folder_tests {
    use super::*;

    /// The lookup asks for the name, the type and "not trashed", and NOTHING about parents.
    /// See the module doc: a clause Google does not document as queryable under `drive.file`
    /// would silently match nothing and make every connect create another folder.
    #[test]
    fn the_lookup_query_is_name_type_and_not_trashed() {
        assert_eq!(
            root_query("Cosmic Capture Kit"),
            "name = 'Cosmic Capture Kit' and \
             mimeType = 'application/vnd.google-apps.folder' and trashed = false"
        );
        assert!(!root_query("Cosmic Capture Kit").contains("parents"), "the parent clause is out");
        // A name with an apostrophe is escaped into the literal rather than closing it.
        assert!(root_query("Shane's Kit").contains(r"'Shane\'s Kit'"));
    }

    /// The folder is created in MY DRIVE, where the user can find it. `root` is Drive's own
    /// alias for that folder.
    #[test]
    fn the_folder_is_created_in_my_drive() {
        let body = root_metadata("Cosmic Capture Kit");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(parsed["name"], "Cosmic Capture Kit");
        assert_eq!(parsed["mimeType"], "application/vnd.google-apps.folder");
        assert_eq!(parsed["parents"], serde_json::json!(["root"]));
    }

    /// **The whole point: an existing folder is REUSED.** A reconnect must never leave a second
    /// "Cosmic Capture Kit" beside the first, with the user's captures split between them.
    #[test]
    fn an_existing_folder_is_found_rather_than_recreated() {
        let body = r#"{
            "kind": "drive#fileList",
            "incompleteSearch": false,
            "files": [
                {"kind": "drive#file", "id": "1CckFolder", "name": "Cosmic Capture Kit",
                 "mimeType": "application/vnd.google-apps.folder"}
            ]
        }"#;
        assert_eq!(
            find_root_folder(body, "Cosmic Capture Kit").expect("readable"),
            Some("1CckFolder".to_string())
        );
    }

    /// An empty listing is the CREATE case, and the only one.
    #[test]
    fn an_empty_listing_means_there_is_none_to_reuse() {
        for body in [r#"{"files":[]}"#, "{}", r#"{"kind":"drive#fileList","incompleteSearch":false}"#] {
            assert_eq!(find_root_folder(body, "Cosmic Capture Kit").expect("readable"), None, "{body}");
        }
    }

    /// **An unreadable reply is not "there isn't one".** Creating on a garbled answer is
    /// exactly how a duplicate folder appears, so this is an error rather than a `None`.
    #[test]
    fn an_unreadable_reply_never_becomes_a_create() {
        assert!(find_root_folder("not json", "Cosmic Capture Kit").is_err());
        assert!(find_root_folder("", "Cosmic Capture Kit").is_err());
        // And the message says nothing about the body, which came off the network.
        let err = find_root_folder("<html>SECRET</html>", "Cosmic Capture Kit").expect_err("bad");
        assert!(!err.contains("SECRET"), "{err}");
    }

    /// A row that is not actually ours is skipped: a different name, or an entry with no usable
    /// id. Drive's `name = '...'` is exact, so this is belt and braces on a value that decides
    /// where every future upload lands.
    #[test]
    fn only_a_row_that_is_really_ours_is_reused() {
        let body = r#"{"files":[
            {"id":"1Other","name":"Cosmic Capture Kit Archive"},
            {"id":"","name":"Cosmic Capture Kit"},
            {"name":"Cosmic Capture Kit"},
            {"id":"1Real","name":"Cosmic Capture Kit"}
        ]}"#;
        assert_eq!(
            find_root_folder(body, "Cosmic Capture Kit").expect("readable"),
            Some("1Real".to_string())
        );
        // A near-miss on case is a DIFFERENT folder, and reusing it would be a guess.
        let cased = r#"{"files":[{"id":"1Lower","name":"cosmic capture kit"}]}"#;
        assert_eq!(find_root_folder(cased, "Cosmic Capture Kit").expect("readable"), None);
    }

    /// Two folders of the same name (a user who made one by hand, a duplicate from an older
    /// build) resolve to the SAME one every time: the listing is ordered by `createdTime`, so
    /// the first row is the oldest. A stable answer matters more than a clever one, and
    /// deleting either is not this app's call.
    #[test]
    fn duplicates_resolve_to_one_stable_answer() {
        let body = r#"{"files":[
            {"id":"1Oldest","name":"Cosmic Capture Kit"},
            {"id":"1Newer","name":"Cosmic Capture Kit"}
        ]}"#;
        for _ in 0..3 {
            assert_eq!(
                find_root_folder(body, "Cosmic Capture Kit").expect("readable"),
                Some("1Oldest".to_string())
            );
        }
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    /// A real `files.create` reply shape, with invented ids.
    #[test]
    fn an_upload_reply_parses() {
        let body = r#"{
            "kind": "drive#file",
            "id": "1AbCdEfGhIjKlMnOpQrStUvWxYz012345",
            "name": "Screenshot 2026-08-02.png",
            "mimeType": "image/png",
            "webViewLink": "https://drive.google.com/file/d/1AbCdEfGhIjKlMnOpQrStUvWxYz012345/view?usp=drivesdk"
        }"#;
        let file = parse_file(body).expect("parse");
        assert_eq!(file.id, "1AbCdEfGhIjKlMnOpQrStUvWxYz012345");
        assert!(file.web_url.expect("a link").starts_with("https://drive.google.com/file/d/"));
    }

    /// Without an id there is nothing to share, delete or record, so the upload is a failure
    /// even though the HTTP call succeeded.
    #[test]
    fn an_upload_reply_without_an_id_is_a_failure() {
        assert!(parse_file(r#"{"name":"a.png"}"#).is_err());
        assert!(parse_file(r#"{"id":""}"#).is_err());
        assert!(parse_file("not json").is_err());
        // A reply with no link is fine: the link is fetched separately.
        let file = parse_file(r#"{"id":"1Ab","name":"a.png"}"#).expect("parse");
        assert_eq!(file.web_url, None);
    }

    /// A real `files.list` reply shape, with invented ids.
    #[test]
    fn a_folder_listing_parses() {
        let body = r#"{
            "kind": "drive#fileList",
            "incompleteSearch": false,
            "files": [
                {"kind": "drive#file", "id": "1FolderAaa", "name": "Screenshots"},
                {"kind": "drive#file", "id": "1FolderBbb", "name": "Recordings"}
            ]
        }"#;
        let folders = parse_folders(body).expect("parse");
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].id, "1FolderAaa");
        assert_eq!(folders[1].name, "Recordings");
    }

    /// One malformed row must not cost the user the whole picker.
    #[test]
    fn a_malformed_row_is_skipped_not_fatal() {
        let body = r#"{"files":[
            {"id":"1Good","name":"Keep"},
            {"id":"","name":"No id"},
            {"name":"Also no id"},
            {"id":"1NoName"}
        ]}"#;
        let folders = parse_folders(body).expect("parse");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "Keep");
        // An empty list is a valid answer, and the usual one under `drive.file`.
        assert!(parse_folders(r#"{"files":[]}"#).expect("parse").is_empty());
        assert!(parse_folders("{}").expect("parse").is_empty());
        assert!(parse_folders("not json").is_err());
    }
}

#[cfg(test)]
mod folder_manager_tests {
    use super::*;

    /// The `files.create` body for a folder the user asked for: the folder mime type, and the
    /// parent the picker was looking at. **The parent is stated at BOTH depths**, and at the top
    /// level it is the app folder's own id (DRAGON-520): omitting it is Drive's documented "put
    /// this in My Drive", which made the folder outside the app folder, where the `drive.file`
    /// listing that re-reads the level cannot see it.
    #[test]
    fn a_new_folder_is_created_inside_the_level_being_viewed() {
        let body = |name: &str, parent: &str| {
            serde_json::from_str::<serde_json::Value>(&folder_metadata(name, parent))
                .expect("valid JSON")
        };
        // The TOP level: the parent is the app folder's own id, not an absent field.
        let top = body("Screenshots", "1AppFolderAaa");
        assert_eq!(top["name"], "Screenshots");
        assert_eq!(top["mimeType"], FOLDER_MIME);
        assert_eq!(top["parents"], serde_json::json!(["1AppFolderAaa"]));
        // One level deep: the same three fields, the subfolder's id.
        let deep = body("2026", "1SubAaa");
        assert_eq!(deep["name"], "2026");
        assert_eq!(deep["mimeType"], FOLDER_MIME);
        assert_eq!(deep["parents"], serde_json::json!(["1SubAaa"]));
        // The mime type is what makes it a FOLDER rather than an empty file, and `parents` is
        // never absent: an absent one is the DRAGON-520 defect, in the one place it could be
        // written again.
        for body in [folder_metadata("x", "1Root"), folder_metadata("y", "1Sub")] {
            assert!(body.contains(FOLDER_MIME), "{body}");
            assert!(body.contains("\"parents\""), "{body}");
        }
        // The name is JSON-encoded, so a quote in it cannot break out of the document.
        let quoted = folder_metadata(r#"say "hi""#, "1Root");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&quoted).expect("valid json")["name"],
            serde_json::json!(r#"say "hi""#)
        );
    }

    /// Where a create is POSTed and where a delete is sent, at both depths (DRAGON-520).
    ///
    /// A create has ONE address whatever the level, because the level rides in the body; a delete
    /// names the folder itself, and a folder listed at the top level is the same kind of Drive
    /// file as one listed three levels down.
    #[test]
    fn the_create_and_delete_urls_are_the_documented_ones() {
        assert_eq!(create_url(), "https://www.googleapis.com/drive/v3/files?fields=id,name");
        // A folder made at the top level, and one made inside a subfolder: both delete by id.
        assert_eq!(
            delete_url("1TopLevelAaa"),
            "https://www.googleapis.com/drive/v3/files/1TopLevelAaa"
        );
        assert_eq!(delete_url("1DeepBbb"), "https://www.googleapis.com/drive/v3/files/1DeepBbb");
        // An id off the network cannot add a path segment.
        let hostile = delete_url("../../files");
        assert_eq!(hostile, "https://www.googleapis.com/drive/v3/files/..%2F..%2Ffiles");
        assert_eq!(hostile.matches('/').count(), delete_url("1Plain").matches('/').count());
    }

    /// **A create at the top level cannot be issued until the app folder is known**
    /// (DRAGON-520), and a root nobody can name is a SENTENCE rather than a create at the top of
    /// My Drive. The shared decision is proven in `providers::create_target_tests`; this pins
    /// THIS provider's wiring of it, including the copy the user would read.
    #[test]
    fn a_create_refuses_to_guess_where_the_app_folder_is() {
        let resolved = super::super::create_parent_with(
            None,
            || Ok(Some("1AppFolderAaa".to_string())),
            ROOT_UNKNOWN,
        );
        assert_eq!(resolved.as_deref(), Ok("1AppFolderAaa"));
        let unknown =
            super::super::create_parent_with(None, || Ok(None), ROOT_UNKNOWN).expect_err("no root");
        assert_eq!(unknown, ROOT_UNKNOWN);
        // Names the service and what could not happen, and quotes no folder name back at the
        // user: it can reach a log.
        assert!(unknown.contains("Google Drive"), "{unknown}");
        assert!(unknown.contains("nowhere to go"), "{unknown}");
    }

    /// A real `files.list` page one, followed by its last page. The token is what the loop
    /// follows, and its ABSENCE is the only thing that ends the listing.
    #[test]
    fn a_listing_follows_its_page_token_until_there_is_none() {
        let page_one = r#"{
            "kind": "drive#fileList",
            "nextPageToken": "~!!~AI9FV7QaBcDeFgHiJkLmNoPqRsTuVwXyZ012345",
            "incompleteSearch": false,
            "files": [{"id": "1FolderAaa", "name": "Screenshots"}]
        }"#;
        assert_eq!(
            next_page_token(page_one).as_deref(),
            Some("~!!~AI9FV7QaBcDeFgHiJkLmNoPqRsTuVwXyZ012345")
        );
        // The entries still parse the same way, so following pages only ever appends.
        assert_eq!(parse_folders(page_one).expect("parse").len(), 1);
        let last = r#"{"kind":"drive#fileList","files":[{"id":"1FolderBbb","name":"Recordings"}]}"#;
        assert_eq!(next_page_token(last), None, "the last page ends the listing");
        // An empty token is not a token: following it would ask for page one forever.
        assert_eq!(next_page_token(r#"{"nextPageToken":"","files":[]}"#), None);
        // An unreadable body says "no more pages" rather than erroring; `parse_folders` is what
        // reports a body that cannot be read.
        assert_eq!(next_page_token("not json"), None);
    }

    /// A real `files.create` reply for a folder. The id is required (it is what the account
    /// stores and what the next level lists against); the NAME falls back to what was asked for,
    /// so a reply missing it cannot cost the user the folder they just made.
    #[test]
    fn a_created_folder_parses_and_falls_back_to_the_name_asked_for() {
        let body = r#"{
            "kind": "drive#file",
            "id": "1NewFolderAaaBbbCcc",
            "name": "Screenshots",
            "mimeType": "application/vnd.google-apps.folder"
        }"#;
        let made = parse_created_folder(body, "Screenshots").expect("parse");
        assert_eq!(made, RemoteFolder { id: "1NewFolderAaaBbbCcc".to_string(), name: "Screenshots".to_string() });
        let nameless = parse_created_folder(r#"{"id":"1New"}"#, "Asked for").expect("parse");
        assert_eq!(nameless.name, "Asked for");
        // No id means we cannot address it, which is a failure however friendly the reply looks.
        assert!(parse_created_folder(r#"{"name":"Screenshots"}"#, "Screenshots").is_err());
        assert!(parse_created_folder(r#"{"id":""}"#, "x").is_err());
        let err = parse_created_folder("<html>SECRET</html>", "x").expect_err("unreadable");
        assert!(!err.contains("SECRET"), "{err}");
    }
}
