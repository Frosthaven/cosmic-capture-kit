//! Per-domain `Msg` handlers (DRAGON-115) — the bodies behind `application.rs`'s
//! thin `update` dispatch, one file per message domain, mirroring `app/message/`.
//! (`PreviewMsg` is the exception: its handler lives with the preview module it
//! drives, `app/preview`.)

mod capture;
mod detect;
mod recording;
mod settings;
mod permissions;
mod window_chrome;
