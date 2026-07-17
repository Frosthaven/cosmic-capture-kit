//! The filter engine (DRAGON-124, phase 2 of the mixer epic DRAGON-122): the mic-cleanup
//! DSP stages, extracted out of `InputProcessor` behind a minimal seam so phase 3's mixer
//! can apply filter chains to any stream, not just the mic. This is where the DSP algorithms
//! LIVE now; `InputProcessor` (`super::input`) only composes these stages in order and
//! threads its config/VAD through them — it owns no DSP math of its own.
//!
//! This ticket is pure restructuring: every constant, the stage ordering, and the
//! drain/flush semantics around them are unchanged from before the move (see CLAUDE.md's
//! CAUTION section — "Do not change constants, ordering, drain/flush behavior, or
//! 'improve' the leveling").
//!
//! ## Chain order (do not reorder)
//!
//! ```text
//! far-end monitor ─▶ [AEC3 + WebRTC NS] ─▶ mic ─▶ [RNNoise] ─▶ [gate] ─▶ [AGC + limiter]
//! ```
//!
//! - [`aec::WebRtcApm`] wraps the `sonora` crate (a pure-Rust WebRTC AudioProcessing
//!   port): AEC3 echo cancellation AND the WebRTC noise-suppression facet, in one
//!   object built from one `Config`. Both are gated independently
//!   (`echo_cancellation` / `noise_suppression`) but are not separably implemented.
//! - [`noise::RnnDenoiser`] wraps `nnnoiseless`: a second, fully separate
//!   noise-suppression engine layered on the AEC/WebRTC-NS pass's output, gated by the
//!   SAME `noise_suppression` flag as the WebRTC-NS facet above (two different engines,
//!   one on/off switch) — and the source of the chain's voice-activity probability
//!   whenever Advanced Voice Activity ([`vad::Vad`]) is off.
//! - [`vad::Vad`] (earshot, neural) is an ANALYZER, not a sample-mutating filter: it
//!   never touches the samples, only reports a voice-activity probability that
//!   supersedes RNNoise's when Advanced Voice Activity is enabled.
//! - [`gate::Gate`] is the voice gate ("Input Sensitivity"): opens on speech, closes on
//!   silence, applied to the samples BEFORE auto-gain (so its decision reflects the
//!   mic's natural, un-boosted loudness).
//! - [`agc::AutoGain`] is the VAD-driven auto-gain AND the never-red/never-clip limiter
//!   (the limiter is not a separate stage — it's the safety tail of `AutoGain::process`),
//!   applied LAST so it lifts what actually gets recorded.
//!
//! ## The seam
//!
//! [`AudioFilter`] is implemented by the two LINEAR stages — [`gate::Gate`] and
//! [`agc::AutoGain`] — that mutate one FRAME-sized block in place given this frame's
//! config and voice-activity probability. Its shape mirrors exactly what
//! `InputProcessor` already threaded to these two stages before this move (the gate
//! needs `cfg` for its threshold/mode, both need `vad`); it is not an idealized,
//! invented-from-scratch filter API.
//!
//! [`aec::WebRtcApm`] and [`noise::RnnDenoiser`] deliberately do NOT implement
//! [`AudioFilter`]:
//! - AEC is a sidechain consumer — it needs a far-end reference fed separately
//!   (`feed_render`), which a single `process_block(&mut self, samples)` call can't
//!   express.
//! - RNNoise's real call shape returns a voice-activity probability as an output
//!   (consumed downstream), which the trait's `()`-returning `process_block` can't
//!   carry without either dropping that probability or inventing a side channel — both
//!   riskier than giving it its own natural shape.
//!
//! Both keep the two-argument (or sidechain) shape the original code already had;
//! forcing them into `AudioFilter` would be a redesign, not code motion.
//!
//! [`duck::Ducker`] (DRAGON-128) is the SYSTEM stream's filter stage — a sidechain
//! consumer like the AEC (mic tap in, system gain out), so it likewise keeps its own
//! two-entry-point shape instead of `AudioFilter`. It is fed and applied by
//! `record::pump`, the one place both streams meet on a single thread.

pub(crate) mod aec;
pub(crate) mod agc;
pub(crate) mod duck;
pub(crate) mod gate;
pub(crate) mod noise;
pub(crate) mod vad;

use super::config::{InputConfig, FRAME};

