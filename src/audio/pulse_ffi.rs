//! Raw libpulse async-client FFI shared by [`crate::audio::monitor_latency`] (the
//! device-latency probe) and [`crate::audio::capture`] (the monitor capture client,
//! DRAGON-123). Both build a threaded-mainloop pulse client with the same connect
//! flow, bounded-wait polling (NEVER `pa_threaded_mainloop_wait` — unbounded), and
//! RAII teardown (DRAGON-118: a wedged sound server must never hang a caller's stop);
//! this module holds the one shared `unsafe` surface plus the small helpers that
//! implement that discipline, so neither caller re-declares the `extern` block or
//! re-derives the polling/teardown logic.
//!
//! Callers keep their own stream-specific logic (sample spec, read callback, what a
//! sample means, what to do with the data) — this module owns only what is
//! bit-for-bit identical between them. DRAGON-123 step 1 moved this out of
//! `monitor_latency.rs` as pure code motion (no behavior change; see that module's
//! `run_probe`, which now imports from here instead of declaring its own copy).
//!
//! `#[cfg(target_os = "linux")]` at this module's declaration in `audio/mod.rs` gates
//! the whole file — every item here is Linux-only, so nothing inside needs its own
//! per-item `cfg`.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// pa enum values branched on / requested (stable ABI constants from <pulse/def.h>,
// <pulse/sample.h>). Only the ones this module's clients actually need.
pub(super) const PA_SAMPLE_S16LE: c_int = 3; // the latency probe's cheap mono spec
// The monitor capture client's spec (`crate::audio::capture`, wired into recordings
// by `record::pump`'s media-clock pipeline, DRAGON-123/125).
pub(super) const PA_SAMPLE_FLOAT32LE: c_int = 5;
pub(super) const PA_CONTEXT_READY: c_int = 4;
pub(super) const PA_CONTEXT_FAILED: c_int = 5;
pub(super) const PA_CONTEXT_TERMINATED: c_int = 6;
pub(super) const PA_STREAM_READY: c_int = 2;
pub(super) const PA_STREAM_FAILED: c_int = 3;
pub(super) const PA_STREAM_TERMINATED: c_int = 4;
pub(super) const PA_OPERATION_RUNNING: c_int = 0;
pub(super) const PA_OPERATION_DONE: c_int = 1;
// Interpolate the latency and receive periodic timing updates, so
// `pa_stream_get_latency` has fresh data without a full server round trip.
pub(super) const PA_STREAM_INTERPOLATE_TIMING: c_int = 0x0002;
pub(super) const PA_STREAM_AUTO_TIMING_UPDATE: c_int = 0x0008;
// Ask the server to size the SOURCE-side buffering to our `buffer_attr` (rather than
// only the client-visible latency): the monitor capture client (DRAGON-126) sets this
// together with a small `fragsize` so chunks arrive ~one fragment stale instead of at
// the server's default ~2s record latency. Value from <pulse/def.h>.
pub(super) const PA_STREAM_ADJUST_LATENCY: c_int = 0x2000;

// Opaque pulse handles — only ever held behind pointers.
pub(super) enum PaThreadedMainloop {}
pub(super) enum PaMainloopApi {}
pub(super) enum PaContext {}
pub(super) enum PaStream {}
pub(super) enum PaOperation {}

/// `pa_sample_spec` — sample format/rate/channel-count layout. Shared: the latency
/// probe requests a tiny S16 mono 8 kHz spec (cheap to keep running), the monitor
/// capture client requests f32 stereo 48 kHz (see
/// [`crate::audio::capture`]) — pulse converts server-side either way.
#[repr(C)]
pub(super) struct PaSampleSpec {
    pub(super) format: c_int,
    pub(super) rate: u32,
    pub(super) channels: u8,
}

/// `pa_buffer_attr` — the per-stream buffering request (field order per
/// <pulse/def.h>: maxlength, tlength, prebuf, minreq, fragsize). Only `fragsize`
/// matters for a record stream (the playback fields stay `u32::MAX` = "server
/// default"); with [`PA_STREAM_ADJUST_LATENCY`] set, a small `fragsize` is what makes
/// the server hand back small, timely chunks instead of ~2s-stale ones (DRAGON-126).
#[repr(C)]
pub(super) struct PaBufferAttr {
    pub(super) maxlength: u32,
    pub(super) tlength: u32,
    pub(super) prebuf: u32,
    pub(super) minreq: u32,
    pub(super) fragsize: u32,
}

/// `pa_channel_map` (PA_CHANNELS_MAX = 32). Declared only so [`PaServerInfo`]'s
/// layout is exact; we never read it.
#[repr(C)]
pub(super) struct PaChannelMap {
    channels: u8,
    map: [c_int; 32],
}

