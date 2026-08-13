//! The combined WebRTC AudioProcessing engine (the `sonora` crate — a pure-Rust WebRTC
//! AudioProcessing port): echo cancellation (AEC3) AND the WebRTC noise-suppression
//! facet, in ONE object. The two are config-gated independently
//! (`cfg.echo_cancellation` / `cfg.noise_suppression`) but are NOT separably
//! IMPLEMENTED — sonora exposes both as fields of one `Config`, built into one
//! `AudioProcessing` instance, so they move together. RNNoise (`super::noise`) is a
//! wholly separate, later engine also gated by `noise_suppression`; see the `filters`
//! module doc for the full chain and why the two aren't the same code despite sharing
//! a config flag.
//!
//! This is the sidechain consumer the `filters` seam calls out as deliberately special
//! (DRAGON-122/124): AEC needs a far-end (speaker monitor) reference fed via
//! [`WebRtcApm::feed_render`] before/around each capture-side call, so it cannot be a
//! uniform in-place [`super::AudioFilter`] — it keeps the two-method shape the code
//! already had on `InputProcessor` (a render feed + a capture pass), just moved here
//! verbatim.

use crate::audio::config::{InputConfig, FRAME};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// The far-end reference ring: FRAME-sized mono 48 kHz blocks, produced by whichever
/// monitor capture is serving as the echo reference and consumed by the mic DSP loop
/// (one frame per capture frame, silence when empty — see
/// `crate::audio::clean_mic::spawn_tap_reader_thread`). Bounded at
/// [`FAR_END_RING_CAP`], drop-oldest, so the reference stays fresh and a stalled
/// consumer can never build latency.
pub(crate) type FarEndRing = Arc<Mutex<VecDeque<[f32; FRAME]>>>;

/// Ring capacity: 16 frames = 160 ms of far-end headroom. AEC3's delay estimator
/// self-aligns within a window comfortably wider than this, so drop-oldest freshness
/// matters more than depth (the same bound `spawn_aec_monitor` has always used).
pub(crate) const FAR_END_RING_CAP: usize = 16;

pub(crate) fn new_far_end_ring() -> FarEndRing {
    Arc::new(Mutex::new(VecDeque::with_capacity(FAR_END_RING_CAP)))
}

/// Push one frame, dropping the oldest at capacity — the ring's single write idiom,
/// shared by the dedicated-capture reader and [`FarEndFeeder`].
pub(crate) fn push_far_end_frame(ring: &FarEndRing, frame: [f32; FRAME]) {
    if let Ok(mut q) = ring.lock() {
        if q.len() >= FAR_END_RING_CAP {
            q.pop_front();
        }
        q.push_back(frame);
    }
}

/// Adapts the OWNED system capture's chunks into far-end frames (DRAGON-128, the
/// mixer epic's phase 4): interleaved stereo 48 kHz in (any chunk size — pulse
/// fragments don't align to FRAME), downmixed mono FRAME blocks out, pushed into the
/// ring as they fill. This is what lets ONE monitor capture serve both the recording
/// (placed by `audible_time`) and the AEC reference (fed at capture time, straight
/// off the capture thread — preserving the near-real-time framing the delay
/// estimator is tuned against; see `StreamTap`'s dual-timebase doc), retiring the
/// second `spawn_aec_monitor` capture for the default-speaker case.
pub(crate) struct FarEndFeeder {
    ring: FarEndRing,
    /// Mono samples downmixed but not yet a full FRAME (< FRAME entries).
    pending: Vec<f32>,
}

impl FarEndFeeder {
    pub(crate) fn new(ring: FarEndRing) -> Self {
        Self { ring, pending: Vec::with_capacity(FRAME * 2) }
    }

    /// Feed one interleaved stereo chunk: downmix `(L+R)/2` (the same fold-down the
    /// dedicated capture's `ffmpeg -ac 1` applied), emit every completed FRAME.
    pub(crate) fn feed_interleaved_stereo(&mut self, samples: &[f32]) {
        for [l, r] in samples.as_chunks::<2>().0 {
            self.pending.push((l + r) * 0.5);
        }
        while self.pending.len() >= FRAME {
            let mut frame = [0f32; FRAME];
            frame.copy_from_slice(&self.pending[..FRAME]);
            self.pending.drain(..FRAME);
            push_far_end_frame(&self.ring, frame);
        }
    }
}

