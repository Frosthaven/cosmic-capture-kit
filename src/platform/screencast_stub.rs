//! macOS/Windows type-stub for [`crate::platform::screencast`].
//!
//! The real module drives an xdg-desktop-portal ScreenCast session (ashpd) — a
//! Linux-only capture path. This stub provides ONLY the data types that leak into
//! platform-free app state (`App::pw_slot`/`pw_held`) and the
//! `on_pipewire_cast_ready` handler, so the app compiles unchanged. The actual
//! session `request()` + `SourceType` live behind Linux-gated methods
//! (`request_pipewire`/`portal_for_mode`), so nothing here is ever constructed on
//! macOS — `pipewire_available` is always false, so the portal path is never taken.
#![allow(dead_code)]

use std::os::fd::OwnedFd;

/// One stream the portal granted (PipeWire node id + monitor geometry).
pub struct StreamInfo {
    pub node_id: u32,
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

/// A live ScreenCast session (PipeWire remote fd + granted streams + restore token).
pub struct CastSession {
    pub fd: OwnedFd,
    pub streams: Vec<StreamInfo>,
    pub restore_token: Option<String>,
}

/// Why a cast request didn't yield a stream.
pub enum CastError {
    Cancelled,
    Unavailable(String),
}
