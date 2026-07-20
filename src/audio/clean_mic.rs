//! The cleaned-microphone capture pipeline: the recording-path tap feeder
//! ([`setup_clean_mic_tap`] + [`MicTapHandle`], DRAGON-125 — the media-clock owned
//! pipeline's mic source) and the live settings-page mic test ([`spawn_mic_test`]),
//! both built on the shared PulseAudio PCM capture helper ([`spawn_pulse_pcm`]) and
//! the [`crate::audio::InputProcessor`] cleanup chain. (DRAGON-127 retired the
//! recording-path FIFO feeder — `setup_clean_mic_input` + `CleanMicHandle` — this
//! module used to ALSO export for the legacy recording path; the tap mode is the
//! only recording consumer now.)

use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};

use super::filters::aec::FarEndRing;

/// Spawn an ffmpeg reading mono 48 kHz 32-bit-float PCM from a capture `source` to its
/// stdout. On Linux `source` is a PulseAudio source name captured via `-f pulse`; on
/// macOS it is an avfoundation device NAME (from [`crate::audio::config::mic_source`];
/// numeric strings would be treated as device indexes, but the ids we persist are
/// names — stable across replugs, and a stale one fails the open loudly) captured via
/// `-f avfoundation -i ":<source>"` — the LEADING COLON selects an
/// audio-only device (avfoundation's `"[video]:[audio]"` input grammar; no video).
/// `PR_SET_PDEATHSIG` keeps it from orphaning if we exit (Linux only). Returns the child
/// plus its piped stdout, or None if ffmpeg won't start.
fn spawn_pulse_pcm(source: &str) -> Option<(Child, std::process::ChildStdout)> {
    let mut cmd = crate::util::ffmpeg_command();
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    // Linux: PulseAudio source. macOS: an avfoundation device NAME (leading colon =
    // audio-only). Windows (DRAGON-229 M3): a DirectShow capture device — the stable
    // ALTERNATIVE name (resolved from the persisted/`default` source by the platform body;
    // format normalized to mono 48k f32 below, so the DSP chain downstream is untouched).
    #[cfg(target_os = "linux")]
    cmd.args(["-f", "pulse", "-i", source]);
    #[cfg(target_os = "macos")]
    cmd.args(["-f", "avfoundation", "-i", &format!(":{source}")]);
    #[cfg(windows)]
    {
        let dev = crate::platform::windows::audio::resolve_mic_device(source)?;
        // `audio_buffer_size` (ms) is THE dshow latency knob: without it, DirectShow hands
        // ffmpeg the device's DEFAULT capture buffer, typically a multiple of ~500ms, so PCM
        // arrives in ~500ms bursts — the recording mic's first blocks land hundreds of ms
        // behind the media clock (dropped as "late") and the on-button meter only refreshes
        // ~twice a second (DRAGON-248 bug 1: the gradient "barely moves"). A 50ms buffer makes
        // dshow deliver ~50ms chunks, matching the ~25ms pulse fragments / small avfoundation
        // buffers the Linux/mac arms get, so the meter animates smoothly and the mic stream
        // stays close to real time. It is small enough for low latency yet comfortably above a
        // shared-mode device period, so it does not risk dropouts. (Input option → before `-i`.)
        cmd.args(["-f", "dshow", "-audio_buffer_size", "50", "-i", &format!("audio={dev}")]);
    }
    cmd.args(["-ac", "1", "-ar", "48000", "-f", "f32le", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Kill the ffmpeg child if we die (PR_SET_PDEATHSIG). Linux-only; macOS has no
    // equivalent, so there the DoneGuard / explicit reaping handles cleanup.
    // SAFETY: only an async-signal-safe syscall in the forked child before exec.
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            let _ = rustix::process::set_parent_process_death_signal(Some(
                rustix::process::Signal::KILL,
            ));
            Ok(())
        });
    }
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take()?;
    Some((child, stdout))
}

