//! Per-domain message sub-enums; `Msg` (app/mod.rs) is a thin wrapper.

mod capture;
mod color_picker;
mod recording;
mod detect;
mod settings;
mod permissions;
mod window_chrome;
mod preview;

pub use capture::CaptureMsg;
pub use color_picker::ColorPickerMsg;
pub use recording::RecordingMsg;
pub use detect::DetectMsg;
pub use settings::{BorderColorTarget, CloudSettingsMsg, SettingsMsg};
// DRAGON-589: portable. Only two platforms can BIND these keys, but every platform's Global
// tab lists the same seven actions, so the labels, the order and the flags are shared.
pub use settings::CaptureHotkeySlot;
pub use permissions::PermissionsMsg;
pub use window_chrome::WindowChromeMsg;
pub use preview::{PreviewMsg, VideoMeta};