/// The combined AEC3 + WebRTC-NS engine, present only when at least one of
/// `noise_suppression` / `echo_cancellation` is enabled (`None` otherwise — a pure
/// passthrough, exactly like the `Option` this replaces on `InputProcessor`).
///
/// # This type CONTAINS sonora's panics, and that is load-bearing (DRAGON-664)
///
/// `sonora-aec3` panics, in normal operation, on any recording long enough for the
/// adaptive filter to leave its initial state. `adaptive_fir_filter::zero_filter`
/// clears the partitions in `old_size..new_size` by SLICING (`&mut h[old..new]`),
/// while the WebRTC C++ it is ported from writes `for (k = old; k < new; ++k)`, which
/// is simply an empty loop when the filter SHRINKS. In Rust that same shrink is an
/// inverted range, so it aborts the frame with
/// `slice index starts at 13 but ends at 12`.
///
/// The shrink is not exotic, it is the design: `exit_initial_state` grows the refined
/// filter from `refined_initial.length_blocks` (12) to `refined.length_blocks` (13)
/// after ~2.5 s, and a later delay change (a new detected echo-path delay, or a render
/// buffer flush) makes `Subtractor::handle_echo_path_change` reset it straight back to
/// 12. Measured on a synthetic echo path, that lands every 15-30 s of audio, which
/// matches the field reports of 8-31 s.
///
/// We cannot dodge it from the call site. The only knob that would prevent the shrink
/// is the AEC3 filter length itself, and changing that is a DSP RETUNE, which
/// CLAUDE.md's audio CAUTION section forbids. So both sonora entry points run inside a
/// `catch_unwind` and a panicking engine is REBUILT FROM THE SAME `InputConfig`, never
/// a different one: this is crash recovery, not a configuration change.
///
/// Containment lives HERE, in the one module that owns the `sonora::AudioProcessing`
/// instance, rather than in each caller's frame loop. That way every caller is covered
/// by construction (the recording mic tap, the settings mic test, and anything added
/// later), on every platform, and a caller cannot half-cover it: the recording loop
/// wrapped its capture pass but not its render feed, while the mic test wrapped both.
/// It also keeps the blast radius to the failing stage. The callers' own
/// `catch_unwind`s rebuild the WHOLE `InputProcessor`, so a sonora panic used to throw
/// away the gate, the AGC, RNNoise and the VAD as well, every 15-30 s, on every
/// recording with echo cancellation on.
pub(crate) struct WebRtcApm {
    apm: sonora::AudioProcessing,
    /// The config the engine was built from, kept so [`Self::recover`] can rebuild a
    /// BYTE-IDENTICAL engine after a panic. `InputConfig` is `Copy`, so this is a plain
    /// value, not a borrow of the caller's settings.
    cfg: InputConfig,
    /// Scratch output buffer `process_render_f32` requires but whose contents nothing
    /// reads (the render call's job is to update the AEC's internal alignment state, not
    /// to produce a signal) — moved here verbatim from `InputProcessor` (DRAGON-124).
    rdest: [f32; FRAME],
    /// How many times this engine has panicked and been rebuilt. Counted so the reset is
    /// never silent: the first one is a `warn`, the rest are `debug` with the count.
    resets: u32,
}

impl WebRtcApm {
    /// Build the engine per `cfg`, or `None` when both noise suppression and echo
    /// cancellation are off.
    pub(crate) fn new(cfg: &InputConfig) -> Option<Self> {
        let apm = Self::engine(cfg)?;
        Some(Self { apm, cfg: *cfg, rdest: [0.0; FRAME], resets: 0 })
    }

    /// The engine builder, shared by [`Self::new`] and [`Self::recover`] so a rebuilt
    /// engine cannot drift from the original one's configuration.
    fn engine(cfg: &InputConfig) -> Option<sonora::AudioProcessing> {
        use sonora::{AudioProcessing, Config, StreamConfig};
        let sc = StreamConfig::new(48_000, 1);
        // First pass: AEC + noise suppression (NO gain — that's applied last, in agc.rs).
        (cfg.noise_suppression || cfg.echo_cancellation).then(|| {
            use sonora::config::{
                EchoCanceller, HighPassFilter, NoiseSuppression, NoiseSuppressionLevel,
            };
            let config = Config {
                high_pass_filter: cfg.noise_suppression.then(HighPassFilter::default),
                noise_suppression: cfg.noise_suppression.then_some(NoiseSuppression {
                    // With echo on, push the suppressor that cleans the linear AEC output
                    // to max so loud continuous far-end audio is removed more fully.
                    level: if cfg.echo_cancellation {
                        NoiseSuppressionLevel::VeryHigh
                    } else {
                        NoiseSuppressionLevel::High
                    },
                    analyze_linear_aec_output_when_available: cfg.echo_cancellation,
                }),
                echo_canceller: cfg.echo_cancellation.then(EchoCanceller::default),
                ..Default::default()
            };
            AudioProcessing::builder().config(config).capture_config(sc).render_config(sc).build()
        })
    }

