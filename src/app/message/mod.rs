//! Per-domain message sub-enums; `Msg` (app/mod.rs) is a thin wrapper.

mod capture;
mod recording;
mod detect;
mod settings;
mod permissions;
mod window_chrome;
mod preview;

pub use capture::CaptureMsg;
pub use recording::RecordingMsg;
pub use detect::DetectMsg;
pub use settings::{BorderColorTarget, SettingsMsg};
pub use permissions::PermissionsMsg;
pub use window_chrome::WindowChromeMsg;
pub use preview::{PreviewMsg, VideoMeta};
