//! Dropbox, through the API v2 (DRAGON-482, stage A2).
//!
//! Verified against `dropbox.com/developers/documentation/http/documentation` (the `files`,
//! `sharing` and `auth` sections), `dropbox.com/developers/reference/json-encoding`, and
//! `developers.dropbox.com/dbx-performance-guide`.
//!
//! # What is different here
//!
//! **A folder is a PATH, not an id.** Dropbox does have file ids (`id:abc123`), and this
//! module uses them for the file it uploaded, because `delete_v2` and the sharing calls
//! accept them. But an upload COMMIT takes a path string, so a destination folder has to be
//! remembered as its path. [`RemoteFolder::id`](super::RemoteFolder) therefore holds
//! `path_lower` for this provider, and [`CloudAccount::folder_id`] does too. The root is the
//! EMPTY string, not `/`, which is Dropbox's own convention: "The empty string ("")
//! represents the root folder."
//!
//! **That root IS the app folder** (DRAGON-498), and nothing in this file had to change for it
//! to be. The app registration is created with **App folder** access rather than Full Dropbox,
//! and Dropbox then presents `Apps/<the app's name>/` to the token "as root or '/'". So every
//! path here, `""` for a listing and `/name` for an upload, is already relative to that folder,
//! and there is no reachable path OUT of it: the sandbox is enforced by Dropbox, not by us.
//! That is also why [`super::ProviderOps::ensure_root_folder`] is left at its default here,
//! with no folder to find or create.
//!
//! Two things follow from that. **Nothing in the sign-in changes**: the access type is fixed
//! when the app is created ("you'll need to delete the existing app and create a new one with
//! the desired access type"), and the registration `CLOUD_ACCOUNTS.md` describes was already
//! made this way, so the client id, the four redirect URIs and the four scopes are all
//! untouched and no connected Dropbox account has to reconnect. (Scope names do not vary by
//! access type; Dropbox's own API spec marks each of the four routes used here
//! `allow_app_folder_app = true`.) And the TYPED PATH field the setup step used to offer for
//! this provider alone is gone: with the token confined to one folder, a typed path could only
//! ever name a subfolder of it, which the folder list already does, and a text box that looks
//! like it can address a drive it cannot is worse than no text box.
//!
//! **Arguments travel in a header, and that header must be plain ASCII.** Dropbox's
//! content-upload endpoints take their JSON argument in `Dropbox-API-Arg` rather than the
//! body, since the body is the file. A header cannot reliably carry non-ASCII, so Dropbox
//! specifies: "you need to make it 'HTTP header safe'. This means using JSON-style
//! `"\uXXXX"` escape codes for the character `0x7F` and all non-ASCII characters." Any file
//! named in a language other than English hits this, so [`header_safe_json`] is not an edge
//! case, and it is unit-tested against an accented name and an emoji.
//!
//! **Errors are 409s with a tag.** Dropbox returns HTTP 409 for endpoint-specific errors and
//! puts the meaning in `error[".tag"]`. That is how the already-shared case is recognised.
//! `error_summary` is deliberately NOT matched on: Dropbox appends "a random number of '.'
//! characters at the end of the string" specifically to stop exact matching.

use std::path::Path;

use super::{
    META_BUDGET, ProviderOps, RemoteFile, RemoteFolder, TempChunk, api, api_failure, bearer,
    chunk_spans, file_size, progress_percent, remote_name, upload_budget,
};
use crate::cloud::accounts::CloudAccount;
use crate::cloud::http::{Body, CurlResponse, Method};

/// Every https host this provider reaches.
///
/// `www.dropbox.com` is the authorization endpoint and comes from the registry; every API
/// call is on one of these two. Splitting RPC from content is Dropbox's design, not ours.
pub(super) const HOSTS: &[&str] = &["api.dropboxapi.com", "content.dropboxapi.com"];

/// Where the JSON-in, JSON-out calls go.
const RPC: &str = "https://api.dropboxapi.com/2";

/// Where the calls whose body is a file go.
const CONTENT: &str = "https://content.dropboxapi.com/2";

/// The largest file sent in one request.
///
/// Dropbox's own cap on `/2/files/upload` is 150 MiB ("Do not use this to upload a file
/// larger than 150 MiB"), and this is deliberately far below it. The reason is progress
/// rather than protocol: a single request reports 0% until it finishes, so a 100 MB upload
/// through it looks frozen. Above this the session path reports after every chunk.
pub const SINGLE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The unit Dropbox wants an append sized in multiples of.
///
/// It is a hard requirement only for `concurrent` sessions, which this module does not use,
/// and a recommendation otherwise ("Consider uploading chunks in multiples of 4 MBs"). It is
/// honoured anyway: it costs nothing, and it keeps the door open to a concurrent session
/// later without re-deriving the chunk size.
pub const CHUNK_UNIT: u64 = 4 * 1024 * 1024;

/// The chunk size: two units.
pub const CHUNK_BYTES: u64 = 2 * CHUNK_UNIT;

const _: () = assert!(
    CHUNK_BYTES.is_multiple_of(CHUNK_UNIT),
    "DRAGON-482: a Dropbox append must be a multiple of 4 MiB to stay eligible for a \
     concurrent upload session"
);
const _: () = assert!(
    CHUNK_BYTES <= 150 * 1024 * 1024,
    "DRAGON-482: Dropbox refuses a single request above 150 MiB"
);

/// How many entries one folder listing asks for. Dropbox's documented maximum is 2000.
const LIST_LIMIT: u32 = 1000;

pub(super) struct Dropbox;