    /// Feed the far-end (speaker monitor) reference for this frame, so AEC3 can align and
    /// subtract the echo path. Call before the capture pass each frame when echo is on.
    pub(crate) fn feed_render(&mut self, render: &[f32; FRAME]) {
        let apm = &mut self.apm;
        let rdest = &mut self.rdest;
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = apm.process_render_f32(&[&render[..]], &mut [&mut rdest[..]]);
        }))
        .is_ok();
        if !ok {
            self.recover();
        }
    }

    /// Run the capture-side pass (AEC + NS + high-pass, one WebRTC call) into `out`.
    ///
    /// On a panic `out` is silenced and the engine rebuilt, so the frame the engine could
    /// not finish reaches the recording as 10 ms of silence. That is exactly what the
    /// callers' own recovery already wrote for such a frame; what changes is that the
    /// stages after this one keep their state instead of being rebuilt with it.
    pub(crate) fn process_capture(&mut self, capture: &[f32; FRAME], out: &mut [f32; FRAME]) {
        let apm = &mut self.apm;
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = apm.process_capture_f32(&[&capture[..]], &mut [&mut out[..]]);
        }))
        .is_ok();
        if !ok {
            out.fill(0.0);
            self.recover();
        }
    }

    /// Replace an engine that panicked mid-frame with a fresh one on the SAME config.
    ///
    /// Unwinding out of a `&mut self` method leaves the old engine logically
    /// inconsistent, so it is dropped rather than reused. sonora is pure Rust with no
    /// FFI and no global state, so dropping it is an ordinary drop; nothing leaks and
    /// nothing else in the process is affected. A rebuild measured 0.23 ms, so even the
    /// pathological case (an input that panics on every 10 ms frame) degrades to
    /// continuous silence at a few percent of one core, never to a crash or a stall.
    fn recover(&mut self) {
        self.resets = self.resets.saturating_add(1);
        if self.resets == 1 {
            log::warn!(
                "mic cleanup: the WebRTC AEC/NS engine panicked mid-frame and was rebuilt \
                 (a known sonora-aec3 defect on adaptive-filter shrink, DRAGON-664); that \
                 frame is 10 ms of silence and echo cancellation re-converges from here"
            );
        } else {
            log::debug!(
                "mic cleanup: WebRTC AEC/NS engine rebuilt after a panic (#{})",
                self.resets
            );
        }
        match Self::engine(&self.cfg) {
            Some(apm) => self.apm = apm,
            // Unreachable in practice: `new` already built an engine from this exact
            // config, and `engine` only declines when both stages are off. Kept honest
            // rather than unwrapped: the old engine stays, so the next frame is caught
            // and silenced the same way instead of taking the thread down.
            None => log::warn!("mic cleanup: could not rebuild the WebRTC AEC/NS engine"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(noise_suppression: bool, echo_cancellation: bool) -> InputConfig {
        InputConfig {
            noise_suppression,
            echo_cancellation,
            auto_gain: false,
            gate: false,
            gate_auto: true,
            gate_threshold: 0.5,
            advanced_vad: false,
        }
    }

    #[test]
    fn new_is_none_when_both_stages_are_off() {
        assert!(WebRtcApm::new(&cfg(false, false)).is_none());
    }

    #[test]
    fn new_is_some_when_either_stage_is_on() {
        assert!(WebRtcApm::new(&cfg(true, false)).is_some());
        assert!(WebRtcApm::new(&cfg(false, true)).is_some());
        assert!(WebRtcApm::new(&cfg(true, true)).is_some());
    }

    #[test]
    fn feed_render_then_process_capture_runs_without_panic() {
        let mut apm = WebRtcApm::new(&cfg(true, true)).expect("both stages on");
        let render = [0.0f32; FRAME];
        let capture = [0.01f32; FRAME];
        let mut out = [0.0f32; FRAME];
        apm.feed_render(&render);
        apm.process_capture(&capture, &mut out);
        assert_eq!(out.len(), FRAME);
    }

    // ---- DRAGON-664: sonora's panics are contained here, not at the call sites ----

    /// The recovery contract, pinned deterministically on every platform: a rebuilt
    /// engine works, carries the SAME config, and processing after a rebuild does not
    /// keep resetting. `recover` is driven directly because whether sonora's delay
    /// estimator trips its own defect depends on the SIMD backend and the signal, and
    /// this contract must hold everywhere regardless.
    #[test]
    fn recover_rebuilds_a_working_engine_and_counts_the_reset() {
        let c = cfg(true, true);
        let mut apm = WebRtcApm::new(&c).expect("both stages on");
        apm.recover();
        assert_eq!(apm.resets, 1, "the reset is counted, never silent");
        assert!(apm.cfg.noise_suppression && apm.cfg.echo_cancellation, "config is kept");

        let mut out = [0.0f32; FRAME];
        apm.feed_render(&[0.0; FRAME]);
        apm.process_capture(&[0.01; FRAME], &mut out);
        assert!(out.iter().all(|s| s.is_finite()), "the rebuilt engine still processes");
        assert_eq!(apm.resets, 1, "a healthy frame after a rebuild must not reset again");
    }

    /// The live one: drive a real echo path whose DELAY CHANGES, which is what makes
    /// AEC3 shrink its adaptive filter and panic inside `zero_filter`. The assertion is
    /// that the caller survives every frame, because that is the whole contract; the
    /// engine reset count is reported, not asserted, since whether the estimator
    /// re-detects depends on sonora's SIMD backend.
    ///
    /// If this ever panics, the `catch_unwind` in `feed_render` / `process_capture` has
    /// been removed or bypassed, and every recording with echo cancellation on is losing
    /// its mic thread. About 0.4 s of wall time for 14 s of synthetic audio.
    #[test]
    fn a_shrinking_adaptive_filter_cannot_escape_as_a_panic() {
        let mut apm = WebRtcApm::new(&cfg(true, true)).expect("both stages on");
        let mut out = [0.0f32; FRAME];
        // Deterministic wideband far end, so the delay estimator has something to lock
        // onto; a long history so the near end can be a delayed copy of it.
        let mut st: u32 = 0x1234_5678;
        let mut rng = move || {
            st ^= st << 13;
            st ^= st >> 17;
            st ^= st << 5;
            (st as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let mut history = vec![0.0f32; 48_000 * 4];
        let mut hpos = 0usize;
        for i in 0..1400 {
            // The echo path moves once, AFTER the filter has left its initial state
            // (~2.5 s) and grown, which is the shrink's precondition.
            let delay = if i <= 500 { 480 } else { 3400 };
            let mut render = [0.0f32; FRAME];
            let mut capture = [0.0f32; FRAME];
            for k in 0..FRAME {
                let t = (i * FRAME + k) as f32 / 48_000.0;
                let s = 0.35 * rng() + 0.25 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
                render[k] = s * (0.6 + 0.4 * (2.0 * std::f32::consts::PI * 0.7 * t).sin());
                history[hpos] = render[k];
                let src = (hpos + history.len() - delay) % history.len();
                capture[k] = 0.5 * history[src]
                    + 0.05 * (2.0 * std::f32::consts::PI * 210.0 * t).sin()
                    + 0.01 * rng();
                hpos = (hpos + 1) % history.len();
            }
            apm.feed_render(&render);
            apm.process_capture(&capture, &mut out);
            assert!(out.iter().all(|s| s.is_finite()), "frame {i} produced a non-finite sample");
        }
        println!("engine resets over 14 s of audio: {}", apm.resets);
    }

    // ---- FarEndFeeder (DRAGON-128): stereo chunks → mono FRAME blocks ----

    #[test]
    fn far_end_feeder_downmixes_and_reframes_across_chunk_boundaries() {
        let ring = new_far_end_ring();
        let mut feeder = FarEndFeeder::new(ring.clone());
        // 1.5 FRAMEs of stereo where L=0.2, R=0.4 → mono 0.3, split across two
        // chunks that don't align to FRAME (like real pulse fragments).
        let stereo: Vec<f32> =
            std::iter::repeat_n([0.2f32, 0.4f32], FRAME + FRAME / 2).flatten().collect();
        feeder.feed_interleaved_stereo(&stereo[..FRAME]); // 240 stereo frames: no full block yet
        assert_eq!(ring.lock().unwrap().len(), 0);
        feeder.feed_interleaved_stereo(&stereo[FRAME..]);
        let q = ring.lock().unwrap();
        assert_eq!(q.len(), 1, "exactly one full FRAME completed");
        assert!(q[0].iter().all(|&s| (s - 0.3).abs() < 1e-6), "downmix is (L+R)/2");
    }

    #[test]
    fn far_end_ring_drops_oldest_at_capacity() {
        let ring = new_far_end_ring();
        for i in 0..(FAR_END_RING_CAP + 3) {
            push_far_end_frame(&ring, [i as f32; FRAME]);
        }
        let q = ring.lock().unwrap();
        assert_eq!(q.len(), FAR_END_RING_CAP);
        assert_eq!(q[0][0], 3.0, "the oldest frames were dropped, freshest kept");
        assert_eq!(q[FAR_END_RING_CAP - 1][0], (FAR_END_RING_CAP + 2) as f32);
    }
}
