//! YouTube, through the YouTube Data API v3 (DRAGON-493).
//!
//! The first VIDEO-HOSTING provider in this app, not a file-storage drive like its three
//! siblings: there is no folder to choose (`ProviderCaps::folder_browse` is false), the
//! "share link" is simply the video's own watch page (no separate API call the way Drive's
//! sharing permission is), and the upload carries a VISIBILITY as metadata
//! (`status.privacyStatus`) rather than something set after the fact. See
//! [`crate::cloud::accounts::Visibility`] for the portable vocabulary that field is stored as.
//!
//! Verified against `developers.google.com/youtube/v3/docs/videos` (the `videos` resource,
//! `insert` and `delete` methods) and Google's general resumable-upload protocol doc (the SAME
//! one `providers::gdrive` verifies against; YouTube's `videos.insert` uses it too, just on a
//! different host path and with different metadata).
//!
//! # Why this file duplicates `gdrive.rs`'s resumable-upload shape instead of sharing it
//!
//! Google's resumable-upload protocol (start a session, get a `Location` back, `PUT` chunks,
//! read `Range` to see what was actually taken) is the SAME mechanism both providers speak, so
//! the chunk loop here looks a lot like `providers::gdrive`'s. It is a deliberate, small
//! duplication rather than a shared helper: the two providers' metadata shapes, endpoints and
//! response bodies differ enough that a shared function would need its own parameters for all
//! of that anyway, and keeping this file self-contained means a change to one provider's upload
//! path can never accidentally perturb the other's.
//!
//! # Same Google OAuth client as Drive, different registry row
//!
//! `youtube.force-ssl` (not the narrower `youtube.upload`) is the scope this row requests:
//! `videos.delete` refuses a token carrying only `youtube.upload`, and this app has to be able
//! to take a video back off the channel when the user cancels an upload that already committed
//! (DRAGON-496) or undoes one during the meter's finish-hold (DRAGON-507).
//! A from-source build can reuse the SAME Google Cloud project/OAuth client its Drive
//! setup already created (just enabling the YouTube Data API v3 and requesting this extra
//! scope), or register a separate one; either way this is its own [`super::super::ProviderSpec`]
//! row with its own env vars (`CCK_YOUTUBE_CLIENT_ID`/`_SECRET`), so connecting YouTube is
//! always a separate account from Drive, never implied by having Drive connected.
//!
//! # Unverified projects: uploads are forced Private (read this before filing a bug)
//!
//! Per YouTube's own API revision history, a video uploaded through `videos.insert` from an
//! OAuth client belonging to an UNVERIFIED Google Cloud project (any project created after
//! 2020-07-28 that has not been through Google's audit process) is forced to `private`
//! regardless of what `status.privacyStatus` asked for. A from-source builder who just
//! registered a fresh project will see every upload land Private no matter which visibility
//! they pick in the editor's dropdown, until that project completes the audit. This is a real,
//! documented Google-side restriction, not a bug in this file; see `CLOUD_ACCOUNTS.md`.

use std::path::Path;

use super::{
    META_BUDGET, ProviderOps, RemoteFile, RemoteFolder, TempChunk, api, api_failure, bearer,
    content_range, file_size, progress_percent, remote_name, upload_budget,
};
use crate::cloud::accounts::{CloudAccount, Visibility};
use crate::cloud::http::{Body, CurlResponse, Method};

/// Every https host this provider reaches. Folded into [`super::api_hosts`]. The SAME two
/// Google hosts `providers::gdrive` uses (this is the same Google API family), listed again
/// here rather than imported so this file has no compile-time dependency on that one.
pub(super) const HOSTS: &[&str] = &["www.googleapis.com", "oauth2.googleapis.com"];

/// The Data API v3, for everything except the upload itself.
const API: &str = "https://www.googleapis.com/youtube/v3";

/// The upload API. A different path prefix from [`API`], same host, same shape as Drive's own
/// `UPLOAD_API` (`developers.google.com/youtube/v3/guides/using_resumable_upload_protocol`).
const UPLOAD_API: &str = "https://www.googleapis.com/upload/youtube/v3/videos";

/// Where a token is revoked. The SAME Google endpoint Drive uses; one access token revokes the
/// whole grant regardless of which of this app's registry rows requested it.
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";

