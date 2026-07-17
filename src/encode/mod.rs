//! Video encoding for recordings: encoder selection (hardware GPU encoders with a
//! software fallback), the capture ffmpeg command, and the RGBA→NV12 conversion.
//!
//! Two paths, chosen by the "hardware encoding" setting and what the machine has:
//! - **Hardware** (NVENC for nvidia, VAAPI for AMD/Intel): we convert frames to
//!   NV12 ourselves (threaded, BT.709 limited) and feed that, so the pipe carries
//!   half the bytes and ffmpeg does no colour conversion — the difference between
//!   ~60 and 120+ fps at 5K.
//! - **Software fallback** (libx264): we feed RGBA and let ffmpeg convert — the
//!   simplest, most compatible path, used whenever the hardware path can't be.

mod command;
mod device;
mod pixfmt;
mod plan;
mod preset;
mod resolution;

/// In-process VAAPI GPU encoder for the experimental zero-copy recording path.
#[cfg(feature = "zero-copy")]
pub mod gpu;

pub use self::command::*;
pub use self::device::*;
pub use self::pixfmt::*;
pub use self::plan::*;
pub use self::preset::*;
pub use self::resolution::*;