impl ProviderOps for Dropbox {
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
        let path = upload_path(acct.folder_id.as_deref(), &name);
        if size <= SINGLE_MAX_BYTES {
            // One request, no per-chunk pause to check between: `should_cancel` is not
            // consulted here (see `providers::upload`'s doc for why).
            return single_upload(token, file, &path, size, on_progress);
        }
        session_upload(token, file, &path, size, on_progress, should_cancel)
    }

    fn create_share_link(&self, token: &str, file_id: &str) -> Result<String, String> {
        let body = serde_json::json!({ "path": file_id }).to_string();
        let response = rpc(token, "sharing/create_shared_link_with_settings", body, META_BUDGET)?;
        if response.is_success() {
            return read_link(&response.text());
        }
        // The one failure that is not a failure: this app already made a link for the file,
        // which is what a second share of the same capture looks like. Dropbox may include
        // the existing link inline, but documents that as optional ("Existing link metadata
        // will not be returned if custom settings were specified"), so the fallback is a
        // second call rather than a hope.
        if response.status == 409
            && error_tag(&response.text()).as_deref() == Some("shared_link_already_exists")
        {
            return existing_link(token, file_id);
        }
        Err(api_failure("dropbox", "share that file", &response))
    }

    /// Every folder inside `parent`, following the cursor to the end (DRAGON-506).
    ///
    /// Dropbox pages with `has_more` plus a cursor fed back into `files/list_folder/continue`,
    /// a DIFFERENT endpoint taking only that cursor, which is why the follow-up request is not
    /// the first one repeated with an extra argument. This used to take the first page only, on
    /// the reasoning that a thousand folders is plenty for a picker; it is now followed to the
    /// end like the other two, under [`super::MAX_LIST_PAGES`].
    fn list_folders(&self, token: &str, parent: Option<&str>) -> Result<Vec<RemoteFolder>, String> {
        let body = serde_json::json!({
            "path": list_path(parent),
            "recursive": false,
            "include_deleted": false,
            "include_mounted_folders": true,
            "limit": LIST_LIMIT,
        })
        .to_string();
        let mut response = rpc(token, "files/list_folder", body, META_BUDGET)?;
        let mut folders: Vec<RemoteFolder> = Vec::new();
        let mut pages = 0u32;
        while super::may_fetch_another_page(pages) {
            if !response.is_success() {
                // **A path that is not there is a NAMED answer** (DRAGON-506), so the setup step
                // can walk its breadcrumb up a level rather than failing the whole dialog.
                // Dropbox reports it as a 409 carrying `path/not_found`, not as a 404. Only for
                // a SUBFOLDER: the app folder is the token's own root and cannot be missing.
                if parent.filter(|p| !p.is_empty()).is_some()
                    && is_path_not_found(&response.text())
                {
                    log::debug!("cloud dropbox: a folder being listed is no longer there");
                    return Err(super::missing_folder_err());
                }
                return Err(api_failure("dropbox", "list folders", &response));
            }
            let text = response.text();
            folders.extend(parse_folders(&text)?);
            pages += 1;
            let Some(cursor) = next_cursor(&text) else {
                return Ok(folders);
            };
            if !super::may_fetch_another_page(pages) {
                break;
            }
            let body = serde_json::json!({ "cursor": cursor }).to_string();
            response = rpc(token, "files/list_folder/continue", body, META_BUDGET)?;
        }
        log::warn!("cloud dropbox: stopped following a folder listing after {pages} pages");
        Ok(folders)
    }

    /// `files/create_folder_v2` with `autorename` OFF.
    ///
    /// Off rather than on, unlike the upload commit beside it: an upload is a capture the user
    /// already asked to keep, so a taken name must not lose it, while a folder is something they
    /// just named, and creating "Screenshots (1)" instead would answer a different question.
    ///
    /// **This provider needed no part of DRAGON-520's fix, and the reason is worth keeping.** A
    /// Dropbox folder is addressed by its PATH, and the app folder's path is the empty string
    /// (the app-folder token's own root), so [`upload_path`] builds `/Name` at the top level and
    /// `/Parent/Name` inside a subfolder with no special case and no lookup. The two providers
    /// that broke both have a root that is a different KIND of thing from the levels below it, an
    /// id to find (Drive) or an alias to resolve (Graph), and both got the top level wrong while
    /// the depths worked.
    fn create_folder(
        &self,
        token: &str,
        parent: Option<&str>,
        name: &str,
    ) -> Result<RemoteFolder, String> {
        let path = upload_path(parent, name);
        let response = rpc(token, "files/create_folder_v2", create_folder_arg(&path), META_BUDGET)?;
        if response.status == 409 {
            let text = response.text();
            if is_path_conflict(&text) {
                return Err("That folder name is already taken here.".to_string());
            }
        }
        if !response.is_success() {
            return Err(api_failure("dropbox", "create that folder", &response));
        }
        parse_created_folder(&response.text(), name, &path)
    }

    fn delete_file(&self, token: &str, file_id: &str) -> Result<(), String> {
        let response = rpc(token, "files/delete_v2", delete_arg(file_id), META_BUDGET)?;
        if !response.is_success() {
            return Err(api_failure("dropbox", "delete that file", &response));
        }
        Ok(())
    }

    /// `files/delete_v2` on the folder's path, which takes its contents with it.
    ///
    /// Dropbox does keep deleted files for a window and can restore them, but that window and
    /// the restore UI depend on the account's plan, so `ProviderCaps::delete_recoverable` is
    /// false here and the confirmation the user sees does not promise a way back.
    ///
    /// One path, at every depth: a folder's `path_lower` is what the listing already handed the
    /// picker, whether it was listed at the top level or three levels down.
    fn delete_folder(&self, token: &str, folder_id: &str) -> Result<(), String> {
        let response = rpc(token, "files/delete_v2", delete_arg(folder_id), META_BUDGET)?;
        if !response.is_success() {
            return Err(api_failure("dropbox", "delete that folder", &response));
        }
        Ok(())
    }

    fn revoke(&self, token: &str) -> Result<(), String> {
        // "Disables the access token used to authenticate the call. If there is a
        // corresponding refresh token for the access token, this disables that refresh token,
        // as well as any other access tokens for that refresh token." No body, no parameters.
        let url = format!("{RPC}/auth/token/revoke");
        let response = bearer(api(Method::Post, &url, META_BUDGET)?, token).no_retry().send()?;
        if !response.is_success() {
            return Err(api_failure("dropbox", "sign this account out", &response));
        }
        Ok(())
    }
}

