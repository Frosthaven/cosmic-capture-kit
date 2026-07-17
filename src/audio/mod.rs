//! The microphone input cleanup chain, shared by the live mic test (and, later, the
//! recording path). One frame in, one cleaned frame's worth of metering out. Stages,
//! each gated by its own flag, chosen per the library research in `audio-levels.md`:
//!
//! ```text
//! far-end monitor ─▶ [AEC3] ─▶ mic ─▶ [WebRTC NS + high-pass] ─▶ [RNNoise] ─▶ [auto-gain] ─▶ [voice gate]
//! ```
//!
//! - AEC3 + noise suppression come from `sonora` (a pure-Rust WebRTC AudioProcessing port).
//! - RNNoise (`nnnoiseless`) cleans the non-stationary residual and yields a per-frame
//!   voice-activity probability for free.
//! - Auto-gain (`agc.rs`) is applied LAST, on the cleaned vocals, so it lifts what actually
//!   gets recorded. It's our own VAD-driven stage (not WebRTC AGC2) targeting this app's RMS
//!   meter: it lands the level in the ideal band and hard-caps it below the "too loud" line.
//! - The voice gate ("Input Sensitivity") opens on speech and closes on silence, driven
//!   by either the RNNoise probability or the `earshot` neural VAD ("Advanced Voice
//!   Activity"), combined with a level threshold that is either manual or auto-tracked
//!   from the noise floor. Gate timing/threshold values follow the research.

pub(crate) mod config;
pub(crate) mod filters;
mod meter;
mod input;
// The recording / mic-test capture pipeline, the per-channel level meters, and the
// PulseAudio device enumeration. Reached by path (`crate::audio::clean_mic::*`, etc.);
// the encoder depends on `clean_mic`/`config`, breaking the old capture<->encode cycle.
pub(crate) mod clean_mic;
pub(crate) mod devices;
pub(crate) mod ducking;
pub(crate) mod meters;
// Raw libpulse async-client FFI shared by `monitor_latency` and `capture` (DRAGON-123
// step 1: pure code motion out of `monitor_latency.rs`, no behavior change). Linux-only,
// so the whole file is gated right here rather than per-item inside it.
#[cfg(target_os = "linux")]
mod pulse_ffi;
// Live device-latency probe for the system-audio A/V-sync fix (DRAGON-119): samples
// the SIGNED monitor record-stream latency via libpulse's async client API (the
// value ffmpeg's pa_simple input clamps to 0). Reached as `crate::audio::MonitorLatencyProbe`.
mod monitor_latency;
// System-audio monitor capture client (DRAGON-123): a background thread runs the
// libpulse async client API to record the default sink's monitor (or a given source)
// as timestamped f32 stereo 48kHz chunks. Two consumers, both by full path
// (`crate::audio::capture::…`, no crate-level re-export): `record::pump`
// (DRAGON-125) reaches it directly, feeding chunks straight to the mixer; and the
// `--test capture-relay` diagnostic (DRAGON-126) starts a bare `MonitorCapture` to
// observe chunk cadence / delivery lag. (DRAGON-127 retired the third, legacy
// consumer — `system_relay`, which wired it into the legacy recording path's FIFO —
// along with that recording path.)
pub(crate) mod capture;

// Re-export the module's public surface so call sites use `crate::audio::InputProcessor`,
// `crate::audio::InputConfig`, `crate::audio::FRAME`, etc. The gate / Vad / metering
// helpers stay crate-internal and are used directly within `input` (`super::gate`, etc.).
pub use self::config::*;
pub use self::input::*;
pub(crate) use self::monitor_latency::MonitorLatencyProbe;
