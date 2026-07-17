//! Live probe of the audio DEVICE's output latency, for the system-audio A/V-sync
//! fix (DRAGON-119 follow-up).
//!
//! ## Why this exists (the mechanism)
//!
//! When you record a lip-synced source (a browser, a video player) with system
//! audio, the recording comes out with the audio LEADING the video by a uniform
//! amount ≈ the audio device's output latency. The reason:
//!
//! - Our system-audio capture taps the sink's MIX POINT (`<sink>.monitor`) — the
//!   samples the moment they are mixed, BEFORE the device's output buffer delays
//!   them on their way to being audible.
//! - A lip-syncing source delays its VIDEO to match the AUDIBLE sound, i.e. it
//!   shifts the picture later by that same device latency.
//!
//! So the captured audio sits ~`device_latency` earlier than the picture it is
//! meant to match. The faithful fix is to stamp/shift the captured system audio to
//! its AUDIBLE time — which is exactly `device_latency` later. The media-clock
//! owned recording pipeline folds that shift into `record::pump`'s own per-session
//! latch (system channel only; DRAGON-119/125) rather than through this probe —
//! see this module's doc note on [`MonitorLatencyProbe`] for what still uses it.
//!
//! ## Why ffmpeg can't read it itself
//!
//! ffmpeg's `-f pulse` input uses the `pa_simple` API, whose latency query returns
//! an UNSIGNED `pa_usec_t`. A monitor record stream's latency is NEGATIVE (its
//! magnitude ≈ the device output latency — the captured data corresponds to sound
//! not yet audible), and the reference `pa_simple` implementation clamps that
//! negative value to 0. So ffmpeg simply never sees it.
//!
//! The SIGNED value is available through the async libpulse client API:
//! `pa_stream_get_latency` fills a magnitude plus a separate `negative` flag. That
//! API works identically on PulseAudio and on PipeWire-Pulse. This module drives it
//! with a threaded mainloop on one background thread (the same approach OBS uses),
//! via a small set of raw `#[link(name = "pulse")]` FFI declarations — no crate
//! dependency, no shelling out to `pactl`.
//!
//! ## What the value means at the edges
//!
//! - A virtual / null sink has NO hardware output buffer, so its `sink_usec` is 0
//!   and the monitor latency is ~0 — correctly, there is no device delay to undo.
//!   (This is why the harness null-sink test measures ~0; the real magnitude only
//!   appears on real hardware.)
//! - A SUSPENDED sink reports no negative latency, so we sample 0.0 — fail-open, we
//!   never over-correct.
//! - The value is LIVE, per-recording data. It is NEVER persisted: a probe runs for
//!   the duration of each recording and its median feeds that one finalize pass.
//!
//! ## Teardown discipline (DRAGON-118)
//!
//! A recording's stop must never hang on a wedged sound server. Rather than the
//! idiomatic (but unbounded) `pa_threaded_mainloop_wait`/`signal` handshake, this
//! probe POLLS state under the mainloop lock with a deadline on every wait, and
//! [`MonitorLatencyProbe::stop`] bounded-joins the thread (≤2s) and DETACHES it if
//! it will not exit — returning whatever was sampled. The unsafe FFI surface is the
//! minimum set of calls that supports that design; DRAGON-123 split it into its own
//! shared module ([`crate::audio::pulse_ffi`]) so the system-audio monitor capture
//! client can reuse it too, without duplicating the `extern` block or the polling
//! discipline.
//!
//! The whole real implementation is `#[cfg(target_os = "linux")]`; a non-Linux stub
//! returns `None` from `start`, so the DRAGON-92 cross-platform ports can slot a
//! CoreAudio / WASAPI equivalent behind the same `fn`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Median of the collected device-latency samples (ms). Empty → 0.0 (no samples, or
/// the probe never started / never connected); all-zero → 0.0. Even counts average
/// the two middle values. Non-finite samples are dropped defensively. Zeros are real
/// fail-open samples (suspended / virtual sink) and participate normally.
///
/// `pub(crate)`: [`crate::audio::capture::MonitorCapture::stop`] reuses this exact
/// function for its own device-latency samples (DRAGON-123) instead of duplicating
/// the logic.
pub(crate) fn median(mut v: Vec<f64>) -> f64 {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// A running probe of the default sink's monitor latency. [`start`](Self::start)
/// spawns one background thread that samples the SIGNED record-stream latency every
/// couple of seconds; [`stop`](Self::stop) tears it down (bounded) and returns the
/// MEDIAN magnitude in ms (0.0 when nothing usable was sampled).
///
/// DRAGON-127: this probe's recording-path wiring (feeding the SYSTEM channel's
/// finalize delay in the legacy wallclock+CFR+segments recorder) was retired along
/// with that recording path — the media-clock owned pipeline needs no such probe
/// (the system-audio tap's own timestamps already land it correctly). The struct
/// itself stays: it's still driven live by the `--test monitor-latency` diagnostic
/// (`crate::cli::diagnostics::monitor_latency_test`) for on-machine device-latency
/// inspection.
pub(crate) struct MonitorLatencyProbe {
    stop: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<f64>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MonitorLatencyProbe {
    /// Spawn the probe. Returns `None` only when the platform is unsupported (or the
    /// thread can't be spawned); a Linux probe that then fails to connect / find a
    /// sink simply collects no samples, so [`stop`](Self::stop) yields 0.0.
    #[cfg(target_os = "linux")]
    pub(crate) fn start() -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let thread_stop = stop.clone();
        let thread_samples = samples.clone();
        let thread = std::thread::Builder::new()
            .name("cck-monitor-latency".to_string())
            .spawn(move || pulse::run_probe(&thread_stop, &thread_samples))
            .ok()?;
        Some(Self { stop, samples, thread: Some(thread) })
    }

    /// Non-Linux stub: no probe yet. DRAGON-92 slots a CoreAudio / WASAPI probe here.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn start() -> Option<Self> {
        None
    }

    /// Signal the probe thread to tear down, bounded-join it (≤2s), and return the
    /// MEDIAN sampled latency (ms). A wedged sound server must never hang the
    /// recording stop (DRAGON-118): if the thread won't exit in time it is DETACHED
    /// and whatever it already sampled is returned.
    pub(crate) fn stop(mut self) -> f64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                log::warn!("monitor-latency probe did not exit within 2s; detaching it (DRAGON-118)");
            }
        }
        let samples = self.samples.lock().map(|g| g.clone()).unwrap_or_default();
        median(samples)
    }
}