/// The answer to every folder question this provider is asked (DRAGON-506).
///
/// One sentence rather than three, because there is one fact behind all of them: a video goes to
/// a channel, and a channel has no folders to list, make or delete. Stated once so the three
/// guards cannot drift into three different explanations of the same thing.
const NO_FOLDERS: &str = "YouTube does not organize uploads into folders.";

/// The status Google's resumable protocol returns for "chunk accepted, send the next one".
const RESUME_INCOMPLETE: u16 = 308;

/// The chunk size the resumable path uses. Same value and same reasoning as Drive's own
/// `RESUMABLE_CHUNK_BYTES`: a multiple of [`RESUMABLE_CHUNK_UNIT`], and large enough to keep the
/// round-trip count reasonable without re-sending too much after a failure.
pub const RESUMABLE_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// The unit Google requires a non-final chunk to be a multiple of. Documented for
/// `videos.insert` the same way it is for Drive's `files.create`.
pub const RESUMABLE_CHUNK_UNIT: u64 = 256 * 1024;

const _: () = assert!(
    RESUMABLE_CHUNK_BYTES.is_multiple_of(RESUMABLE_CHUNK_UNIT),
    "DRAGON-493: a YouTube resumable chunk must be a multiple of 256 KiB, or the upload fails \
     when it is committed rather than when it is sent"
);

/// How many times in a row a chunk may fail to advance the offset before giving up. Same value
/// and reasoning as Drive's own bound: one unlucky answer must not end an upload, and a session
/// that never advances must not grind on forever.
const MAX_STALLED_ROUNDS: u32 = 3;

pub(super) struct YouTube;

impl ProviderOps for YouTube {
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
        // The visibility is read straight off the account (DRAGON-493): the editor's Upload
        // dropdown writes it there the same way it already writes `folder_id` for the
        // file-storage providers, so no new argument or argv plumbing is needed to carry a
        // per-upload choice into this detached child.
        let visibility = acct.visibility.unwrap_or_else(Visibility::default_choice);
        let metadata = create_metadata(&video_title(&name), "", visibility);
        let mime = content_type_for(&name);
        resumable_upload(token, file, &metadata, mime, size, on_progress, should_cancel)
    }

    /// No separate API call: a video's watch page IS its own share link, so this just formats
    /// the URL from the id the upload already returned. `token` is unused, since nothing here
    /// asks the network for anything.
    fn create_share_link(&self, _token: &str, file_id: &str) -> Result<String, String> {
        Ok(watch_url(file_id))
    }

    /// YouTube has no folder concept at all; `ProviderCaps::folder_browse` is false for this
    /// provider, so no UI path calls this in practice. Defensive rather than `unreachable!`,
    /// since a future call site is a much cheaper mistake to make survivable than to panic on.
    fn list_folders(&self, _token: &str, _parent: Option<&str>) -> Result<Vec<RemoteFolder>, String> {
        Err(NO_FOLDERS.to_string())
    }

    /// Nothing to make one inside of. Same reasoning as [`Self::list_folders`]: the folder
    /// manager the setup step grew in DRAGON-506 is gated on the same capability that hides the
    /// list, so this is a guard for a state the UI does not offer.
    fn create_folder(
        &self,
        _token: &str,
        _parent: Option<&str>,
        _name: &str,
    ) -> Result<RemoteFolder, String> {
        Err(NO_FOLDERS.to_string())
    }

    fn delete_folder(&self, _token: &str, _folder_id: &str) -> Result<(), String> {
        Err(NO_FOLDERS.to_string())
    }

    fn delete_file(&self, token: &str, file_id: &str) -> Result<(), String> {
        let url = format!("{API}/videos?id={}", esc(file_id));
        let response = bearer(api(Method::Delete, &url, META_BUDGET)?, token).send()?;
        if !response.is_success() {
            return Err(api_failure("youtube", "delete that video", &response));
        }
        Ok(())
    }

    /// "The token can be an access token or a refresh token... the refresh token will also be
    /// revoked" (the same Google revoke semantics Drive's own `revoke` relies on), so one call
    /// with the access token drops the whole grant, YouTube scope included.
    fn revoke(&self, token: &str) -> Result<(), String> {
        let response = api(Method::Post, REVOKE_URL, META_BUDGET)?
            .form_field("token", token)
            .no_retry()
            .send()?;
        if !response.is_success() {
            return Err(api_failure("youtube", "sign this account out", &response));
        }
        Ok(())
    }
}