/// Ask a capture child to stop gracefully (SIGTERM → ffmpeg flushes its input queue to
/// stdout and exits) and reap it, waiting a BOUNDED moment; one that doesn't die —
/// SIGTERM can't take effect while the child is blocked writing a full stdout nobody
/// drains (the DRAGON-118 wedge) — is SIGKILLed so the stop tail can't hang. Returns
/// whether the graceful path won (the flushed tail made it out).
fn term_then_wait(child: &mut Child) -> bool {
    // DRAGON-229: SIGTERM (the graceful "flush your tail and exit" ask) is POSIX-only;
    // Windows has no signal equivalent, so the bounded wait below simply falls through
    // to `child.kill()` (TerminateProcess). This mic-cleanup path does not run on
    // Windows in M0 (no audio capture yet); the M3 audio path revisits graceful stop.
    #[cfg(unix)]
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Err(_) => return true, // no child to wait on — nothing left to hang on
            Ok(None) => {}
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Shared by [`setup_clean_mic_tap`]: set up the DEDICATED AEC far-end reference
/// capture. Returns the monitor capture child (if spawned) and the ring buffer the
/// reader thread pulls render frames from — both None when echo cancellation is off
/// or the monitor capture fails to start. Since DRAGON-128 this is only the
/// EXPLICIT-SPEAKER path (and the recording-less diagnostics'): with the default
/// speaker, the owned recording's own system capture feeds the ring instead (see
/// `record::pipewire::try_start_owned_audio`) and no second capture is spawned.
fn spawn_aec_monitor(
    cfg: crate::audio::InputConfig,
    speaker: &str,
) -> (Option<Child>, Option<FarEndRing>) {
    use crate::audio::filters::aec::{new_far_end_ring, push_far_end_frame};
    use crate::audio::FRAME;
    use std::io::Read;

    // AEC far-end reference: capture the chosen sink's monitor (only when echo is on) into
    // a small ring buffer. A separate mono capture from the recorded system track; AEC3's
    // delay estimator aligns it to the mic, so it needs no sample-alignment with that track.
    let mut monitor_child: Option<Child> = None;
    let render_buf: Option<FarEndRing> = if cfg.echo_cancellation {
        let monitor = if speaker.trim().is_empty() {
            "@DEFAULT_MONITOR@".to_string()
        } else {
            format!("{}.monitor", speaker.trim())
        };
        match spawn_pulse_pcm(&monitor) {
            Some((mc, mstdout)) => {
                monitor_child = Some(mc);
                let rb = new_far_end_ring();
                let rb2 = rb.clone();
                std::thread::spawn(move || {
                    let mut r = std::io::BufReader::new(mstdout);
                    let mut b = [0u8; FRAME * 4];
                    loop {
                        if r.read_exact(&mut b).is_err() {
                            break;
                        }
                        let mut f = [0f32; FRAME];
                        for (i, c) in b.as_chunks::<4>().0.iter().enumerate() {
                            f[i] = f32::from_le_bytes(*c);
                        }
                        push_far_end_frame(&rb2, f);
                    }
                });
                Some(rb)
            }
            None => None,
        }
    } else {
        None
    };

    (monitor_child, render_buf)
}

// ---------------------------------------------------------------------------
// Tap mode (DRAGON-125; the ONLY recording-path mic consumer since DRAGON-127
// retired the legacy FIFO feeder): the media-clock owned pipeline needs cleaned-mic
// blocks handed to the mixer pump directly instead of written to a FIFO for ffmpeg
// to read. The DSP chain itself (`InputProcessor`, `spawn_pulse_pcm`,
// `spawn_aec_monitor`) is reused UNTOUCHED, per CLAUDE.md's audio CAUTION section.
// ---------------------------------------------------------------------------

/// Teardown handle for [`setup_clean_mic_tap`]: no FIFO to free a blocked writer
/// from (there is none) — the reader thread's channel send simply errors once the
/// pump drops its receiver, which is what ends the thread instead.
pub(crate) struct MicTapHandle {
    mic_child: Child,
    monitor_child: Option<Child>,
    reader: Option<std::thread::JoinHandle<()>>,
    latency_ms: f64,
}

impl MicTapHandle {
    /// The cleanup chain's processing latency (ms) — kept for parity / diagnostics,
    /// though `record::pump` bakes this into each tap's `capture_time` at the point
    /// the tap is produced (see [`spawn_tap_reader_thread`]), not at consumption.
    pub(crate) fn processing_latency_ms(&self) -> f64 {
        self.latency_ms
    }

    /// Stop the captures and bounded-join the reader thread. No FIFO to self-drain a
    /// wedged reader out of: once the captures are killed, `rdr.read_exact` fails and
    /// the loop ends on its own; this only bounds the (otherwise unlikely) case where
    /// the reader is instead blocked on a full/abandoned channel send.
    pub(crate) fn drain(&mut self) -> bool {
        let mut clean = term_then_wait(&mut self.mic_child);
        if let Some(mc) = self.monitor_child.as_mut() {
            clean &= term_then_wait(mc);
        }
        if let Some(r) = self.reader.take() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !r.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if r.is_finished() {
                let _ = r.join();
            } else {
                clean = false;
                log::warn!("clean-mic tap reader still stuck; detaching from it");
            }
        }
        clean
    }
}