// ---------------------------------------------------------------------------
// Linux: the probe-specific logic. The shared libpulse FFI (the `extern` block,
// opaque types, `PaGuard`, the bounded-wait helpers) lives in `crate::audio::pulse_ffi`
// (DRAGON-123 step 1 — pure code motion, no behavior change); this module keeps only
// what's specific to sampling the default sink's monitor latency: the
// drain-everything read callback and the probe thread body.
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod pulse {
    use super::{AtomicBool, Duration, Instant, Mutex, Ordering};
    use super::super::pulse_ffi::*;
    use std::ffi::{c_int, c_void, CString};
    use std::ptr;

    /// Read callback: drain + discard. We keep a record stream running ONLY so its
    /// timing info stays fresh for `pa_stream_get_latency`; the samples are unused,
    /// so peek/drop the whole readable buffer each notification (a peeked hole still
    /// needs a matching drop to advance the read index).
    unsafe extern "C" fn drain_cb(s: *mut PaStream, _nbytes: usize, _userdata: *mut c_void) {
        loop {
            let mut data: *const c_void = ptr::null();
            let mut nbytes: usize = 0;
            // SAFETY: `s` is the live stream pulse handed us, called under the lock.
            let rc = unsafe { pa_stream_peek(s, &mut data, &mut nbytes) };
            if rc < 0 || nbytes == 0 {
                break;
            }
            // SAFETY: a successful peek is matched by exactly one drop.
            unsafe { pa_stream_drop(s) };
        }
    }

    /// The probe thread body: connect a threaded-mainloop pulse client, resolve the
    /// default sink's monitor once, then sample its SIGNED record latency every ~2s
    /// until `stop`, pushing each magnitude (ms) into `samples`. Every wait is
    /// deadline-bounded; a connection/stream failure exits early with whatever was
    /// collected.
    pub(super) fn run_probe(stop: &AtomicBool, samples: &Mutex<Vec<f64>>) {
        // Generous on connect (runs once at recording START, off the stop path);
        // short per-sample.
        const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
        const OP_TIMEOUT: Duration = Duration::from_secs(1);
        const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

        // Declared before `guard` so it drops AFTER it (after the mainloop is stopped
        // and no callback can still write it).
        let sink_slot: Mutex<Option<CString>> = Mutex::new(None);

        // SAFETY: every pulse call below runs on this one thread; all access to the
        // context/stream after `start` is under the mainloop lock, and `guard` tears
        // the client down on any return path.
        unsafe {
            let m = pa_threaded_mainloop_new();
            if m.is_null() {
                return;
            }
            let mut guard = PaGuard { m, c: ptr::null_mut(), s: ptr::null_mut(), started: false };

            let api = pa_threaded_mainloop_get_api(m);
            if api.is_null() {
                return;
            }
            let ctx_name = CString::new("cosmic-capture-kit-latency").unwrap();
            let c = pa_context_new(api, ctx_name.as_ptr());
            if c.is_null() {
                return;
            }
            guard.c = c;

            // Connect before the loop runs (nothing else touches the context yet),
            // then start the IO thread and poll to READY under the lock.
            if pa_context_connect(c, ptr::null(), 0, ptr::null()) < 0 {
                return;
            }
            if pa_threaded_mainloop_start(m) != 0 {
                return;
            }
            guard.started = true;
            if !wait_context_ready(m, c, stop, CONNECT_TIMEOUT) {
                return;
            }

            // Resolve the default sink ONCE and take its monitor source.
            pa_threaded_mainloop_lock(m);
            let op = pa_context_get_server_info(
                c,
                server_info_cb,
                &sink_slot as *const Mutex<Option<CString>> as *mut c_void,
            );
            pa_threaded_mainloop_unlock(m);
            if !await_op(m, op, Instant::now() + OP_TIMEOUT, stop) {
                return;
            }
            let Some(sink) = sink_slot.lock().ok().and_then(|g| g.clone()) else {
                return;
            };
            let Some(monitor) = make_monitor_name(&sink) else {
                return;
            };

            // A cheap S16 mono 8 kHz record stream on the monitor.
            let ss = PaSampleSpec { format: PA_SAMPLE_S16LE, rate: 8000, channels: 1 };
            let stream_name = CString::new("cosmic-capture-kit-latency").unwrap();
            pa_threaded_mainloop_lock(m);
            let s = pa_stream_new(c, stream_name.as_ptr(), &ss, ptr::null());
            if s.is_null() {
                pa_threaded_mainloop_unlock(m);
                return;
            }
            guard.s = s;
            pa_stream_set_read_callback(s, Some(drain_cb), ptr::null_mut());
            let flags = PA_STREAM_INTERPOLATE_TIMING | PA_STREAM_AUTO_TIMING_UPDATE;
            let rc = pa_stream_connect_record(s, monitor.as_ptr(), ptr::null(), flags);
            pa_threaded_mainloop_unlock(m);
            if rc < 0 {
                return;
            }
            if !wait_stream_ready(m, s, stop, CONNECT_TIMEOUT) {
                return;
            }

            // A short settle so the first timing update reflects real device data
            // (and the ≤1s smoke test still gets a reading), then sample every ~2s.
            sleep_or_stop(Duration::from_millis(300), stop);
            while !stop.load(Ordering::Relaxed) {
                // Flush our client-side record backlog FIRST (the read callback drains
                // continuously, but data keeps arriving) so the measured latency
                // reflects the pure source/sink offset — NEGATIVE when the sink has a
                // real device output buffer — not our own accumulated read buffer,
                // which would read positive and hide the sign. Mirrors the validated
                // C probe; correctness-critical for real (native-PulseAudio) hardware.
                pa_threaded_mainloop_lock(m);
                let flush_op = pa_stream_flush(s, None, ptr::null_mut());
                pa_threaded_mainloop_unlock(m);
                let _ = await_op(m, flush_op, Instant::now() + OP_TIMEOUT, stop);

                pa_threaded_mainloop_lock(m);
                let op = pa_stream_update_timing_info(s, None, ptr::null_mut());
                pa_threaded_mainloop_unlock(m);
                if await_op(m, op, Instant::now() + OP_TIMEOUT, stop) {
                    let mut usec: u64 = 0;
                    let mut negative: c_int = 0;
                    pa_threaded_mainloop_lock(m);
                    let got = pa_stream_get_latency(s, &mut usec, &mut negative);
                    pa_threaded_mainloop_unlock(m);
                    if got >= 0 {
                        // Monitor tap = the sink's MIX point; the device output buffer
                        // makes the record latency NEGATIVE, its magnitude ≈ the
                        // device latency (the audible-time shift). A non-negative
                        // reading (virtual / suspended sink, no device buffer) → 0.
                        let sample = if negative != 0 { usec as f64 / 1000.0 } else { 0.0 };
                        if let Ok(mut g) = samples.lock() {
                            g.push(sample);
                        }
                    }
                }
                // Bail if the stream or context dropped out.
                pa_threaded_mainloop_lock(m);
                let sst = pa_stream_get_state(s);
                let cst = pa_context_get_state(c);
                pa_threaded_mainloop_unlock(m);
                if sst == PA_STREAM_FAILED
                    || sst == PA_STREAM_TERMINATED
                    || cst == PA_CONTEXT_FAILED
                    || cst == PA_CONTEXT_TERMINATED
                {
                    break;
                }
                sleep_or_stop(SAMPLE_INTERVAL, stop);
            }
            // `guard` drops here → bounded teardown; `sink_slot` drops after it.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_empty_is_zero() {
        assert_eq!(median(vec![]), 0.0);
    }

    #[test]
    fn median_all_zero_is_zero() {
        assert_eq!(median(vec![0.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn median_odd_picks_the_middle() {
        assert_eq!(median(vec![30.0, 10.0, 20.0]), 20.0);
    }

    #[test]
    fn median_even_averages_the_two_middle() {
        assert_eq!(median(vec![10.0, 20.0, 30.0, 40.0]), 25.0);
    }

    #[test]
    fn median_mixed_counts_fail_open_zeros() {
        // Suspended/virtual-sink zeros are real samples: [0,0,300,300] → (0+300)/2.
        assert_eq!(median(vec![300.0, 0.0, 300.0, 0.0]), 150.0);
    }

    /// Smoke test: on a box with a reachable pulse server, start + ~1s + stop must
    /// return a finite, non-negative latency without hanging. LOUDLY skips (never a
    /// silent green) when no server looks reachable — mirroring the ffmpeg gate in
    /// `record::av_sync_tests`. The value itself may be 0.0 (e.g. a suspended sink).
    #[cfg(target_os = "linux")]
    #[test]
    fn monitor_latency_probe_smoke() {
        let reachable = std::env::var_os("PULSE_SERVER").is_some()
            || std::env::var_os("XDG_RUNTIME_DIR")
                .map(|d| std::path::Path::new(&d).join("pulse/native").exists())
                .unwrap_or(false);
        if !reachable {
            eprintln!(
                "SKIPPED (loud): monitor_latency_probe_smoke needs a reachable PulseAudio/\
                 PipeWire-Pulse server — the probe was not exercised"
            );
            return;
        }
        let started = Instant::now();
        let Some(probe) = MonitorLatencyProbe::start() else {
            eprintln!("SKIPPED (loud): MonitorLatencyProbe unsupported on this platform");
            return;
        };
        std::thread::sleep(Duration::from_millis(1000));
        let v = probe.stop();
        eprintln!("monitor_latency_probe_smoke: default-sink monitor latency = {v:.1} ms");
        assert!(
            v.is_finite() && v >= 0.0,
            "probe must return a finite, non-negative latency (got {v})"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "probe start+stop must be bounded (took {:?})",
            started.elapsed()
        );
    }
}