/// Start a resumable session, then feed it chunks until YouTube commits the video. The loop is
/// driven by the SERVER's own reported offset, never a precomputed plan, for the same reason
/// Drive's resumable loop is: after each `308` YouTube reports what it actually holds in a
/// `Range` header, and resuming from anywhere else would leave a hole in the file or re-send
/// bytes it already has.
fn resumable_upload(
    token: &str,
    file: &Path,
    metadata: &str,
    mime: &str,
    size: u64,
    on_progress: &mut dyn FnMut(u8),
    should_cancel: &dyn Fn() -> bool,
) -> Result<RemoteFile, String> {
    let session = begin_session(token, metadata, mime, size)?;
    log::debug!("cloud youtube: uploading {size} bytes in a resumable session");

    let mut offset = 0u64;
    let mut stalled = 0u32;
    while offset < size {
        // Checked BETWEEN chunks, the natural pause: the loop already stops here between one
        // request and the next (DRAGON-490's cancel contract, the same shape every provider's
        // upload loop follows).
        // Nothing to tell YouTube: like Drive, an abandoned resumable session expires on its
        // own and leaves no visible video behind (DRAGON-495).
        if should_cancel() {
            return Err(super::canceled_err());
        }
        let len = RESUMABLE_CHUNK_BYTES.min(size - offset);
        let chunk = TempChunk::write(file, offset, len)?;
        let response = put_chunk(&session, token, &chunk, offset, len, size)?;

        if response.is_success() {
            let uploaded = parse_video(&response.text())?;
            on_progress(100);
            log::debug!("cloud youtube: finished a {size} byte resumable upload");
            return Ok(uploaded);
        }
        if response.status != RESUME_INCOMPLETE {
            return Err(api_failure("youtube", "upload part of that video", &response));
        }

        let confirmed = response.header("Range").and_then(parse_range_end);
        let next = match confirmed {
            Some(end) => end.saturating_add(1).min(size),
            None => offset,
        };
        if next <= offset {
            stalled += 1;
            if stalled >= MAX_STALLED_ROUNDS {
                log::warn!("cloud youtube: the upload stopped advancing at {stalled} rounds");
                return Err(
                    "YouTube stopped accepting this upload part way through. Try again.".to_string(),
                );
            }
        } else {
            stalled = 0;
            offset = next;
            on_progress(progress_percent(offset, size));
        }
    }
    Err("YouTube accepted the whole video but did not confirm it was saved.".to_string())
}

/// Open a resumable session and return its URI.
///
/// The URI is a CREDENTIAL (it authorizes writing to this account's channel) and is never
/// logged. `X-Upload-Content-Length` lets YouTube reject an over-quota upload before a byte is
/// sent rather than at the end, the same as Drive's own session start.
fn begin_session(token: &str, metadata: &str, mime: &str, size: u64) -> Result<String, String> {
    let url = format!("{UPLOAD_API}?uploadType=resumable&part=snippet,status");
    let response = bearer(api(Method::Post, &url, META_BUDGET)?, token)
        .header("Content-Type", "application/json; charset=UTF-8")
        .header("X-Upload-Content-Type", mime)
        .header("X-Upload-Content-Length", &size.to_string())
        .body(Body::Text(metadata.to_string()))
        // The whole point: the session URI comes back as a header, not in the body.
        .capture_headers()
        .no_retry()
        .send()?;
    if !response.is_success() {
        return Err(api_failure("youtube", "start that upload", &response));
    }
    let session = response
        .header("Location")
        .filter(|uri| !uri.is_empty())
        .ok_or_else(|| "YouTube did not return an address to upload to.".to_string())?;
    // Checked against GOOGLE'S OWN hosts before a byte of the user's recording is sent to it
    // (the same reasoning as Drive's own check): the session URI comes from the network, and a
    // shared app-wide allowlist would accept it pointing at some other provider's API instead.
    let host = crate::cloud::http::host_of(session)
        .ok_or_else(|| "YouTube returned an upload address this app cannot use.".to_string())?;
    if !is_upload_session_host(host) {
        log::warn!("cloud youtube: refused an upload session on the unexpected host {host}");
        return Err(
            "YouTube returned an upload address this app does not recognise.".to_string(),
        );
    }
    Ok(session.to_string())
}