/// One request to an RPC endpoint: JSON in, JSON out.
fn rpc(
    token: &str,
    endpoint: &str,
    body: String,
    budget: std::time::Duration,
) -> Result<CurlResponse, String> {
    let url = format!("{RPC}/{endpoint}");
    bearer(api(Method::Post, &url, budget)?, token)
        .header("Content-Type", "application/json")
        .body(Body::Text(body))
        .send()
}

/// One request to a content endpoint: the argument in a header, the file as the body.
fn content(
    token: &str,
    endpoint: &str,
    arg: &str,
    body: Body,
    budget: std::time::Duration,
) -> Result<CurlResponse, String> {
    let url = format!("{CONTENT}/{endpoint}");
    bearer(api(Method::Post, &url, budget)?, token)
        .header("Dropbox-API-Arg", &header_safe_json(arg))
        .header("Content-Type", "application/octet-stream")
        .body(body)
        // Never repeated: an upload that actually landed would be duplicated, and an append
        // would be applied twice at the same offset.
        .no_retry()
        .send()
}

/// One request, for a file small enough to be worth sending whole.
fn single_upload(
    token: &str,
    file: &Path,
    path: &str,
    size: u64,
    on_progress: &mut dyn FnMut(u8),
) -> Result<RemoteFile, String> {
    let arg = commit_arg(path);
    let response = content(
        token,
        "files/upload",
        &arg,
        Body::File(file.to_path_buf()),
        upload_budget(size),
    )?;
    if !response.is_success() {
        return Err(api_failure("dropbox", "upload that file", &response));
    }
    let uploaded = parse_metadata(&response.text())?;
    on_progress(100);
    log::debug!("cloud dropbox: uploaded a file of {size} bytes in one request");
    Ok(uploaded)
}

/// start, then one append per chunk, then finish.
fn session_upload(
    token: &str,
    file: &Path,
    path: &str,
    size: u64,
    on_progress: &mut dyn FnMut(u8),
    should_cancel: &dyn Fn() -> bool,
) -> Result<RemoteFile, String> {
    // No data with start or finish, per Dropbox's performance guide: it keeps the two calls
    // that bracket the upload "smaller, faster, and easier to retry".
    let start = content(
        token,
        "files/upload_session/start",
        r#"{"close":false}"#,
        Body::None,
        META_BUDGET,
    )?;
    if !start.is_success() {
        return Err(api_failure("dropbox", "start that upload", &start));
    }
    let session_id = super::json_field(&start.text(), "session_id", "starting an upload")?;

    let spans = chunk_spans(size, CHUNK_BYTES);
    for (offset, len) in &spans {
        // Checked BETWEEN chunks (DRAGON-490), the natural pause: the loop already stops
        // here between one request and the next.
        // Nothing to tell Dropbox: an abandoned upload session expires on its own and commits
        // nothing, since only `upload_session/finish` creates the file (DRAGON-495).
        if should_cancel() {
            return Err(super::canceled_err());
        }
        let chunk = TempChunk::write(file, *offset, *len)?;
        let response = content(
            token,
            "files/upload_session/append_v2",
            &append_arg(&session_id, *offset),
            Body::File(chunk.path().to_path_buf()),
            upload_budget(*len),
        )?;
        if !response.is_success() {
            return Err(api_failure("dropbox", "upload part of that file", &response));
        }
        on_progress(progress_percent(offset + len, size));
    }

    let finish = content(
        token,
        "files/upload_session/finish",
        &finish_arg(&session_id, size, path),
        Body::None,
        META_BUDGET,
    )?;
    if !finish.is_success() {
        return Err(api_failure("dropbox", "finish that upload", &finish));
    }
    let uploaded = parse_metadata(&finish.text())?;
    log::debug!("cloud dropbox: finished a {size} byte upload in {} chunks", spans.len());
    Ok(uploaded)
}

/// Fetch the link Dropbox says already exists.
fn existing_link(token: &str, file_id: &str) -> Result<String, String> {
    // `direct_only` matters: without it Dropbox also returns links to PARENT folders, and the
    // first of those would be handed to the user as though it were a link to their file.
    let body = serde_json::json!({ "path": file_id, "direct_only": true }).to_string();
    let response = rpc(token, "sharing/list_shared_links", body, META_BUDGET)?;
    if !response.is_success() {
        return Err(api_failure("dropbox", "find that file's link", &response));
    }
    parse_existing_link(&response.text())
}

