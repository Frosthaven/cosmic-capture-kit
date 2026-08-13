//! Linux audio OUTPUT for the preview player (DRAGON preview-hitch): a PulseAudio playback
//! stream we own, so the presentation clock reads the TRUE audible position instead of
//! estimating the sink buffer. The Linux analogue of the macOS AudioQueue sink
//! (`platform/mac/services/audio_out.rs`); see `app/preview/playback.rs` for the whole
//! argument (real players never guess the audio delay, they ask the device).
//!
//! Uses the BLOCKING `pa_simple` API rather than the async threaded-mainloop client in
//! `pulse_ffi.rs`. For one-directional playback that is the right tool: `pa_simple_write`
//! blocks until the server accepts the data, which paces the pump to realtime by
//! construction (the same backpressure the mac sink gets from waiting on a free buffer), and
//! `pa_simple_get_latency` reports how much is buffered-but-not-yet-played, so the audible
//! position is `written − latency`. `pa_simple` lives in libpulse-simple, a separate link
//! from libpulse; both ship with PulseAudio, which every COSMIC box already has (libpulse is
//! a hard build dep, see `pulse_ffi.rs`).
//!
//! Single-thread use (the preview audio pump): filled by [`PulseSink::write`] and read by
//! [`PulseSink::played_secs`] from that one thread. The only cross-thread hop is the pump
//! copying `played_secs` into the shared `Clock.anchor`, exactly as on macOS.

use std::ffi::{c_char, c_int, c_void, CString};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

/// `pa_stream_direction_t::PA_STREAM_PLAYBACK` (<pulse/def.h>).
const PA_STREAM_PLAYBACK: c_int = 1;
/// `PA_SAMPLE_FLOAT32LE` (<pulse/sample.h>), matching the f32 PCM ffmpeg decodes for us.
const PA_SAMPLE_FLOAT32LE: c_int = 5;

/// `pa_sample_spec` — format/rate/channels. Redeclared here (rather than reused from
/// `pulse_ffi`) so this playback surface stays self-contained and does not couple to the
/// capture client's visibility.
#[repr(C)]
struct PaSampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

/// Opaque `pa_simple`.
enum PaSimple {}

#[link(name = "pulse-simple")]
unsafe extern "C" {
    fn pa_simple_new(
        server: *const c_char,
        name: *const c_char,
        dir: c_int,
        dev: *const c_char,
        stream_name: *const c_char,
        ss: *const PaSampleSpec,
        map: *const c_void,
        attr: *const c_void,
        error: *mut c_int,
    ) -> *mut PaSimple;
    fn pa_simple_write(s: *mut PaSimple, data: *const c_void, bytes: usize, error: *mut c_int)
        -> c_int;
    fn pa_simple_get_latency(s: *mut PaSimple, error: *mut c_int) -> u64;
    fn pa_simple_drain(s: *mut PaSimple, error: *mut c_int) -> c_int;
    fn pa_simple_free(s: *mut PaSimple);
}

/// An owned PulseAudio playback stream rendering interleaved f32 PCM, reporting the true
/// audible position. Single-thread use (the preview audio pump).
pub struct PulseSink {
    simple: *mut PaSimple,
    sample_rate: u32,
    channels: u8,
    frames_written: u64,
}

impl PulseSink {
    /// Open a default-sink playback stream for `sample_rate`/`channels` interleaved f32.
    /// `None` (caller plays silently) if the stream cannot be created, mirroring the macOS
    /// sink and the Windows missing-ffplay path.
    pub fn open(sample_rate: u32, channels: u8) -> Option<Self> {
        let ss = PaSampleSpec {
            format: PA_SAMPLE_FLOAT32LE,
            rate: sample_rate,
            channels,
        };
        let app = CString::new("cosmic-capture-kit").ok()?;
        let stream = CString::new("preview").ok()?;
        let mut err: c_int = 0;
        // SAFETY: `ss` is a fully-initialised spec; NULL server/dev/map/attr select the
        // defaults; the two CStrings outlive the call.
        let simple = unsafe {
            pa_simple_new(
                ptr::null(),
                app.as_ptr(),
                PA_STREAM_PLAYBACK,
                ptr::null(),
                stream.as_ptr(),
                &ss,
                ptr::null(),
                ptr::null(),
                &mut err,
            )
        };
        if simple.is_null() {
            log::warn!("preview audio: pa_simple_new failed (err {err}); playing silently");
            return None;
        }
        Some(Self {
            simple,
            sample_rate,
            channels,
            frames_written: 0,
        })
    }

    /// Play `interleaved` f32 samples. `pa_simple_write` blocks until the server accepts the
    /// data, which is the backpressure that paces the pump to realtime. `stop` short-circuits
    /// a teardown rather than starting another blocking write.
    pub fn write(&mut self, interleaved: &[f32], stop: &AtomicBool) {
        if interleaved.is_empty() || stop.load(Ordering::Relaxed) {
            return;
        }
        let bytes = std::mem::size_of_val(interleaved);
        let mut err: c_int = 0;
        // SAFETY: `interleaved` is valid for `bytes`; `simple` is live for the sink's life.
        let rc = unsafe {
            pa_simple_write(
                self.simple,
                interleaved.as_ptr() as *const c_void,
                bytes,
                &mut err,
            )
        };
        if rc < 0 {
            log::warn!("preview audio: pa_simple_write failed (err {err})");
            return;
        }
        self.frames_written += (interleaved.len() / self.channels.max(1) as usize) as u64;
    }

    /// Seconds of audio the device has actually PLAYED (written minus what is still buffered),
    /// i.e. the true audible position. `None` before anything has been written.
    pub fn played_secs(&self) -> Option<f64> {
        if self.frames_written == 0 {
            return None;
        }
        let written = self.frames_written as f64 / self.sample_rate as f64;
        let mut err: c_int = 0;
        // SAFETY: `simple` is live for the sink's lifetime.
        let latency_us = unsafe { pa_simple_get_latency(self.simple, &mut err) };
        // pa_simple_get_latency returns (pa_usec_t)-1 on error; treat that as "no buffer
        // estimate", i.e. everything written is audible.
        if latency_us == u64::MAX {
            return Some(written);
        }
        Some((written - latency_us as f64 / 1_000_000.0).max(0.0))
    }

    /// True once the buffered latency has fallen to near zero, so the pump can stop draining.
    pub fn drained(&self) -> bool {
        let mut err: c_int = 0;
        // SAFETY: `simple` is live for the sink's lifetime.
        let latency_us = unsafe { pa_simple_get_latency(self.simple, &mut err) };
        latency_us == u64::MAX || latency_us < 20_000 // < 20 ms still buffered
    }
}

impl Drop for PulseSink {
    fn drop(&mut self) {
        let mut err: c_int = 0;
        // SAFETY: `simple` was created by `pa_simple_new` and not yet freed. Drain first so a
        // natural end plays its tail; both calls are safe on a live handle.
        unsafe {
            pa_simple_drain(self.simple, &mut err);
            pa_simple_free(self.simple);
        }
    }
}