/// A linear DSP stage that can be composed into a chain: mutate one FRAME-sized block of
/// samples in place. `cfg` and `vad` are exactly the per-frame context `InputProcessor`
/// already threads to these stages ([`gate::Gate`] needs `cfg` for its threshold/mode and
/// `vad` for its speech decision; [`agc::AutoGain`] needs only `vad` and ignores `cfg`) —
/// this is the narrowest shape that unifies the two real call sites, not an idealized
/// generic filter API invented from scratch (see the module doc for the stages that
/// deliberately do NOT implement this trait).
///
/// Phase 3 (DRAGON-122) is expected to hold chains of these as `Vec<Box<dyn AudioFilter>>`.
pub(crate) trait AudioFilter {
    fn process_block(&mut self, samples: &mut [f32; FRAME], cfg: &InputConfig, vad: f32);

    /// Fixed processing latency this stage adds (ms), if any. Default 0 — both current
    /// implementers (gate, AGC) add none; `config::processing_latency_ms` remains the
    /// authoritative chain-level model (see its doc for why it is not derived from this).
    /// Not called from production code yet (no chain walks stages to sum this) — kept for
    /// phase 3 and exercised by `tests::linear_stages_default_to_zero_latency` below.
    #[allow(dead_code)]
    fn latency_ms(&self) -> f64 {
        0.0
    }
}

/// One tapped block of a stream, stamped on BOTH timebases DRAGON-122's dual-timebase
/// invariant requires a phase-3 mixer consumer to pick from:
///
/// - **`capture_time`**: when the block was actually captured/arrived off the device.
///   The AEC far-end reference must tap the system track at this, capture/mix-point,
///   time — preserving the timing characteristics `spawn_aec_monitor`'s delay
///   estimation is tuned against today.
/// - **`audible_time`**: when the block will actually be heard (capture time plus
///   whatever device/processing latency separates the two). Recording placement of a
///   tapped track uses THIS base, so it lands in sync with what a listener actually
///   hears (DRAGON-119's audible-time stamping, generalized).
///
/// One capture then serves both consumers — each picks its own base off the SAME tap
/// rather than needing a second, separately-timed capture (realized in DRAGON-128:
/// the owned system capture's tee feeds the AEC far-end ring at capture time —
/// `aec::FarEndFeeder` — retiring `spawn_aec_monitor`'s second capture for the
/// default-speaker case). Consumed since DRAGON-125 chunk B1: the mixer
/// ([`crate::mixer::Track`]) places taps by `audible_time`, produced by
/// [`crate::audio::clean_mic::setup_clean_mic_tap`] and `record::pump` — the
/// forward-looking `#[allow(dead_code)]`s this carried while ahead of those
/// consumers went with that wiring.
pub(crate) struct StreamTap {
    pub samples: Vec<f32>,
    pub capture_time: std::time::Instant,
    pub audible_time: std::time::Instant,
}

impl StreamTap {
    pub(crate) fn new(
        samples: Vec<f32>,
        capture_time: std::time::Instant,
        audible_time: std::time::Instant,
    ) -> Self {
        Self { samples, capture_time, audible_time }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn stream_tap_carries_both_timebases_independently() {
        let capture_time = Instant::now();
        let audible_time = capture_time + Duration::from_millis(20);
        let tap = StreamTap::new(vec![0.1, 0.2, 0.3], capture_time, audible_time);
        assert_eq!(tap.samples, vec![0.1, 0.2, 0.3]);
        assert_eq!(tap.capture_time, capture_time);
        assert_eq!(tap.audible_time, audible_time);
        assert!(tap.audible_time > tap.capture_time);
    }

    /// Both current `AudioFilter` implementers add no fixed latency — matching
    /// `config::processing_latency_ms`'s model ("AGC2, the VAD, the gate, and the
    /// high-pass add no fixed delay"). If a future change to either stage needs to
    /// override `latency_ms`, `processing_latency_ms`'s doc/tests must be revisited
    /// alongside it — this test is the tripwire linking the two.
    #[test]
    fn linear_stages_default_to_zero_latency() {
        let gate = gate::Gate::new();
        let agc = agc::AutoGain::new();
        assert_eq!(AudioFilter::latency_ms(&gate), 0.0);
        assert_eq!(AudioFilter::latency_ms(&agc), 0.0);
    }
}