/// Where an upload lands. Pure; unit-tested.
///
/// Dropbox paths start with a slash and the root is the empty string, so a root upload is
/// `/name` and a folder upload is `/Folder/name`. A trailing slash on the stored folder is
/// tolerated, because a hand-edited accounts file will eventually have one.
///
/// Every one of those paths is inside the APP FOLDER (DRAGON-498), because that is what this
/// token's root is; `/name` is `Apps/<app>/name` in the user's own Dropbox.
pub fn upload_path(folder_id: Option<&str>, name: &str) -> String {
    let folder = folder_id.unwrap_or("").trim_end_matches('/');
    format!("{folder}/{name}")
}

/// The path for a folder listing. Pure; unit-tested.
///
/// The root is `""`, NOT `/`: Dropbox refuses a path that is only a slash. With an app-folder
/// registration that empty string IS the app folder (DRAGON-498), which is why a `None` parent
/// lists the sandbox rather than the user's whole Dropbox.
pub fn list_path(parent: Option<&str>) -> String {
    match parent.map(str::trim).filter(|p| !p.is_empty() && *p != "/") {
        Some(path) => path.trim_end_matches('/').to_string(),
        None => String::new(),
    }
}

/// The `files/create_folder_v2` argument. Pure; unit-tested.
///
/// `autorename: false` is the refusal [`Dropbox::create_folder`] documents: a name already taken
/// comes back as a 409 the picker turns into a message, not as "Screenshots (1)".
pub fn create_folder_arg(path: &str) -> String {
    serde_json::json!({ "path": path, "autorename": false }).to_string()
}

/// The `files/delete_v2` argument, for a file or a folder. Pure; unit-tested.
///
/// One builder for both, because Dropbox has one endpoint for both: what a path points at is the
/// only difference, and deleting a folder takes its contents with it.
pub fn delete_arg(path: &str) -> String {
    serde_json::json!({ "path": path }).to_string()
}

/// The commit argument for an upload. Pure; unit-tested.
///
/// `autorename` rather than the default `add` behaviour on collision, so two captures made in
/// the same second do not lose the second one.
pub fn commit_arg(path: &str) -> String {
    serde_json::json!({
        "path": path,
        "mode": "add",
        "autorename": true,
        "mute": false,
        "strict_conflict": false,
    })
    .to_string()
}

/// The argument for one append. Pure; unit-tested.
pub fn append_arg(session_id: &str, offset: u64) -> String {
    serde_json::json!({
        "cursor": { "session_id": session_id, "offset": offset },
        "close": false,
    })
    .to_string()
}

/// The argument that commits a session. Pure; unit-tested.
///
/// The cursor offset is the TOTAL size: it is where the session has got to, and Dropbox
/// checks it against what it received. A wrong value here fails the whole upload after every
/// byte has already been sent.
pub fn finish_arg(session_id: &str, total: u64, path: &str) -> String {
    let commit: serde_json::Value =
        serde_json::from_str(&commit_arg(path)).unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "cursor": { "session_id": session_id, "offset": total },
        "commit": commit,
    })
    .to_string()
}

/// Re-escape a JSON document so it can travel in an HTTP header. Pure; unit-tested.
///
/// Dropbox: "you need to make it 'HTTP header safe'. This means using JSON-style `"\uXXXX"`
/// escape codes for the character `0x7F` and all non-ASCII characters."
///
/// The escapes are UTF-16 code units, so a character outside the basic plane (an emoji in a
/// file name) becomes a surrogate PAIR of them. `char::encode_utf16` does exactly that, which
/// is why this is six lines rather than a parser.
pub fn header_safe_json(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut buf = [0u16; 2];
    for ch in json.chars() {
        if ch.is_ascii() && ch != '\u{7f}' {
            out.push(ch);
            continue;
        }
        use std::fmt::Write as _;
        for unit in ch.encode_utf16(&mut buf) {
            let _ = write!(out, "\\u{unit:04x}");
        }
    }
    out
}

/// The `.tag` of a Dropbox error body. Pure; unit-tested.
///
/// Read from `error[".tag"]` and never from `error_summary`, because Dropbox appends "a
/// random number of '.' characters at the end of the string" to that field specifically "to
/// disincentivize exact string matching".
pub fn error_tag(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("error")?
        .get(".tag")?
        .as_str()
        .map(str::to_string)
}

/// One entry of a listing or an upload reply.
#[derive(serde::Deserialize)]
struct Entry {
    #[serde(rename = ".tag")]
    tag: Option<String>,
    id: Option<String>,
    name: Option<String>,
    path_lower: Option<String>,
}

/// A `list_folder` reply.
#[derive(serde::Deserialize)]
struct ListResult {
    #[serde(default)]
    entries: Vec<Entry>,
    /// Where the next page starts, meaningful only while [`Self::has_more`] is true.
    cursor: Option<String>,
    #[serde(default)]
    has_more: bool,
}

/// Read an uploaded file's metadata. Pure; unit-tested.
pub fn parse_metadata(body: &str) -> Result<RemoteFile, String> {
    let parsed: Entry = serde_json::from_str(body)
        .map_err(|_| "Dropbox's reply after the upload could not be read.".to_string())?;
    let id = parsed
        .id
        .filter(|i| !i.is_empty())
        .ok_or_else(|| "Dropbox did not say where it put the file.".to_string())?;
    Ok(RemoteFile {
        // Dropbox does not hand back a viewable URL with an upload; that is what the sharing
        // call is for.
        web_url: None,
        id,
    })
}

