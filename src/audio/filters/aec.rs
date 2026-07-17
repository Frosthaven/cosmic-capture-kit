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
pub(crate) struct WebRtcApm {
    apm: sonora::AudioProcessing,
    /// Scratch output buffer `process_render_f32` requires but whose contents nothing
    /// reads (the render call's job is to update the AEC's internal alignment state, not
    /// to produce a signal) — moved here verbatim from `InputProcessor` (DRAGON-124).
    rdest: [f32; FRAME],
}

impl WebRtcApm {
    /// Build the engine per `cfg`, or `None` when both noise suppression and echo
    /// cancellation are off.
    pub(crate) fn new(cfg: &InputConfig) -> Option<Self> {
        use sonora::{AudioProcessing, Config, StreamConfig};
        let sc = StreamConfig::new(48_000, 1);
        // First pass: AEC + noise suppression (NO gain — that's applied last, in agc.rs).
        let apm = (cfg.noise_suppression || cfg.echo_cancellation).then(|| {
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
        })?;
        Some(Self { apm, rdest: [0.0; FRAME] })
    }

    /// Feed the far-end (speaker monitor) reference for this frame, so AEC3 can align and
    /// subtract the echo path. Call before the capture pass each frame when echo is on.
    pub(crate) fn feed_render(&mut self, render: &[f32; FRAME]) {
        let _ = self.apm.process_render_f32(&[&render[..]], &mut [&mut self.rdest[..]]);
    }

    /// Run the capture-side pass (AEC + NS + high-pass, one WebRTC call) into `out`.
    pub(crate) fn process_capture(&mut self, capture: &[f32; FRAME], out: &mut [f32; FRAME]) {
        let _ = self.apm.process_capture_f32(&[&capture[..]], &mut [&mut out[..]]);
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
