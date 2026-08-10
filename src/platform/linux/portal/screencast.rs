//! xdg-desktop-portal ScreenCast session: request a PipeWire stream for a chosen
//! source type (monitor or window), returning the remote fd + stream node(s) so we
//! can consume frames in-process ([`crate::platform::pipewire`]). Backs the "Prefer
//! PipeWire" recording path; we fall back to direct screencopy when it fails.
//!
//! # Why the persist mode is `ExplicitlyRevoked` (mode 2), not `Application` (mode 1)
//!
//! DRAGON-570, root-caused by the DRAGON-552 research. Mode 1 sounds like the
//! gentler choice ("persist while the application is running") and it is exactly
//! wrong for this app: xdg-desktop-portal keeps mode-1 grants in a PROCESS-LOCAL
//! table keyed by the requester's D-Bus connection and deletes them when that
//! connection closes. Our capture children are one-shot processes (launch, capture,
//! exit), so the connection dies with every capture and the restore token we
//! persisted is already dead the next time any process replays it. The visible
//! symptom: EVERY capture prompted for the portal grant again, with the token
//! round-tripping uselessly through the config file.
//!
//! Mode 2 ("persist until explicitly revoked") writes the grant to the on-disk
//! xdg-permission-store, which survives process exit, logout and reboot. That is
//! the mode every other one-shot or restart-prone screen tool uses; OBS held 28
//! such entries on the development machine when this was diagnosed. COSMIC's
//! backend ignores `persist_mode` and always returns restore data; persistence is
//! the FRONTEND's job (xdg-desktop-portal itself), so mode 2 works regardless of
//! which backend answers.
//!
//! Do not revert this to mode 1 because it "sounds less scary": mode 1 can never
//! work under a one-shot process model, on any backend. The user-facing escape
//! hatch is the settings page's Forget button (clears our replay tokens) plus
//! `flatpak permission-remove screencast dev.thedragon.CosmicCaptureKit` for the
//! portal's own store entry (that CLI manages the shared permission store for
//! host apps too, not only Flatpaks). Nothing here keys on sandbox detection:
//! every Linux build that routes capture through the portal (chosen in settings,
//! or clamped there where native screencopy is absent) behaves identically.
//!
//! # The cursor is THIS backend's business (DRAGON-592)
//!
//! The stream's cursor mode is the whole of how the portal backend honours
//! "Preserve mouse cursor" (`Caps::cursor_toggle`). The shared tree hands this
//! module a [`CursorRequest`], meaning nothing more than "the user wants the
//! pointer, yes or no", and [`cursor_mode`] turns that into the portal's own
//! vocabulary. No caller mentions `CursorMode`, and no caller needs to know that
//! the portal's mechanism is include-or-omit rather than a sprite.
//!
//! This used to be a constant. `request` passed `CursorMode::Embedded` on every
//! call, so a portal capture ALWAYS carried the pointer, whatever the user had
//! set, and the setting was not even offered (the capability bit was named for
//! the native sprite session, so the portal reported it false and the settings
//! row never rendered). The colour picker paid worst: the portal's permission
//! dialog forces the pointer onto its OK button, so that sprite landed in the
//! picker's frozen snapshot and permanently covered real pixels for the whole
//! session. The picker now always asks for [`CursorRequest::Omit`].

use ashpd::desktop::PersistMode;
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use std::os::fd::OwnedFd;

/// One stream the portal granted: its PipeWire node id and (for monitors) global
/// logical position/size, used to validate a region falls inside it and to crop.
pub struct StreamInfo {
    pub node_id: u32,
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

/// A live ScreenCast session: the PipeWire remote fd, the granted streams, and a
/// restore token (persist it to skip the dialog next time for the same source).
pub struct CastSession {
    pub fd: OwnedFd,
    pub streams: Vec<StreamInfo>,
    pub restore_token: Option<String>,
}

/// Why a cast request didn't yield a stream.
pub enum CastError {
    /// The user dismissed/denied the portal dialog — abort (do NOT fall back, since
    /// recording anyway would defy the explicit cancel).
    Cancelled,
    /// The portal/PipeWire couldn't be reached or negotiation failed — the caller
    /// should fall back to direct screencopy so recording still happens.
    Unavailable(String),
}

/// What a ScreenCast request wants done with the pointer (DRAGON-592).
///
/// A named pair rather than a bare `bool`, because this is a trailing argument of
/// [`request`] read at four call sites across two files: `CursorRequest::Omit` at
/// the colour picker says what it means and why, where a `false` would only invite
/// someone to pass it backwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorRequest {
    /// Bake the pointer into the stream, honouring "Preserve mouse cursor" ON.
    Keep,
    /// Leave the pointer out: either the preference is off, or the caller is the
    /// colour picker, whose snapshot is a measurement instrument rather than a
    /// picture.
    Omit,
}