/// `pa_server_info`. We read only `default_sink_name`; the rest is present so the
/// field offset is correct.
#[repr(C)]
pub(super) struct PaServerInfo {
    user_name: *const c_char,
    host_name: *const c_char,
    server_version: *const c_char,
    server_name: *const c_char,
    sample_spec: PaSampleSpec,
    default_sink_name: *const c_char,
    default_source_name: *const c_char,
    cookie: u32,
    channel_map: PaChannelMap,
}

pub(super) type ServerInfoCb = unsafe extern "C" fn(*mut PaContext, *const PaServerInfo, *mut c_void);
pub(super) type StreamRequestCb = unsafe extern "C" fn(*mut PaStream, usize, *mut c_void);
pub(super) type StreamSuccessCb = unsafe extern "C" fn(*mut PaStream, c_int, *mut c_void);

#[link(name = "pulse")]
unsafe extern "C" {
    pub(super) fn pa_threaded_mainloop_new() -> *mut PaThreadedMainloop;
    pub(super) fn pa_threaded_mainloop_free(m: *mut PaThreadedMainloop);
    pub(super) fn pa_threaded_mainloop_start(m: *mut PaThreadedMainloop) -> c_int;
    pub(super) fn pa_threaded_mainloop_stop(m: *mut PaThreadedMainloop);
    pub(super) fn pa_threaded_mainloop_lock(m: *mut PaThreadedMainloop);
    pub(super) fn pa_threaded_mainloop_unlock(m: *mut PaThreadedMainloop);
    pub(super) fn pa_threaded_mainloop_get_api(m: *mut PaThreadedMainloop) -> *mut PaMainloopApi;

    pub(super) fn pa_context_new(api: *mut PaMainloopApi, name: *const c_char) -> *mut PaContext;
    pub(super) fn pa_context_unref(c: *mut PaContext);
    pub(super) fn pa_context_connect(
        c: *mut PaContext,
        server: *const c_char,
        flags: c_int,
        api: *const c_void,
    ) -> c_int;
    pub(super) fn pa_context_disconnect(c: *mut PaContext);
    pub(super) fn pa_context_get_state(c: *const PaContext) -> c_int;
    pub(super) fn pa_context_get_server_info(
        c: *mut PaContext,
        cb: ServerInfoCb,
        userdata: *mut c_void,
    ) -> *mut PaOperation;

    pub(super) fn pa_stream_new(
        c: *mut PaContext,
        name: *const c_char,
        ss: *const PaSampleSpec,
        map: *const PaChannelMap,
    ) -> *mut PaStream;
    pub(super) fn pa_stream_unref(s: *mut PaStream);
    pub(super) fn pa_stream_get_state(s: *const PaStream) -> c_int;
    pub(super) fn pa_stream_connect_record(
        s: *mut PaStream,
        dev: *const c_char,
        attr: *const PaBufferAttr,
        flags: c_int,
    ) -> c_int;
    pub(super) fn pa_stream_disconnect(s: *mut PaStream) -> c_int;
    pub(super) fn pa_stream_set_read_callback(
        s: *mut PaStream,
        cb: Option<StreamRequestCb>,
        userdata: *mut c_void,
    );
    pub(super) fn pa_stream_peek(s: *mut PaStream, data: *mut *const c_void, nbytes: *mut usize) -> c_int;
    pub(super) fn pa_stream_drop(s: *mut PaStream) -> c_int;
    pub(super) fn pa_stream_flush(
        s: *mut PaStream,
        cb: Option<StreamSuccessCb>,
        userdata: *mut c_void,
    ) -> *mut PaOperation;
    pub(super) fn pa_stream_update_timing_info(
        s: *mut PaStream,
        cb: Option<StreamSuccessCb>,
        userdata: *mut c_void,
    ) -> *mut PaOperation;
    pub(super) fn pa_stream_get_latency(s: *mut PaStream, r_usec: *mut u64, negative: *mut c_int) -> c_int;

    pub(super) fn pa_operation_get_state(o: *const PaOperation) -> c_int;
    pub(super) fn pa_operation_unref(o: *mut PaOperation);
}

