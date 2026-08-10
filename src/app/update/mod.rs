//! Per-domain `Msg` handlers (DRAGON-115) — the bodies behind `application.rs`'s
//! thin `update` dispatch, one file per message domain, mirroring `app/message/`.
//! (`PreviewMsg` is the exception: its handler lives with the preview module it
//! drives, `app/preview`.)

mod capture;
mod color_picker;
mod detect;
mod recording;
mod settings;
mod permissions;
// Visible inside `crate::app` (not just `update`) because `subscriptions` reads its pure
// decision fns — `overlay_finalize_active` decides whether `sub_overlay_finalize` runs, and
// that rule belongs next to the tick handler that consumes it, not duplicated at the
// subscription. Its items stay `pub(in crate::app)`, so nothing leaks past the app.
pub(in crate::app) mod window_chrome;