/// Read the folders out of a listing. Pure; unit-tested.
///
/// The id carried forward is `path_lower`, not the Dropbox id: an upload commit takes a
/// path, so the path is what a destination folder has to be remembered as.
pub fn parse_folders(body: &str) -> Result<Vec<RemoteFolder>, String> {
    let parsed: ListResult = serde_json::from_str(body)
        .map_err(|_| "Dropbox's folder list could not be read.".to_string())?;
    Ok(parsed
        .entries
        .into_iter()
        .filter(|e| e.tag.as_deref() == Some("folder"))
        .filter_map(|e| {
            let id = e.path_lower.filter(|p| !p.is_empty())?;
            let name = e.name.filter(|n| !n.is_empty())?;
            Some(RemoteFolder { id, name })
        })
        .collect())
}

/// The cursor for the NEXT page of a listing, when Dropbox says there is one (DRAGON-506).
/// Pure; unit-tested.
///
/// `has_more` is the authority and the cursor is only meaningful with it: Dropbox returns a
/// cursor on EVERY page (it is also how a caller would later ask "what changed since"), so a
/// loop keyed on the cursor's presence alone would never stop.
pub fn next_cursor(body: &str) -> Option<String> {
    let parsed = serde_json::from_str::<ListResult>(body).ok()?;
    if !parsed.has_more {
        return None;
    }
    parsed.cursor.filter(|c| !c.is_empty())
}

/// Whether an error body says the PATH itself is not there (DRAGON-506). Pure; unit-tested.
///
/// Dropbox answers a listing of a folder that has been deleted with a 409 whose error is
/// `{"error": {".tag": "path", "path": {".tag": "not_found"}}}`, not with a 404. The NESTED tag
/// is what says which kind of path error it is, so [`error_tag`]'s outer read is not enough on
/// its own; both are checked, since `path` alone also covers "restricted content" and "malformed
/// path", which are not a folder having gone away.
pub fn is_path_not_found(body: &str) -> bool {
    path_error_tag(body).as_deref() == Some("not_found")
}

/// Whether an error body says the path is ALREADY TAKEN. Pure; unit-tested.
///
/// The `conflict` arm of the same nested tag, which is what `create_folder_v2` answers when a
/// folder of that name is already there and `autorename` is off.
pub fn is_path_conflict(body: &str) -> bool {
    path_error_tag(body).as_deref() == Some("conflict")
}

/// The nested `error.path[".tag"]`, the field that says WHICH path error this is. Pure;
/// unit-tested.
///
/// Read from the tags and never from `error_summary`, for the reason [`error_tag`] gives: that
/// field carries a random number of trailing dots specifically to break exact matching. The
/// nested value can itself be an object (`{"conflict": {".tag": "folder"}}` in some replies), so
/// a plain string and a tagged object are both accepted.
fn path_error_tag(body: &str) -> Option<String> {
    let error = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let path = error.get("error")?.get("path")?;
    match path {
        serde_json::Value::String(tag) => Some(tag.clone()),
        other => other.get(".tag")?.as_str().map(str::to_string),
    }
}

/// Read the folder a `create_folder_v2` just made. Pure; unit-tested.
///
/// The reply wraps the folder in a `metadata` object, unlike the flat entry an upload answers
/// with. The id carried forward is `path_lower`, exactly as [`parse_folders`] does, because a
/// Dropbox destination has to be remembered as a path; `requested_path` is the fallback for a
/// reply that carries no metadata at all, since this app built that path itself and knows it is
/// where the folder went.
pub fn parse_created_folder(
    body: &str,
    requested_name: &str,
    requested_path: &str,
) -> Result<RemoteFolder, String> {
    let metadata = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| "Dropbox's reply after making the folder could not be read.".to_string())?;
    let folder = metadata.get("metadata");
    let id = folder
        .and_then(|f| f.get("path_lower"))
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| requested_path.to_ascii_lowercase());
    let name = folder
        .and_then(|f| f.get("name"))
        .and_then(|n| n.as_str())
        .filter(|n| !n.is_empty())
        .unwrap_or(requested_name)
        .to_string();
    Ok(RemoteFolder { id, name })
}

/// Read the URL out of a `create_shared_link_with_settings` reply. Pure; unit-tested.
pub fn read_link(body: &str) -> Result<String, String> {
    super::json_field(body, "url", "sharing a file")
}

/// Read the first direct link out of a `list_shared_links` reply. Pure; unit-tested.
pub fn parse_existing_link(body: &str) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("links")?
                .as_array()?
                .iter()
                .find_map(|l| l.get("url")?.as_str().filter(|u| !u.is_empty()).map(str::to_string))
        })
        .ok_or_else(|| "Dropbox said this file is already shared but did not give the link.".to_string())
}

#[cfg(test)]
mod path_tests {
    use super::*;

    /// The root is `/name`, a folder is `/Folder/name`, and a stored trailing slash does not
    /// produce a double one.
    #[test]
    fn an_upload_path_is_rooted_and_single_slashed() {
        assert_eq!(upload_path(None, "shot.png"), "/shot.png");
        assert_eq!(upload_path(Some(""), "shot.png"), "/shot.png");
        assert_eq!(upload_path(Some("/Screenshots"), "shot.png"), "/Screenshots/shot.png");
        assert_eq!(upload_path(Some("/Screenshots/"), "shot.png"), "/Screenshots/shot.png");
        assert_eq!(upload_path(Some("/a/b"), "c.png"), "/a/b/c.png");
    }

    /// **The root is the empty string, not a slash.** Dropbox refuses a path that is only a
    /// slash, so getting this wrong makes the folder picker fail on its first call.
    #[test]
    fn the_listing_root_is_the_empty_string() {
        assert_eq!(list_path(None), "");
        assert_eq!(list_path(Some("")), "");
        assert_eq!(list_path(Some("/")), "");
        assert_eq!(list_path(Some("  ")), "");
        assert_eq!(list_path(Some("/Screenshots")), "/Screenshots");
        assert_eq!(list_path(Some("/Screenshots/")), "/Screenshots");
    }
}