/// Server-info callback: copy the default sink name out into the shared slot. The C
/// string is only valid for this call, so we own a `CString` copy.
pub(super) unsafe extern "C" fn server_info_cb(
    _c: *mut PaContext,
    info: *const PaServerInfo,
    userdata: *mut c_void,
) {
    if info.is_null() || userdata.is_null() {
        return;
    }
    // SAFETY: `userdata` is the `&Mutex<Option<CString>>` passed to
    // get_server_info; it outlives the mainloop (dropped only after the mainloop is
    // stopped, so no callback can still reference it).
    let slot = unsafe { &*(userdata as *const Mutex<Option<CString>>) };
    // SAFETY: `info` is valid for this call; `default_sink_name` is NUL-terminated.
    let name = unsafe { (*info).default_sink_name };
    if name.is_null() {
        return;
    }
    let owned = unsafe { CStr::from_ptr(name) }.to_owned();
    if let Ok(mut g) = slot.lock() {
        *g = Some(owned);
    }
}

/// Bounded sleep that returns early once `stop` is set.
pub(super) fn sleep_or_stop(dur: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Poll the context state to READY, bounded by `stop`/`timeout`.
pub(super) fn wait_context_ready(
    m: *mut PaThreadedMainloop,
    c: *const PaContext,
    stop: &AtomicBool,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: state read under the mainloop lock; `c` is live.
        unsafe { pa_threaded_mainloop_lock(m) };
        let st = unsafe { pa_context_get_state(c) };
        unsafe { pa_threaded_mainloop_unlock(m) };
        if st == PA_CONTEXT_READY {
            return true;
        }
        if st == PA_CONTEXT_FAILED || st == PA_CONTEXT_TERMINATED {
            return false;
        }
        if stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Poll the stream state to READY, bounded by `stop`/`timeout`.
pub(super) fn wait_stream_ready(
    m: *mut PaThreadedMainloop,
    s: *const PaStream,
    stop: &AtomicBool,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: state read under the mainloop lock; `s` is live.
        unsafe { pa_threaded_mainloop_lock(m) };
        let st = unsafe { pa_stream_get_state(s) };
        unsafe { pa_threaded_mainloop_unlock(m) };
        if st == PA_STREAM_READY {
            return true;
        }
        if st == PA_STREAM_FAILED || st == PA_STREAM_TERMINATED {
            return false;
        }
        if stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Drive a pulse operation to completion, bounded by `deadline`/`stop`, then
/// unref it. Returns true iff it reached DONE.
pub(super) fn await_op(
    m: *mut PaThreadedMainloop,
    op: *mut PaOperation,
    deadline: Instant,
    stop: &AtomicBool,
) -> bool {
    if op.is_null() {
        return false;
    }
    let mut done = false;
    loop {
        // SAFETY: operation state read under the mainloop lock.
        unsafe { pa_threaded_mainloop_lock(m) };
        let st = unsafe { pa_operation_get_state(op) };
        unsafe { pa_threaded_mainloop_unlock(m) };
        if st != PA_OPERATION_RUNNING {
            done = st == PA_OPERATION_DONE;
            break;
        }
        if stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // SAFETY: release our reference to the operation.
    unsafe { pa_threaded_mainloop_lock(m) };
    unsafe { pa_operation_unref(op) };
    unsafe { pa_threaded_mainloop_unlock(m) };
    done
}

/// `<sink>.monitor` for the resolved default sink — matching what ffmpeg's
/// `@DEFAULT_MONITOR@` resolves at its own open.
pub(super) fn make_monitor_name(sink: &CStr) -> Option<CString> {
    let sink = sink.to_str().ok()?;
    CString::new(format!("{sink}.monitor")).ok()
}

/// RAII teardown for a pulse client (any early return runs it). Stops the mainloop
/// FIRST — halting the IO thread so no callback is mid-flight — then disconnects +
/// unrefs the stream and context, then frees the mainloop. Each caller's own bounded
/// stop/join ([`super::MonitorLatencyProbe::stop`], [`super::capture::MonitorCapture::stop`])
/// is the backstop if the server has wedged (DRAGON-118); this guard is what makes
/// that safe to detach.
pub(super) struct PaGuard {
    pub(super) m: *mut PaThreadedMainloop,
    pub(super) c: *mut PaContext,
    pub(super) s: *mut PaStream,
    pub(super) started: bool,
}

impl Drop for PaGuard {
    fn drop(&mut self) {
        // SAFETY: pointers are either null or live handles this thread owns;
        // stop() is gated on `started` (stopping a never-started mainloop aborts).
        unsafe {
            if self.started && !self.m.is_null() {
                pa_threaded_mainloop_stop(self.m);
            }
            if !self.s.is_null() {
                pa_stream_disconnect(self.s);
                pa_stream_unref(self.s);
            }
            if !self.c.is_null() {
                pa_context_disconnect(self.c);
                pa_context_unref(self.c);
            }
            if !self.m.is_null() {
                pa_threaded_mainloop_free(self.m);
            }
        }
    }
}
