//! The stateful per-stream input processor: it runs the AEC / NS / AGC2 / RNNoise / VAD /
//! gate chain over 480-sample frames and emits per-frame metering. The stage
//! implementations live in `filters/` (DRAGON-124); this module only composes them in
//! order and threads `cfg`/the per-frame VAD through — see `filters/mod.rs`'s module doc
//! for the chain order guarantee and the CAUTION pointer.

use super::config::{InputConfig, FRAME};
use super::filters::{
    aec::WebRtcApm, agc::AutoGain, gate::Gate, noise::RnnDenoiser, vad::Vad, AudioFilter,
};
use super::meter::{level_to_meter, rms};

/// One processed frame's metering outputs (levels are 0..1 on the meter dBFS scale).
pub struct FrameMeters {
    /// Final recorded level: gated + auto-gained (what the waveform shows).
    pub clean: f32,
    /// Raw pre-cleanup level, for the "removed" overlay behind the waveform.
    pub raw: f32,
    /// The level the voice gate decides on — denoised, BEFORE the gate and auto-gain. This is
    /// what the Input Sensitivity threshold is compared against, so it's what that bar shows.
    pub gate_in: f32,
    /// Whether the gate is currently open (speech passing).
    pub open: bool,
    /// Voice-activity probability this frame (0..1).
    pub vad: f32,
}

/// Stateful per-stream processor. Build once per capture, feed 480-sample frames.
pub struct InputProcessor {
    cfg: InputConfig,
    apm: Option<WebRtcApm>,
    /// Auto-gain applied LAST — after AEC/NS/RNNoise — so it lifts the final transformed
    /// vocals (not an intermediate signal a later denoiser would pull back down) and is driven
    /// by the chain's VAD, targeting the meters' ideal band with a hard ceiling below red.
    agc: Option<AutoGain>,
    denoise: Option<RnnDenoiser>,
    vad: Option<Vad>,
    // scratch buffer (avoid per-frame allocation): the AEC/NS pass's output, handed to the
    // next stage (RNNoise or passthrough). The other scratch buffers this used to hold
    // (`rdest`, `i16f`, `out`) now live on the stage that exclusively owns them
    // (`filters::aec::WebRtcApm`, `filters::noise::RnnDenoiser` respectively).
    ns: [f32; FRAME],
    // voice-gate state machine (owns its own click-free gain-ramp state since DRAGON-124)
    gate: Gate,
}

impl InputProcessor {
    pub fn new(cfg: InputConfig) -> Self {
        let apm = WebRtcApm::new(&cfg);
        // Final stage: our own VAD-driven auto-gain, applied to the cleaned vocals (see
        // filters/agc.rs). Matched to this app's RMS metering — it lands the level in the
        // green band and hard-caps below red — rather than WebRTC AGC2's peak-headroom
        // targeting, which under-boosts here.
        let agc = cfg.auto_gain.then(AutoGain::new);
        Self {
            cfg,
            apm,
            agc,
            denoise: cfg.noise_suppression.then(RnnDenoiser::new),
            vad: cfg.advanced_vad.then(Vad::new),
            ns: [0.0; FRAME],
            gate: Gate::new(),
        }
    }

    /// Feed the far-end (speaker monitor) reference for this frame, so AEC3 can align and
    /// subtract the echo path. Call before `process` each frame when echo is on.
    pub fn feed_render(&mut self, render: &[f32; FRAME]) {
        if let Some(apm) = self.apm.as_mut() {
            apm.feed_render(render);
        }
    }

    /// Process one capture frame, returning its metering outputs. When `pcm_out` is
    /// `Some`, it is filled with the cleaned + gated mono PCM (for recording); the mic
    /// test passes `None` and uses only the returned levels.
    pub fn process(
        &mut self,
        capture: &[f32; FRAME],
        pcm_out: Option<&mut [f32; FRAME]>,
    ) -> FrameMeters {
        let raw = rms(capture);

        // AEC + noise suppression + AGC2 (one WebRTC pass), else passthrough.
        if let Some(apm) = self.apm.as_mut() {
            apm.process_capture(capture, &mut self.ns);
        } else {
            self.ns.copy_from_slice(capture);
        }

        // RNNoise on the residual; it also returns a voice-activity probability. Copy
        // the result into a local frame (always [-1, 1]) so we don't hold a borrow of
        // `self` across the &mut-self VAD call below.
        let mut rnn_vad = -1.0f32;
        let mut cleaned = [0f32; FRAME];
        if let Some(d) = self.denoise.as_mut() {
            rnn_vad = d.process(&self.ns, &mut cleaned);
        } else {
            cleaned.copy_from_slice(&self.ns);
        }

        // Voice-activity probability: earshot (advanced) takes precedence, else the
        // RNNoise probability, else 1.0 (level-only gating).
        let mut vad = if rnn_vad >= 0.0 { rnn_vad } else { 1.0 };
        if let Some(v) = self.vad.as_mut() {
            v.push_samples(&cleaned);
            vad = v.last();
        }

        // Voice gate, BEFORE auto-gain: it decides on the denoised signal's NATURAL loudness, so
        // the Input Sensitivity threshold is relative to your real mic level (not an auto-levelled
        // one). `gate_in` is that decision level, surfaced for the settings bar.
        let gate_in = rms(&cleaned);
        self.gate.process_block(&mut cleaned, &self.cfg, vad);

        // Auto-gain LAST: lift the gated speech toward the meters' ideal band (see agc.rs) — on
        // the final signal, so the boost is what gets recorded and the level it targets is real.
        if let Some(ag) = self.agc.as_mut() {
            ag.process_block(&mut cleaned, &self.cfg, vad);
        }
        let clean_lin = rms(&cleaned);

        if let Some(out) = pcm_out {
            out.copy_from_slice(&cleaned);
        }

        FrameMeters {
            clean: level_to_meter(clean_lin),
            raw: level_to_meter(raw),
            gate_in: level_to_meter(gate_in),
            open: !self.cfg.gate || self.gate.open,
            vad,
        }
    }
}