impl Drop for MicTapHandle {
    fn drop(&mut self) {
        self.drain();
    }
}

/// Start the mic capture + (optional) AEC far-end monitor + [`InputProcessor`]
/// chain, handing each cleaned FRAME-sized block to the returned channel instead of
/// writing a FIFO — for `record::pump` (DRAGON-125) to place directly onto the
/// mixer's mic track (the ONLY recording-path mic consumer since DRAGON-127 retired
/// the legacy FIFO feeder this used to sit alongside). There is no ffmpeg-opens-the-
/// read-end rendezvous to wait for, so the reader starts consuming the mic capture's
/// stdout immediately (no pre-roll to discard — nothing had the chance to buffer
/// before a reader was attached). Returns `None` if the mic capture can't start
/// (the recording then fails outright — see `record::pipewire::try_start_owned_audio`).
///
/// `external_farend` (DRAGON-128): when the caller already runs a system capture
/// that can serve as the AEC's far-end reference (the owned recording's
/// `MonitorCapture`, via its tee), pass its ring here — the reader consumes it and
/// NO dedicated second monitor capture is spawned. `None` keeps the historical
/// dedicated capture (the explicit-speaker case, and callers with no system capture
/// of their own, like the `mic-rec-test` diagnostic).
pub(crate) fn setup_clean_mic_tap(
    cfg: crate::audio::InputConfig,
    speaker: &str,
    external_farend: Option<FarEndRing>,
) -> Option<(MicTapHandle, std::sync::mpsc::Receiver<crate::audio::filters::StreamTap>)> {
    use crate::audio::processing_latency_ms;

    let (mic_child, mic_stdout) = spawn_pulse_pcm(&crate::audio::config::mic_source())?;
    let (monitor_child, render_buf) = match external_farend {
        Some(ring) if cfg.echo_cancellation => (None, Some(ring)),
        _ => spawn_aec_monitor(cfg, speaker),
    };
    let l = processing_latency_ms(&cfg);
    // Generous bound: the pump renders/drains every ~100ms, so a few hundred ms of
    // headroom absorbs normal scheduling jitter while still applying real
    // backpressure (a blocking `send`) if the pump ever falls meaningfully behind.
    let (tx, rx) = std::sync::mpsc::sync_channel(256);
    let reader = spawn_tap_reader_thread(mic_stdout, render_buf, cfg, l, tx);
    Some((MicTapHandle { mic_child, monitor_child, reader: Some(reader), latency_ms: l }, rx))
}

