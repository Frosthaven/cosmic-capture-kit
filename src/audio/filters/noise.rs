//! RNNoise (`nnnoiseless`): the residual denoiser layered AFTER the combined AEC/WebRTC-NS
//! pass (`super::aec`), cleaning what that pass didn't remove and yielding a per-frame
//! voice-activity probability as a free side effect of its own inference. It is a fully
//! separate, standalone engine (its own crate object, its own scratch buffers) from the
//! WebRTC noise-suppression facet living in `aec.rs`'s combined object — the two are both
//! gated by the SAME `noise_suppression` config flag, but are NOT the same code (see the
//! `filters` module doc for the full chain order).
//!
//! Does not implement [`super::AudioFilter`]: its real call shape returns the
//! voice-activity probability as an output (consumed by the VAD/gate stages downstream in
//! `InputProcessor::process`), which a `fn(&mut self, samples: &mut [f32])`-shaped trait
//! method can't carry without either discarding it (a functionality loss, not a refactor)
//! or inventing a side channel — both riskier than giving it its own natural shape, like
//! AEC.

use crate::audio::config::FRAME;

/// Stateful RNNoise wrapper: the `nnnoiseless` engine plus its i16-range scratch buffers
/// (nnnoiseless processes in roughly a `[-32768, 32768]` range, not `[-1, 1]`).
pub(crate) struct RnnDenoiser {
    state: Box<nnnoiseless::DenoiseState<'static>>,
    // scratch buffers (avoid per-frame allocation), moved here verbatim from
    // `InputProcessor` (DRAGON-124) — used ONLY by `process`, never read elsewhere.
    i16f: [f32; FRAME],
    out: [f32; FRAME],
}

impl RnnDenoiser {
    pub(crate) fn new() -> Self {
        Self { state: nnnoiseless::DenoiseState::new(), i16f: [0.0; FRAME], out: [0.0; FRAME] }
    }

    /// Denoise one frame into `out`, returning the voice-activity probability nnnoiseless
    /// computes as a side effect (RNNoise's contribution to the chain's VAD, taking
    /// precedence unless Advanced Voice Activity — [`super::vad::Vad`] — is enabled; see
    /// `InputProcessor::process`). The scale conversions (`* 32768.0` / `* (1.0 /
    /// 32768.0)`) are exactly the ones the chain has always used.
    pub(crate) fn process(&mut self, input: &[f32; FRAME], out: &mut [f32; FRAME]) -> f32 {
        for (o, &s) in self.i16f.iter_mut().zip(input.iter()) {
            *o = s * 32768.0; // nnnoiseless works in the i16 range
        }
        let vad = self.state.process_frame(&mut self.out, &self.i16f);
        for (c, &s) in out.iter_mut().zip(self.out.iter()) {
            *c = s * (1.0 / 32768.0);
        }
        vad
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_runs_without_panic_and_reports_a_probability() {
        let mut d = RnnDenoiser::new();
        let input = [0.01f32; FRAME];
        let mut out = [0.0f32; FRAME];
        let vad = d.process(&input, &mut out);
        assert!((0.0..=1.0).contains(&vad), "vad probability out of range: {vad}");
    }

    #[test]
    fn silence_in_stays_small_out() {
        // Basic sanity check on the scale-conversion wiring (a mixed-up 32768.0 factor
        // would blow this up to a very different magnitude) — not a claim about
        // nnnoiseless's exact internal behavior.
        let mut d = RnnDenoiser::new();
        let input = [0.0f32; FRAME];
        let mut out = [0.0f32; FRAME];
        for _ in 0..5 {
            d.process(&input, &mut out);
        }
        let peak = out.iter().fold(0f32, |m, &s| m.max(s.abs()));
        assert!(peak < 0.05, "silence produced unexpectedly large output: peak={peak}");
    }
}
