//! xdg-desktop-portal ScreenCast session: request a PipeWire stream for a chosen
//! source type (monitor or window), returning the remote fd + stream node(s) so we
//! can consume frames in-process ([`crate::platform::pipewire`]). Backs the "Prefer
//! PipeWire" recording path; we fall back to direct screencopy when it fails.

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

/// Request a single-source ScreenCast session for `source`. When `restore_token`
/// (from a previous grant) is valid the portal reuses that source and skips the
/// dialog. The cursor is embedded so it shows in the recording.
pub async fn request(
    source: SourceType,
    restore_token: Option<String>,
) -> Result<CastSession, CastError> {
    let unavailable = |e: ashpd::Error| CastError::Unavailable(e.to_string());

    // Tell the portal who we are. Host (non-sandboxed) apps have no app_id by default,
    // so the portal can't resolve our .desktop and the screen-share dialog shows a
    // wrong/placeholder name ("KWin Dialog Helper"). Registering our app_id on ashpd's
    // (shared) D-Bus connection — the same one the cast request uses — makes the portal
    // map us to dev.frosthaven.CosmicCaptureKit.desktop → "Cosmic Capture Kit". Best
    // effort: older portals without the Registry interface just keep the old behaviour.
    if let Ok(app_id) = ashpd::AppID::try_from("dev.frosthaven.CosmicCaptureKit") {
        let _ = ashpd::register_host_app(app_id).await;
    }

    let sc = Screencast::new().await.map_err(unavailable)?;
    let session = sc.create_session().await.map_err(unavailable)?;
    // Configure the source type (monitor/window), single-select. The dialog itself
    // is raised by Start, below.
    sc.select_sources(
        &session,
        CursorMode::Embedded,
        source.into(),
        false,
        restore_token.as_deref(),
        PersistMode::Application,
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