/// The tap-mode reader thread: the SAME per-frame DSP loop as
/// [`spawn_reader_thread`] (feed the AEC far-end reference, run
/// [`crate::audio::InputProcessor::process`], rebuild on a DSP panic rather than drop
/// a frame) — only the output step differs (channel send instead of FIFO write).
///
/// Block timing follows the contiguous model (`crate::audio::capture`'s module doc,
/// via the same [`super::capture::StreamAnchor`]): the FIRST block's arrival −
/// `dsp_latency_ms` − the block's own 10 ms establishes the stream's anchor, and
/// every block is stamped `capture_time = audible_time = anchor + blocks·FRAME/48k`.
/// Stamping each block at its own arrival (the pre-integration model) leaks the mic
/// pipe's scheduling jitter into `mixer::Track`'s sample placement — ~100
/// micro-chops per second of recorded mic audio (the DRAGON-122-integration
/// garbling). Arrival drift beyond the re-anchor threshold (a stalled mic ffmpeg
/// recovering, a device swap) re-anchors with a loud log instead. `record::pump`
/// shifts `audible_time` further by the session's A/V-sync offset; nothing else
/// adjusts it.
fn spawn_tap_reader_thread(
    mic_stdout: std::process::ChildStdout,
    render_buf: Option<FarEndRing>,
    cfg: crate::audio::InputConfig,
    dsp_latency_ms: f64,
    tx: std::sync::mpsc::SyncSender<crate::audio::filters::StreamTap>,
) -> std::thread::JoinHandle<()> {
    use crate::audio::filters::StreamTap;
    use crate::audio::{InputProcessor, FRAME};
    use std::io::Read;

    let latency = std::time::Duration::from_secs_f64(dsp_latency_ms.max(0.0) / 1000.0);
    std::thread::spawn(move || {
        let mut rdr = std::io::BufReader::new(mic_stdout);
        let mut proc = InputProcessor::new(cfg);
        let mut bytes = [0u8; FRAME * 4];
        let mut inp = [0f32; FRAME];
        let mut pcm = [0f32; FRAME];
        // The stream's contiguous stamping clock (fn doc): anchored at the first
        // block, advanced by sample count, re-anchored only on a logged
        // discontinuity. The constant DSP latency is folded into the anchor by
        // shifting every arrival time fed to it.
        let mut anchor: Option<super::capture::StreamAnchor> = None;
        loop {
            if rdr.read_exact(&mut bytes).is_err() {
                break; // mic capture gone (stopped) -> end
            }
            for (i, c) in bytes.as_chunks::<4>().0.iter().enumerate() {
                inp[i] = f32::from_le_bytes(*c);
            }
            if let Some(rb) = render_buf.as_ref() {
                let rf = rb.lock().ok().and_then(|mut q| q.pop_front()).unwrap_or([0.0; FRAME]);
                proc.feed_render(&rf);
            }
            let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                proc.process(&inp, Some(&mut pcm));
            }))
            .is_ok();
            if !ok {
                pcm = [0.0; FRAME];
                proc = InputProcessor::new(cfg);
            }
            let arrival = std::time::Instant::now() - latency;
            let a = anchor.get_or_insert_with(|| super::capture::StreamAnchor::new(FRAME, arrival));
            let before = a.reanchor_count();
            let (capture_time, drift) = a.stamp(FRAME, arrival);
            if a.reanchor_count() > before {
                log::warn!(
                    "clean-mic tap discontinuity: arrivals drifted {:.0} ms from the stream's \
                     contiguous clock — re-anchoring (#{})",
                    drift * 1000.0,
                    a.reanchor_count()
                );
            }
            if tx.send(StreamTap::new(pcm.to_vec(), capture_time, capture_time)).is_err() {
                break; // pump gone (recording stopped) -> end
            }
        }
    })
}

/// One waveform column: `(clean, raw, gate_in)` levels (0..1, dBFS-normalized). `clean` is the
/// final recorded level and `raw` the pre-cleanup level (their gap is what the filters removed);
/// `gate_in` is the level the voice gate decides on (denoised, pre-gate/gain) — the level the
/// Input Sensitivity bar shows so it matches what the threshold is compared against.
pub type MicColumn = (f32, f32, f32);