/// Pure, unit-tested (`cursor_mode_tests`): the portal cursor mode a request asks
/// for.
///
/// Two of the portal's three modes are the whole mechanism here. `Embedded` bakes
/// the pointer into the stream buffers and `Hidden` leaves it out, which is exactly
/// the include-or-omit capability the backend advertises.
///
/// `CursorMode::Metadata` is deliberately unused. It keeps the pointer out of the
/// pixels and delivers it as PipeWire stream metadata instead, so consuming it would
/// mean writing a second sprite-compositing path over a portal frame, and nothing in
/// `platform::pipewire` reads pointer metadata today. If that is ever wanted, it is a
/// new feature with its own reasoning, not a quiet third arm of this function.
pub fn cursor_mode(cursor: CursorRequest) -> CursorMode {
    match cursor {
        CursorRequest::Keep => CursorMode::Embedded,
        CursorRequest::Omit => CursorMode::Hidden,
    }
}

/// Request a single-source ScreenCast session for `source`. When `restore_token`
/// (from a previous grant) is valid the portal reuses that source and skips the
/// dialog. `cursor` decides whether the pointer is baked into the stream (see
/// [`cursor_mode`]); it used to be `Embedded` unconditionally (DRAGON-592).
pub async fn request(
    source: SourceType,
    restore_token: Option<String>,
    cursor: CursorRequest,
) -> Result<CastSession, CastError> {
    let unavailable = |e: ashpd::Error| CastError::Unavailable(e.to_string());

    // Tell the portal who we are. Host (non-sandboxed) apps have no app_id by default,
    // so the portal can't resolve our .desktop and the screen-share dialog shows a
    // wrong/placeholder name ("KWin Dialog Helper"). Registering our app_id on ashpd's
    // (shared) D-Bus connection — the same one the cast request uses — makes the portal
    // map us to dev.thedragon.CosmicCaptureKit.desktop → "Cosmic Capture Kit". Best
    // effort: older portals without the Registry interface just keep the old behaviour.
    if let Ok(app_id) = ashpd::AppID::try_from("dev.thedragon.CosmicCaptureKit") {
        let _ = ashpd::register_host_app(app_id).await;
    }

    let sc = Screencast::new().await.map_err(unavailable)?;
    let session = sc.create_session().await.map_err(unavailable)?;
    // Configure the source type (monitor/window), single-select. The dialog itself
    // is raised by Start, below. ExplicitlyRevoked (mode 2) is load-bearing: see the
    // module doc for why mode 1 (Application) is dead on arrival for a one-shot
    // process (DRAGON-570).
    sc.select_sources(
        &session,
        cursor_mode(cursor),
        source.into(),
        false,
        restore_token.as_deref(),
        PersistMode::ExplicitlyRevoked,
    )
    .await
    .map_err(unavailable)?
    .response()
    .map_err(unavailable)?;

    // Start raises the picker and resolves to the granted streams. A
    // `Response(_)` error means the user cancelled/denied (abort); anything else is
    // an infrastructure failure (fall back).
    let streams = match sc.start(&session, None).await {
        Ok(req) => match req.response() {
            Ok(s) => s,
            Err(ashpd::Error::Response(_)) => return Err(CastError::Cancelled),
            Err(e) => return Err(CastError::Unavailable(e.to_string())),
        },
        Err(e) => return Err(CastError::Unavailable(e.to_string())),
    };

    let fd = sc.open_pipe_wire_remote(&session).await.map_err(unavailable)?;
    let restore_token = streams.restore_token().map(|s| s.to_string());
    let streams = streams
        .streams()
        .iter()
        .map(|s| StreamInfo {
            node_id: s.pipe_wire_node_id(),
            position: s.position(),
            size: s.size(),
        })
        .collect();
    Ok(CastSession {
        fd,
        streams,
        restore_token,
    })
}

/// DRAGON-592: the portal's half of the cursor toggle, pinned. Tiny on purpose, and
/// worth pinning anyway: the two arms were once a hard-coded `Embedded`, and every
/// portal user got a pointer they could not turn off. The pair is also EXHAUSTIVE
/// against `CursorRequest`, so adding a variant forces a decision here.
#[cfg(test)]
mod cursor_mode_tests {
    use super::{CursorRequest, cursor_mode};
    use ashpd::desktop::screencast::CursorMode;

    /// "Preserve mouse cursor" ON: the pointer is baked into the stream buffers,
    /// which is the historical behaviour and stays the default preference.
    #[test]
    fn keeping_the_cursor_embeds_it_in_the_stream() {
        assert_eq!(cursor_mode(CursorRequest::Keep), CursorMode::Embedded);
    }

    /// OFF, and the colour picker whatever the preference says: the pointer is not
    /// part of the stream at all. Never `Metadata`, which would leave it out of the
    /// pixels but hand us a sprite nothing here composites.
    #[test]
    fn omitting_the_cursor_hides_it_rather_than_sending_metadata() {
        assert_eq!(cursor_mode(CursorRequest::Omit), CursorMode::Hidden);
        assert_ne!(cursor_mode(CursorRequest::Omit), CursorMode::Metadata);
    }

    /// The two requests can never resolve to the same mode, which is the one way
    /// this could fail silently: a capture that ignores the toggle looks fine until
    /// someone checks a screenshot for a pointer.
    #[test]
    fn the_two_requests_never_collapse_into_one_mode() {
        assert_ne!(cursor_mode(CursorRequest::Keep), cursor_mode(CursorRequest::Omit));
    }
}
