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
/// Which VAAPI render node a captured frame can be encoded on (DRAGON-425). UNGATED
/// policy — pure decisions over plain data, so the macOS/Windows suites cover it even
/// though the `zero-copy` glue that calls it is Linux-DRM-only.
mod vaapi_node;

/// In-process VAAPI GPU encoder for the experimental zero-copy recording path.
#[cfg(feature = "zero-copy")]
pub mod gpu;

/// Importing a compositor dmabuf into CUDA so NVENC can encode it in place
/// (DRAGON-457) — the NVIDIA answer to the question `vaapi_node` answers for
/// AMD/Intel. Linux-only and `dlopen`-based, so a machine with no NVIDIA driver
/// simply reports it as unavailable.
#[cfg(all(target_os = "linux", feature = "zero-copy"))]
pub mod cuda;

pub use self::command::*;
pub use self::device::*;
pub use self::pixfmt::*;
pub use self::plan::*;
pub use self::preset::*;
pub use self::resolution::*;
// Re-exported ungated so `crate::encode::…` is the one path to it, but every consumer is
// behind `zero-copy`, so off-feature the re-export itself reads as unused.
#[cfg_attr(not(feature = "zero-copy"), allow(unused_imports))]
pub use self::vaapi_node::*;