/// Start a live microphone test: an ffmpeg reading mono 48 kHz float PCM from `device`
/// (empty = system default), plus a reader thread that runs each 10 ms frame through the
/// shared [`InputProcessor`] cleanup chain (per `cfg`) and reduces it to a rolling
/// `(clean, raw)` RMS envelope (≈100 columns/sec, newest at the back, capped to
/// `columns`) on the meters' dBFS scale. When echo cancellation is on, the chosen
/// `speaker`'s monitor is captured as the AEC far-end reference. Returns the mic process
/// + shared buffer, or None if ffmpeg won't start. `PR_SET_PDEATHSIG` prevents orphans.
///
/// [`InputProcessor`]: crate::audio::InputProcessor
#[allow(clippy::type_complexity)]
pub fn spawn_mic_test(
    device: &str,
    columns: usize,
    cfg: crate::audio::InputConfig,
    speaker: &str,
) -> Option<(Child, Arc<Mutex<(std::collections::VecDeque<MicColumn>, usize)>>)> {
    use crate::audio::{InputProcessor, FRAME};
    use std::collections::VecDeque;

    let mic_source = if device.trim().is_empty() {
        "default".to_string()
    } else {
        device.trim().to_string()
    };
    let (child, stdout) = spawn_pulse_pcm(&mic_source)?;

    // Echo cancellation needs a live reference of what's going to the speakers: capture
    // the chosen sink's monitor as the AEC far-end. A dedicated thread reads it into a
    // small ring buffer; the mic loop pulls one render frame per capture frame (silence
    // if none queued), so an idle/suspended sink can never stall the mic waveform.
    let mut monitor_child: Option<Child> = None;
    let render_buf: Option<Arc<Mutex<VecDeque<[f32; FRAME]>>>> = if cfg.echo_cancellation {
        let monitor = if speaker.trim().is_empty() {
            "@DEFAULT_MONITOR@".to_string()
        } else {
            format!("{}.monitor", speaker.trim())
        };
        match spawn_pulse_pcm(&monitor) {
            Some((mc, mstdout)) => {
                monitor_child = Some(mc);
                let rb: Arc<Mutex<VecDeque<[f32; FRAME]>>> =
                    Arc::new(Mutex::new(VecDeque::with_capacity(16)));
                let rb2 = rb.clone();
                std::thread::spawn(move || {
                    use std::io::Read;
                    let mut r = std::io::BufReader::new(mstdout);
                    let mut b = [0u8; FRAME * 4];
                    loop {
                        if r.read_exact(&mut b).is_err() {
                            break; // monitor ffmpeg gone (test closed) -> stop
                        }
                        let mut f = [0f32; FRAME];
                        for (i, c) in b.as_chunks::<4>().0.iter().enumerate() {
                            f[i] = f32::from_le_bytes(*c);
                        }
                        if let Ok(mut q) = rb2.lock() {
                            if q.len() >= 16 {
                                q.pop_front(); // bound latency; drop oldest
                            }
                            q.push_back(f);
                        }
                    }
                });
                Some(rb)
            }
            None => None,
        }
    } else {
        None
    };

    let buf: Arc<Mutex<(std::collections::VecDeque<MicColumn>, usize)>> =
        Arc::new(Mutex::new((std::collections::VecDeque::with_capacity(columns), 0)));
    let shared = buf.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut rdr = std::io::BufReader::new(stdout);
        let mut proc = InputProcessor::new(cfg);
        let mut bytes = [0u8; FRAME * 4];
        let mut inp = [0f32; FRAME];
        loop {
            // Read a full frame; EOF/err means ffmpeg exited (dialog closed) -> stop.
            if rdr.read_exact(&mut bytes).is_err() {
                break;
            }
            for (i, c) in bytes.as_chunks::<4>().0.iter().enumerate() {
                inp[i] = f32::from_le_bytes(*c);
            }
            // Feed the far-end reference for this frame (silence if the monitor hasn't
            // produced one yet), so AEC3 can align and subtract the echo path.
            if let Some(rb) = render_buf.as_ref() {
                let rframe = rb
                    .lock()
                    .ok()
                    .and_then(|mut q| q.pop_front())
                    .unwrap_or([0.0; FRAME]);
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    proc.feed_render(&rframe)
                }));
            }
            // Process the frame; if a DSP stage panics (e.g. a debug-build range assert),
            // rebuild the processor and skip the frame rather than killing the reader —
            // so the waveform never freezes/restarts on a single bad frame.
            let o = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                proc.process(&inp, None)
            })) {
                Ok(o) => o,
                Err(_) => {
                    proc = InputProcessor::new(cfg);
                    continue;
                }
            };
            if let Ok(mut g) = shared.lock() {
                if g.0.len() >= columns {
                    g.0.pop_front();
                }
                g.0.push_back((o.clean, o.raw, o.gate_in));
                g.1 += 1; // total columns ever produced (for the waveform scroll)
            }
        }
        // Mic capture ended: tear down the monitor ffmpeg so its reader thread exits too.
        if let Some(mut mc) = monitor_child {
            let _ = mc.kill();
            let _ = mc.wait();
        }
    });
    Some((child, buf))
}