/// Whether a host may receive resumable session chunks. Pure; unit-tested.
///
/// The same rule `providers::gdrive::is_upload_session_host` applies, kept as its own copy in
/// this file (see the module doc for why): Google's resumable `Location` for this API is on
/// `www.googleapis.com` in every documented example, with the `.googleapis.com` suffix allowed
/// on top of it since Google routes some upload traffic through per-service subdomains and does
/// not call the URI contractual. The leading dot is what stops a lookalike registered as
/// `evilgoogleapis.com` from matching.
pub fn is_upload_session_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    HOSTS.iter().any(|h| *h == host) || host.ends_with(".googleapis.com")
}

/// Send one chunk, with one retry when repeating could help. Safe to repeat: a chunk is
/// addressed by its `Content-Range`, so re-sending the same range writes the same bytes to the
/// same place, and YouTube answers a range it already holds with a `308` naming what it has.
fn put_chunk(
    session: &str,
    token: &str,
    chunk: &TempChunk,
    offset: u64,
    len: u64,
    total: u64,
) -> Result<CurlResponse, String> {
    let attempt = || -> Result<CurlResponse, String> {
        bearer(api(Method::Put, session, upload_budget(len))?, token)
            .header("Content-Range", &content_range(offset, len, total))
            .body(Body::File(chunk.path().to_path_buf()))
            // `Range` says how much YouTube actually took.
            .capture_headers()
            .no_retry()
            .send()
    };
    let first = attempt()?;
    if first.status == 0 || first.status >= 500 {
        log::debug!("cloud youtube: a chunk came back {}; sending it once more", first.status);
        return attempt();
    }
    Ok(first)
}

/// The last byte YouTube confirms it holds, from a `308`'s `Range` header. Pure; unit-tested.
/// Same inclusive-upper-bound reading as Drive's own `parse_range_end`.
pub fn parse_range_end(value: &str) -> Option<u64> {
    let range = value.trim().strip_prefix("bytes=")?;
    let (_first, last) = range.rsplit_once('-')?;
    last.trim().parse::<u64>().ok()
}

/// `https://www.youtube.com/watch?v=<id>` is the standard, universally-reliable watch URL
/// shape; it is not itself a field the API returns (there is no Drive-style `webViewLink`
/// equivalent here), so it is built from the video id the upload already produced. Pure;
/// unit-tested.
fn watch_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

/// The title a video gets, derived from the remote name every provider's upload already
/// computes ([`remote_name`]). Pure; unit-tested.
///
/// The extension is stripped: a video title ending in ".mp4" reads as a mistake in a way that a
/// FILE named "clip.mp4" does not (Drive/OneDrive/Dropbox keep the extension because it names a
/// real file in a drive; a YouTube title is prose, not a filename).
fn video_title(remote_name: &str) -> String {
    match remote_name.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
        _ => remote_name.to_string(),
    }
}

/// The media type for a capture, from its name. Pure; unit-tested.
///
/// Only the video types this app can actually produce and hand to this provider (`accepts_images`
/// is false for YouTube, so a still never reaches here); anything else is the honest fallback
/// rather than a guess, the same posture `providers::gdrive::content_type_for` takes for its own
/// wider set.
fn content_type_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// The `videos.insert` metadata document. Pure; unit-tested.
///
/// `status.selfDeclaredMadeForKids` is ALWAYS sent explicitly, `false`: YouTube's own upload
/// guidance (community-reported, not a documented hard failure, but safer to assume) says a
/// video with this field omitted can be left blocked from normal viewing until fixed by hand in
/// Studio. This app has no UI asking a user to declare otherwise, so the honest default for a
/// screen-recording tool is "not made for kids".
fn create_metadata(title: &str, description: &str, visibility: Visibility) -> String {
    let mut snippet = serde_json::Map::new();
    snippet.insert("title".to_string(), serde_json::Value::String(title.to_string()));
    snippet.insert(
        "description".to_string(),
        serde_json::Value::String(description.to_string()),
    );

    let mut status = serde_json::Map::new();
    status.insert(
        "privacyStatus".to_string(),
        serde_json::Value::String(visibility_wire(visibility).to_string()),
    );
    status.insert("selfDeclaredMadeForKids".to_string(), serde_json::Value::Bool(false));

    let mut doc = serde_json::Map::new();
    doc.insert("snippet".to_string(), serde_json::Value::Object(snippet));
    doc.insert("status".to_string(), serde_json::Value::Object(status));
    serde_json::Value::Object(doc).to_string()
}