#[cfg(test)]
mod arg_tests {
    use super::*;

    /// The commit renames on collision, so a second capture in the same second is not lost.
    #[test]
    fn the_commit_renames_on_collision() {
        let parsed: serde_json::Value = serde_json::from_str(&commit_arg("/a/b.png")).expect("json");
        assert_eq!(parsed["path"], "/a/b.png");
        assert_eq!(parsed["autorename"], true);
        assert_eq!(parsed["mode"], "add");
    }

    /// The session arguments carry the cursor Dropbox checks its byte count against. The
    /// finish offset is the TOTAL, and a wrong one fails the upload after every byte has been
    /// sent.
    #[test]
    fn the_session_arguments_carry_the_right_offsets() {
        let append: serde_json::Value = serde_json::from_str(&append_arg("sess-1", 8388608)).expect("json");
        assert_eq!(append["cursor"]["session_id"], "sess-1");
        assert_eq!(append["cursor"]["offset"], 8388608);
        assert_eq!(append["close"], false);

        let finish: serde_json::Value =
            serde_json::from_str(&finish_arg("sess-1", 25165824, "/a/b.png")).expect("json");
        assert_eq!(finish["cursor"]["offset"], 25165824);
        assert_eq!(finish["commit"]["path"], "/a/b.png");
        assert_eq!(finish["commit"]["autorename"], true);
    }

    /// The offsets an append actually receives are the chunk starts, contiguous from zero.
    #[test]
    fn append_offsets_walk_the_file() {
        let total = 20 * 1024 * 1024;
        let offsets: Vec<u64> = chunk_spans(total, CHUNK_BYTES).into_iter().map(|(s, _)| s).collect();
        assert_eq!(offsets, vec![0, 8388608, 16777216]);
        // Every append but the last is a whole chunk, which keeps the 4 MiB rule.
        for (_, len) in &chunk_spans(total, CHUNK_BYTES)[..2] {
            assert_eq!(*len % CHUNK_UNIT, 0);
        }
    }
}

#[cfg(test)]
mod progress_shape_tests {
    use super::*;

    /// Unlike Drive and OneDrive, Dropbox has no gap between "past the single-request cap"
    /// and "more than one chunk": [`SINGLE_MAX_BYTES`] equals [`CHUNK_BYTES`] exactly, so
    /// anything routed to `session_upload` is always at least two chunks. (DRAGON-490
    /// follow-up: the other two providers' equivalent gap is why "will this report progress"
    /// is decided dynamically now, `cloud::child::run_cloud_upload`, rather than predicted
    /// from `size` — this test just keeps Dropbox's own no-gap property honest.)
    #[test]
    fn the_single_upload_cap_and_the_chunk_size_are_the_same_byte() {
        assert_eq!(SINGLE_MAX_BYTES, CHUNK_BYTES, "this provider's whole no-dead-zone property");
        assert_eq!(chunk_spans(SINGLE_MAX_BYTES + 1, CHUNK_BYTES).len(), 2);
    }
}

#[cfg(test)]
mod header_encoding_tests {
    use super::*;