/// [`Visibility`] as YouTube's `status.privacyStatus` spells it. Pure; unit-tested. The API's
/// own documented enum is exactly these three lowercase strings.
fn visibility_wire(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Public => "public",
        Visibility::Unlisted => "unlisted",
        Visibility::Private => "private",
    }
}

/// Percent-encode a value for a URL query. Reuses the OAuth encoder so there is one
/// implementation in `cloud/`, the same as `providers::gdrive`'s own `esc`.
fn esc(value: &str) -> String {
    crate::cloud::oauth::percent_encode(value)
}

/// One `videos` resource, as much of it as we use.
#[derive(serde::Deserialize)]
struct VideoResource {
    id: Option<String>,
}

/// Read an uploaded video out of a `videos.insert` reply. Pure; unit-tested.
fn parse_video(body: &str) -> Result<RemoteFile, String> {
    let parsed: VideoResource = serde_json::from_str(body)
        .map_err(|_| "YouTube's reply after the upload could not be read.".to_string())?;
    let id = parsed
        .id
        .filter(|i| !i.is_empty())
        .ok_or_else(|| "YouTube did not say where it put the video.".to_string())?;
    let web_url = Some(watch_url(&id));
    Ok(RemoteFile { id, web_url })
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    /// The metadata carries the exact three fields this file depends on, spelled exactly as
    /// the API documents them, and always declares the kids field rather than leaving it out.
    #[test]
    fn the_metadata_carries_title_description_and_visibility() {
        let doc = create_metadata("My Clip", "made with Cosmic Capture Kit", Visibility::Unlisted);
        let parsed: serde_json::Value = serde_json::from_str(&doc).expect("valid JSON");
        assert_eq!(parsed["snippet"]["title"], "My Clip");
        assert_eq!(parsed["snippet"]["description"], "made with Cosmic Capture Kit");
        assert_eq!(parsed["status"]["privacyStatus"], "unlisted");
        assert_eq!(parsed["status"]["selfDeclaredMadeForKids"], false);
    }

    /// A title or description with a quote or backslash cannot break the hand-built JSON: it is
    /// serialized through `serde_json::Value`, never interpolated as a raw string.
    #[test]
    fn a_hostile_title_cannot_break_the_metadata() {
        let doc = create_metadata(r#"a"b\c"#, "", Visibility::Private);
        let parsed: serde_json::Value = serde_json::from_str(&doc).expect("still valid JSON");
        assert_eq!(parsed["snippet"]["title"], "a\"b\\c");
    }

    /// The API's own documented enum, exactly: lowercase, and only these three strings.
    #[test]
    fn visibility_maps_to_the_apis_exact_enum() {
        assert_eq!(visibility_wire(Visibility::Public), "public");
        assert_eq!(visibility_wire(Visibility::Unlisted), "unlisted");
        assert_eq!(visibility_wire(Visibility::Private), "private");
    }
}

#[cfg(test)]
mod naming_tests {
    use super::*;

    /// A title reads as prose, not a filename, so the extension comes off; a name with no
    /// extension at all (or nothing BUT an extension) is left as-is rather than guessed at.
    #[test]
    fn the_title_drops_the_extension() {
        assert_eq!(video_title("Screen Recording 2026-08-03.mp4"), "Screen Recording 2026-08-03");
        assert_eq!(video_title("clip.webm"), "clip");
        assert_eq!(video_title("clip.MP4"), "clip", "the stem survives regardless of case");
        assert_eq!(video_title("no-extension"), "no-extension");
        assert_eq!(video_title(".hidden"), ".hidden", "a leading dot with nothing after is not an extension to strip");
    }

    /// Only the video types this provider ever receives (`accepts_images` is false); anything
    /// else is the honest fallback.
    #[test]
    fn the_media_type_matches_a_video_capture() {
        assert_eq!(content_type_for("clip.mp4"), "video/mp4");
        assert_eq!(content_type_for("clip.MP4"), "video/mp4");
        assert_eq!(content_type_for("clip.webm"), "video/webm");
        assert_eq!(content_type_for("clip.mkv"), "video/x-matroska");
        assert_eq!(content_type_for("clip.mov"), "video/quicktime");
        assert_eq!(content_type_for("shot.png"), "application/octet-stream", "a still is not this provider's job");
        assert_eq!(content_type_for(""), "application/octet-stream");
    }

    /// The watch URL is the ONE address this provider ever hands back, and it is built the
    /// same way every time, from just the video id.
    #[test]
    fn the_watch_url_is_built_from_the_video_id() {
        assert_eq!(watch_url("dQw4w9WgXcQ"), "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
    }
}

#[cfg(test)]
mod upload_shape_tests {
    use super::*;

    /// `create_share_link` and `list_folders` do not touch the network at all; the former
    /// always succeeds (it is pure string formatting), the latter always refuses.
    #[test]
    fn share_link_and_folder_listing_never_touch_the_network() {
        let yt = YouTube;
        assert_eq!(
            yt.create_share_link("unused-token", "abc123").unwrap(),
            "https://www.youtube.com/watch?v=abc123"
        );
        assert!(yt.list_folders("unused-token", None).is_err());
    }
}

#[cfg(test)]
mod resumable_tests {
    use super::*;
    use crate::cloud::http::parse_headers;

    fn response(status: u16, dump: &str) -> CurlResponse {
        CurlResponse { status, body: Vec::new(), headers: parse_headers(dump) }
    }

    /// The session URI is read from the `Location` header, the same shape Drive's own
    /// resumable start uses, and it is checked against Google's own hosts.
    #[test]
    fn the_session_uri_is_read_from_the_location_header() {
        let dump = "HTTP/1.1 100 Continue\r\n\r\n\
                    HTTP/1.1 200 OK\r\n\
                    Location: https://www.googleapis.com/upload/youtube/v3/videos?uploadType=resumable&upload_id=AEnB2Uo\r\n\
                    Content-Length: 0\r\n\r\n";
        let session = response(200, dump).header("Location").expect("a session URI").to_string();
        assert!(session.contains("upload_id=AEnB2Uo"));
        let host = crate::cloud::http::host_of(&session).expect("a host");
        assert!(is_upload_session_host(host), "{host} must be accepted");
    }

    /// A session URI on another provider's host, or a lookalike, is refused: the shared
    /// allowlist would accept the first of these, which is exactly why this check is
    /// per-provider.
    #[test]
    fn a_session_uri_on_another_providers_host_is_refused() {
        for host in ["www.googleapis.com", "oauth2.googleapis.com", "storage.googleapis.com"] {
            assert!(is_upload_session_host(host), "{host} must be accepted");
        }
        for host in [
            "graph.microsoft.com",
            "content.dropboxapi.com",
            "evilgoogleapis.com",
            "www.googleapis.com.evil.test",
            "evil.test",
            "",
        ] {
            assert!(!is_upload_session_host(host), "{host} must be refused");
        }
        assert!(is_upload_session_host("WWW.GoogleAPIs.COM"));
    }

    /// `Range: bytes=0-262143` means "YouTube holds through byte 262143", so the next chunk
    /// starts at 262144. The inclusive reading, same as Drive's own `parse_range_end`.
    #[test]
    fn the_range_header_gives_the_last_byte_held() {
        assert_eq!(parse_range_end("bytes=0-262143"), Some(262_143));
        assert_eq!(parse_range_end("bytes=0-0"), Some(0));
        let next = parse_range_end("bytes=0-8388607").expect("an end") + 1;
        assert_eq!(next, RESUMABLE_CHUNK_BYTES, "a full 8 MiB chunk was taken");
    }

    /// A missing or unusable `Range` is not treated as progress: on the first chunk it
    /// legitimately means "I have nothing yet".
    #[test]
    fn an_unusable_range_is_not_progress() {
        for bad in ["", "0-100", "bytes=", "bytes=0-", "bytes=abc", "items=0-5"] {
            assert_eq!(parse_range_end(bad), None, "{bad:?} must not parse");
        }
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    /// A real `videos.insert` reply shape, with an invented id.
    #[test]
    fn an_upload_reply_parses() {
        let body = r#"{"kind":"youtube#video","id":"dQw4w9WgXcQ","snippet":{"title":"My Clip"}}"#;
        let video = parse_video(body).expect("parse");
        assert_eq!(video.id, "dQw4w9WgXcQ");
        assert_eq!(video.web_url.as_deref(), Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
    }

    /// Without an id there is nothing to share, delete or record, so the upload is a failure
    /// even though the HTTP call itself succeeded.
    #[test]
    fn an_upload_reply_without_an_id_is_a_failure() {
        assert!(parse_video(r#"{"snippet":{"title":"a"}}"#).is_err());
        assert!(parse_video(r#"{"id":""}"#).is_err());
        assert!(parse_video("not json").is_err());
    }
}