    /// **Any non-English file name hits this.** Non-ASCII in an HTTP header is not reliably
    /// transmitted, so Dropbox requires `\uXXXX` escapes, and a file called `Café.png` is a
    /// perfectly ordinary capture.
    #[test]
    fn non_ascii_becomes_a_json_escape() {
        assert_eq!(header_safe_json(r#"{"path":"/Café.png"}"#), r#"{"path":"/Caf\u00e9.png"}"#);
        // Plain ASCII is untouched, so the common case costs nothing.
        assert_eq!(header_safe_json(r#"{"path":"/shot.png"}"#), r#"{"path":"/shot.png"}"#);
        // 0x7F is named explicitly by the docs even though it is ASCII.
        assert_eq!(header_safe_json("a\u{7f}b"), r"a\u007fb");
    }

    /// A character outside the basic plane becomes a SURROGATE PAIR of escapes, which is
    /// what "JSON-style \uXXXX" means. Emoji in a file name are common enough that a
    /// single-unit implementation would be found by a user rather than by a test.
    #[test]
    fn an_astral_character_becomes_a_surrogate_pair() {
        // U+1F4F7 CAMERA is D83D DCF7 in UTF-16, so it is TWO escapes, not one.
        assert_eq!(header_safe_json("📷"), r"\ud83d\udcf7");
        let escaped = header_safe_json(r#"{"path":"/📷.png"}"#);
        assert!(escaped.is_ascii(), "the header must be plain ASCII: {escaped}");
        // And it is still the same document once a JSON parser has read the escapes back.
        let parsed: serde_json::Value = serde_json::from_str(&escaped).expect("valid JSON");
        assert_eq!(parsed["path"], "/📷.png");
    }

    /// The escaped form round-trips for a real commit argument, which is the whole point:
    /// Dropbox parses this header as JSON.
    #[test]
    fn an_escaped_commit_still_parses_as_the_same_json() {
        let arg = commit_arg("/Zrzuty ekranu/Zdjęcie 📸.png");
        let escaped = header_safe_json(&arg);
        assert!(escaped.is_ascii());
        let parsed: serde_json::Value = serde_json::from_str(&escaped).expect("valid JSON");
        assert_eq!(parsed["path"], "/Zrzuty ekranu/Zdjęcie 📸.png");
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    /// A real `files/upload` reply shape, with invented ids.
    #[test]
    fn an_upload_reply_parses() {
        let body = r#"{
            "name": "Screenshot 2026-08-02.png",
            "id": "id:a4ayc_80_OEAAAAAAAAAXw",
            "client_modified": "2026-08-02T15:50:38Z",
            "server_modified": "2026-08-02T15:51:30Z",
            "rev": "a1c10ce0dd78",
            "size": 7212,
            "path_lower": "/screenshots/screenshot 2026-08-02.png",
            "path_display": "/Screenshots/Screenshot 2026-08-02.png"
        }"#;
        let file = parse_metadata(body).expect("parse");
        assert_eq!(file.id, "id:a4ayc_80_OEAAAAAAAAAXw");
        assert_eq!(file.web_url, None, "an upload does not come with a link");
        assert!(parse_metadata(r#"{"name":"a.png"}"#).is_err(), "no id is a failed upload");
        assert!(parse_metadata("not json").is_err());
    }

    /// **Folders only, and the id carried forward is the PATH.** A Dropbox upload commits to
    /// a path, so remembering the entry's `id:` would give a destination that cannot be used.
    #[test]
    fn a_listing_yields_folders_keyed_by_path() {
        let body = r#"{
            "entries": [
                {".tag": "folder", "name": "Screenshots", "id": "id:a4ayc_80_OEAAAAAAAAAXz",
                 "path_lower": "/screenshots", "path_display": "/Screenshots"},
                {".tag": "file", "name": "notes.txt", "id": "id:a4ayc_80_OEAAAAAAAAAX1",
                 "path_lower": "/notes.txt", "size": 12},
                {".tag": "folder", "name": "Recordings", "id": "id:a4ayc_80_OEAAAAAAAAAX2",
                 "path_lower": "/recordings", "path_display": "/Recordings"}
            ],
            "cursor": "AAF-EXAMPLE",
            "has_more": false
        }"#;
        let folders = parse_folders(body).expect("parse");
        assert_eq!(folders.len(), 2, "the file must not appear");
        assert_eq!(folders[0].name, "Screenshots");
        assert_eq!(folders[0].id, "/screenshots", "the id must be the path, not id:...");
        assert_eq!(folders[1].id, "/recordings");
        assert!(parse_folders(r#"{"entries":[]}"#).expect("parse").is_empty());
        assert!(parse_folders("not json").is_err());
    }

    /// The share link, and the already-shared fallback.
    #[test]
    fn share_link_replies_parse() {
        let created = r#"{
            ".tag": "file",
            "url": "https://www.dropbox.com/scl/fi/EXAMPLE/shot.png?rlkey=abc123&dl=0",
            "name": "shot.png",
            "path_lower": "/screenshots/shot.png",
            "link_permissions": {"resolved_visibility": {".tag": "public"}}
        }"#;
        assert!(read_link(created).expect("parse").contains("dropbox.com/scl/fi/EXAMPLE"));

        let existing = r#"{
            "links": [
                {".tag": "file", "url": "https://www.dropbox.com/scl/fi/OLD/shot.png?rlkey=xyz&dl=0",
                 "name": "shot.png", "path_lower": "/screenshots/shot.png"}
            ],
            "has_more": false
        }"#;
        assert!(parse_existing_link(existing).expect("parse").contains("scl/fi/OLD"));
        // No link is a real failure: we told the user it was shared.
        assert!(parse_existing_link(r#"{"links":[]}"#).is_err());
        assert!(parse_existing_link(r#"{"links":[{"name":"x"}]}"#).is_err());
        assert!(parse_existing_link("not json").is_err());
    }

    /// **The 409 tag.** The already-shared case is recognised by `error[".tag"]` and never by
    /// `error_summary`, which Dropbox pads with a random number of dots precisely to break
    /// exact matching.
    #[test]
    fn the_already_shared_error_is_recognised_by_its_tag() {
        let body = r#"{
            "error_summary": "shared_link_already_exists/...",
            "error": {".tag": "shared_link_already_exists"}
        }"#;
        assert_eq!(error_tag(body).as_deref(), Some("shared_link_already_exists"));
        // A different endpoint error is not mistaken for it.
        let conflict = r#"{"error_summary":"path/conflict/file/..","error":{".tag":"path","path":{".tag":"conflict"}}}"#;
        assert_eq!(error_tag(conflict).as_deref(), Some("path"));
        assert_eq!(error_tag("not json"), None);
        assert_eq!(error_tag("{}"), None);
        // The padded summary really does vary, which is why it is never matched on.
        assert_ne!(
            r#"shared_link_already_exists/..."#,
            r#"shared_link_already_exists/."#,
            "the dots are random padding, so equality on this field is a bug"
        );
    }
}

#[cfg(test)]
mod folder_manager_tests {
    use super::*;

    /// A real `list_folder` page carrying more, then the page that ends it. **`has_more` is the
    /// authority**: Dropbox returns a cursor on every page, so a loop keyed on the cursor's
    /// presence alone would never stop.
    #[test]
    fn a_listing_follows_its_cursor_only_while_dropbox_says_there_is_more() {
        let page_one = r#"{
            "entries": [
                {".tag": "folder", "name": "Screenshots", "id": "id:aBcDeFg", "path_lower": "/screenshots", "path_display": "/Screenshots"},
                {".tag": "file", "name": "note.txt", "id": "id:hIjKlMn", "path_lower": "/note.txt"}
            ],
            "cursor": "AAHqBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789",
            "has_more": true
        }"#;
        assert_eq!(next_cursor(page_one).as_deref(), Some("AAHqBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789"));
        // The FILE in the same page is still filtered out, so paging only appends folders.
        let folders = parse_folders(page_one).expect("parse");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].id, "/screenshots", "a Dropbox destination is remembered as a path");
        let last = r#"{"entries":[],"cursor":"AAHqZzZz","has_more":false}"#;
        assert_eq!(next_cursor(last), None, "a cursor without has_more is not another page");
        assert_eq!(next_cursor(r#"{"entries":[],"cursor":"","has_more":true}"#), None);
        assert_eq!(next_cursor("not json"), None);
    }

    /// **A missing path is a 409, not a 404**, and the NESTED tag is what says which kind of
    /// path error it is: `path` alone also covers restricted content and a malformed path,
    /// neither of which is a folder having gone away.
    #[test]
    fn a_deleted_folder_is_told_apart_from_other_path_errors() {
        let gone = r#"{
            "error_summary": "path/not_found/...",
            "error": {".tag": "path", "path": {".tag": "not_found"}}
        }"#;
        assert!(is_path_not_found(gone));
        assert!(!is_path_conflict(gone));
        let taken = r#"{
            "error_summary": "path/conflict/folder/.",
            "error": {".tag": "path", "path": {".tag": "conflict", "conflict": {".tag": "folder"}}}
        }"#;
        assert!(is_path_conflict(taken));
        assert!(!is_path_not_found(taken));
        for other in [
            r#"{"error":{".tag":"path","path":{".tag":"malformed_path"}}}"#,
            r#"{"error":{".tag":"path","path":{".tag":"restricted_content"}}}"#,
            r#"{"error":{".tag":"other"}}"#,
            "{}",
            "not json",
        ] {
            assert!(!is_path_not_found(other), "{other}");
            assert!(!is_path_conflict(other), "{other}");
        }
        // The summary is never read: Dropbox pads it with a random number of dots on purpose.
        assert_eq!(error_tag(gone).as_deref(), Some("path"));
    }

    /// A real `create_folder_v2` reply. The folder rides inside a `metadata` object, unlike the
    /// flat entry an upload answers with, and the id carried forward is the PATH, because that
    /// is what a Dropbox destination has to be remembered as.
    #[test]
    fn a_created_folder_parses_out_of_its_metadata_wrapper() {
        let body = r#"{
            "metadata": {
                "name": "Screenshots",
                "id": "id:aBcDeFgHiJkLmNoPqRsTuVw",
                "path_lower": "/screenshots",
                "path_display": "/Screenshots"
            }
        }"#;
        let made = parse_created_folder(body, "Screenshots", "/Screenshots").expect("parse");
        assert_eq!(
            made,
            RemoteFolder { id: "/screenshots".to_string(), name: "Screenshots".to_string() }
        );
        // A reply with no metadata at all still resolves: this app built the path itself, and
        // Dropbox lowercases it, so the fallback is the same value the listing would report.
        let bare = parse_created_folder("{}", "Screenshots", "/Work/Screenshots").expect("parse");
        assert_eq!(bare.id, "/work/screenshots");
        assert_eq!(bare.name, "Screenshots");
        assert!(parse_created_folder("not json", "x", "/x").is_err());
    }

    /// The path a new folder is created at is the SAME builder an upload uses, so a folder made
    /// while looking at a level and a capture uploaded into it cannot end up in two places.
    ///
    /// **This is why Dropbox was the one provider DRAGON-520 did not touch.** The app folder is
    /// the empty path, so the top level needs no lookup and no alias: `/Name` at the top and
    /// `/Parent/Name` inside a subfolder come out of one line of arithmetic.
    #[test]
    fn a_new_folder_is_created_at_the_level_being_viewed() {
        assert_eq!(upload_path(None, "Screenshots"), "/Screenshots");
        assert_eq!(upload_path(Some(""), "Screenshots"), "/Screenshots");
        assert_eq!(upload_path(Some("/work"), "Screenshots"), "/work/Screenshots");
        assert_eq!(upload_path(Some("/work/2026"), "August"), "/work/2026/August");
    }

    /// The create and delete ARGUMENTS, at the top level and one level deep (DRAGON-520).
    ///
    /// Dropbox addresses both by path, so the two depths differ only in the path, and a delete
    /// takes whatever the listing reported as the folder's `path_lower`.
    #[test]
    fn the_create_and_delete_arguments_carry_the_level_they_act_on() {
        let arg = |json: String| {
            serde_json::from_str::<serde_json::Value>(&json).expect("valid JSON")
        };
        // The TOP level: a path directly under the app folder's own empty root.
        let top = arg(create_folder_arg(&upload_path(None, "Screenshots")));
        assert_eq!(top["path"], "/Screenshots");
        assert_eq!(top["autorename"], serde_json::json!(false), "a folder must not be renamed");
        // One level deep.
        let deep = arg(create_folder_arg(&upload_path(Some("/screenshots"), "2026")));
        assert_eq!(deep["path"], "/screenshots/2026");
        assert_eq!(deep["autorename"], serde_json::json!(false));
        // Deleting either one names the path the listing handed over, and nothing else.
        assert_eq!(arg(delete_arg("/screenshots"))["path"], "/screenshots");
        assert_eq!(arg(delete_arg("/screenshots/2026"))["path"], "/screenshots/2026");
        assert_eq!(
            arg(delete_arg("/screenshots")).as_object().map(serde_json::Map::len),
            Some(1),
            "a delete carries the path and nothing that could change what it means"
        );
    }
}
