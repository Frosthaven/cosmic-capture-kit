//! The voice gate ("Input Sensitivity") state machine: it opens on speech and closes on
//! silence, driven by a voice-activity probability plus a manual or auto-tracked level
//! threshold, and ramps a per-frame gain (fast attack, slow release) for click-free gaps.

use crate::audio::config::{InputConfig, FRAME};
use crate::audio::meter::{lin_to_db, rms};

use super::AudioFilter;

// Gate tuning (per `audio-levels.md`), in 10 ms frames where relevant.
const HOLD_FRAMES: u32 = 20; // 200 ms hold: ride over inter-word gaps
const RELEASE_STEP: f32 = 0.04; // ~250 ms release ramp (1/25 per frame)
const ATTACK_STEP: f32 = 0.5; // ~2-frame attack: fast, no onset clipping
const VAD_OPEN: f32 = 0.65; // open above this voice probability
const VAD_CLOSE: f32 = 0.35; // close below this (probability hysteresis)
const LEVEL_OPEN_MARGIN: f32 = 10.0; // auto: open at noise_floor + 10 dB
const LEVEL_CLOSE_MARGIN: f32 = 4.0; // auto: close at noise_floor + 4 dB (6 dB hyst)
const FLOOR_FALL: f32 = 0.06; // floor tracker: fall fast (~150 ms)
const FLOOR_RISE: f32 = 0.003; // floor tracker: rise slow (~3 s)
const VAD_NONSPEECH: f32 = 0.3; // only update the floor when clearly not speech

/// Voice-gate state machine state.
pub(crate) struct Gate {
    floor_db: f32,
    gain: f32,
    hold: u32,
    /// Whether the gate is currently open (speech passing).
    pub(crate) open: bool,
    /// Last frame's gate gain, for click-free intra-frame ramping of PCM (moved here
    /// from `InputProcessor` — DRAGON-124 — so the gate can stand alone as an
    /// [`AudioFilter`] chain link; same one-per-stream lifetime, same ramp formula).
    prev_gain: f32,
}

impl Gate {
    pub(crate) fn new() -> Self {
        Self { floor_db: -60.0, gain: 1.0, hold: 0, open: false, prev_gain: 1.0 }
    }

    /// Advance the gate state machine and return this frame's gate gain (0..1).
    pub(crate) fn step(&mut self, cfg: &InputConfig, vad: f32, level_db: f32) -> f32 {
        let (open_l, close_l) = if cfg.gate_auto {
            // Track the noise floor on clearly non-speech frames: fall fast, rise slow.
            if vad < VAD_NONSPEECH {
                let c = if level_db < self.floor_db { FLOOR_FALL } else { FLOOR_RISE };
                self.floor_db += (level_db - self.floor_db) * c;
                self.floor_db = self.floor_db.clamp(-90.0, 0.0);
            }
            (self.floor_db + LEVEL_OPEN_MARGIN, self.floor_db + LEVEL_CLOSE_MARGIN)
        } else {
            // Manual: the slider sets the open threshold directly (meter scale -> dBFS).
            let db = cfg.gate_threshold * 60.0 - 60.0;
            (db, db - 6.0)
        };

        // Speech decision with level + VAD hysteresis (stricter to open, looser to stay).
        let speaking = if self.open {
            vad > VAD_CLOSE && level_db > close_l
        } else {
            vad > VAD_OPEN && level_db > open_l
        };
        if speaking {
            self.open = true;
            self.hold = HOLD_FRAMES;
        } else if self.hold > 0 {
            self.hold -= 1;
        } else {
            self.open = false;
        }

        // Ramp the gain toward the target (fast attack, slow release) for click-free gaps.
        let target = if self.open { 1.0 } else { 0.0 };
        if target > self.gain {
            self.gain = (self.gain + ATTACK_STEP).min(1.0);
        } else if target < self.gain {
            self.gain = (self.gain - RELEASE_STEP).max(0.0);
        }
        self.gain
    }
}

impl AudioFilter for Gate {
    /// Decide this frame's gate gain (`step`, unchanged) and apply it to `samples` with a
    /// click-free intra-frame ramp from last frame's gain — exactly the loop that used to
    /// live in `InputProcessor::process`, now self-contained here so `Gate` is a complete
    /// chain link. `level_db` is recomputed from `samples` itself (the same `rms(&cleaned)`
    /// value `InputProcessor` used to compute once and reuse — recomputing it here is the
    /// same pure function over the same unmutated data, so it's bit-for-bit identical).
    fn process_block(&mut self, samples: &mut [f32; FRAME], cfg: &InputConfig, vad: f32) {
        let level_db = lin_to_db(rms(samples));
        let gain = if cfg.gate { self.step(cfg, vad, level_db) } else { 1.0 };
        let g0 = self.prev_gain;
        for (i, c) in samples.iter_mut().enumerate() {
            let t = i as f32 / FRAME as f32;
            *c *= g0 + (gain - g0) * t;
        }
        self.prev_gain = gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Manual-threshold config (no floor tracking): threshold 0.5 -> open at -30 dBFS,
    // close at -36 dBFS (the 6 dB hysteresis).
    fn manual(threshold: f32) -> InputConfig {
        InputConfig {
            noise_suppression: false,
            echo_cancellation: false,
            auto_gain: false,
            gate: true,
            gate_auto: false,
            gate_threshold: threshold,
            advanced_vad: false,
        }
    }

    fn auto() -> InputConfig {
        InputConfig { gate_auto: true, ..manual(0.5) }
    }

    const SPEECH_VAD: f32 = 0.9; // well above VAD_OPEN
    const SPEECH_DB: f32 = -10.0; // well above the -30 dBFS open level
    const SILENCE_VAD: f32 = 0.0;
    const SILENCE_DB: f32 = -80.0;

    #[test]
    fn new_starts_closed_at_full_gain() {
        let g = Gate::new();
        assert!(!g.open);
        assert_eq!(g.gain, 1.0);
        assert_eq!(g.hold, 0);
        assert_eq!(g.floor_db, -60.0);
    }

    #[test]
    fn opens_on_speech_and_attack_ramps_to_one() {
        let cfg = manual(0.5);
        let mut g = Gate::new();
        // Drive the gate closed first so the attack ramp is observable from 0.
        for _ in 0..30 {
            g.step(&cfg, SILENCE_VAD, SILENCE_DB);
        }
        assert!(!g.open);
        assert_eq!(g.gain, 0.0);

        // Speech opens immediately; gain ramps +0.5/frame: 0.0 -> 0.5 -> 1.0.
        let a = g.step(&cfg, SPEECH_VAD, SPEECH_DB);
        assert!(g.open, "speech opens the gate");
        assert_eq!(a, 0.5);
        assert_eq!(g.hold, HOLD_FRAMES); // hold refreshed on speech
        let b = g.step(&cfg, SPEECH_VAD, SPEECH_DB);
        assert_eq!(b, 1.0);
        let c = g.step(&cfg, SPEECH_VAD, SPEECH_DB);
        assert_eq!(c, 1.0); // clamped at 1.0
    }

    #[test]
    fn attack_ramp_is_monotonically_non_decreasing() {
        let cfg = manual(0.5);
        let mut g = Gate::new();
        for _ in 0..30 {
            g.step(&cfg, SILENCE_VAD, SILENCE_DB);
        }
        let mut gains = Vec::new();
        for _ in 0..5 {
            gains.push(g.step(&cfg, SPEECH_VAD, SPEECH_DB));
        }
        for w in gains.windows(2) {
            assert!(w[1] >= w[0], "attack must not dip: {gains:?}");
        }
        assert_eq!(*gains.last().unwrap(), 1.0);
    }

    #[test]
    fn holds_open_through_hold_window_then_closes_and_releases() {
        let cfg = manual(0.5);
        let mut g = Gate::new();
        // Open it (gain already 1.0 from new, stays 1.0).
        for _ in 0..3 {
            g.step(&cfg, SPEECH_VAD, SPEECH_DB);
        }
        assert!(g.open);
        assert_eq!(g.gain, 1.0);
        assert_eq!(g.hold, HOLD_FRAMES);

        // During the hold window the gate stays open at full gain (target == 1.0).
        for _ in 0..HOLD_FRAMES {
            g.step(&cfg, SILENCE_VAD, SILENCE_DB);
        }
        assert!(g.open, "held open across the hold window");
        assert_eq!(g.gain, 1.0);
        assert_eq!(g.hold, 0);

        // The next silent frame (hold exhausted) closes the gate.
        g.step(&cfg, SILENCE_VAD, SILENCE_DB);
        assert!(!g.open);

        // Release ramp: monotonically non-increasing down to 0.0.
        let mut gains = vec![g.gain];
        for _ in 0..40 {
            gains.push(g.step(&cfg, SILENCE_VAD, SILENCE_DB));
        }
        for w in gains.windows(2) {
            assert!(w[1] <= w[0], "release must not rise: {gains:?}");
        }
        assert_eq!(*gains.last().unwrap(), 0.0);
    }

    #[test]
    fn vad_below_open_threshold_does_not_open_from_closed() {
        let cfg = manual(0.5);
        let mut g = Gate::new();
        // VAD 0.5 sits between VAD_CLOSE (0.35) and VAD_OPEN (0.65): not enough to open.
        for _ in 0..5 {
            g.step(&cfg, 0.5, SPEECH_DB);
        }
        assert!(!g.open);
    }

    #[test]
    fn low_level_does_not_open_even_with_strong_vad() {
        let cfg = manual(0.5); // open level -30 dBFS
        let mut g = Gate::new();
        // Strong VAD but level below the open threshold -> stays closed.
        for _ in 0..5 {
            g.step(&cfg, SPEECH_VAD, -50.0);
        }
        assert!(!g.open);
    }

    #[test]
    fn hysteresis_mid_vad_keeps_open_gate_open() {
        let cfg = manual(0.5);
        let mut g = Gate::new();
        g.step(&cfg, SPEECH_VAD, SPEECH_DB); // open
        assert!(g.open);
        // VAD 0.5 is above VAD_CLOSE (0.35) so an already-open gate keeps passing and
        // refreshes the hold, even though 0.5 would not have opened a closed gate.
        g.step(&cfg, 0.5, SPEECH_DB);
        assert!(g.open);
        assert_eq!(g.hold, HOLD_FRAMES);
    }

    #[test]
    fn auto_floor_falls_fast_on_quiet_nonspeech_frame() {
        let cfg = auto();
        let mut g = Gate::new();
        assert_eq!(g.floor_db, -60.0);
        // Non-speech (vad < 0.3) below the floor: floor += (level-floor)*FLOOR_FALL.
        g.step(&cfg, 0.0, -80.0);
        let expected = -60.0 + (-80.0 - -60.0) * FLOOR_FALL; // -61.2
        assert!((g.floor_db - expected).abs() < 1e-3, "floor_db = {}", g.floor_db);
    }

    #[test]
    fn auto_floor_not_updated_on_speech_frames() {
        let cfg = auto();
        let mut g = Gate::new();
        // vad >= VAD_NONSPEECH (0.3): the floor tracker must not move.
        g.step(&cfg, 0.5, -80.0);
        assert_eq!(g.floor_db, -60.0);
    }

    #[test]
    fn auto_floor_is_clamped_to_minus_90() {
        let cfg = auto();
        let mut g = Gate::new();
        for _ in 0..500 {
            g.step(&cfg, 0.0, -200.0);
        }
        assert_eq!(g.floor_db, -90.0);
    }

    // --- AudioFilter::process_block (DRAGON-124: the relocated ramp-apply loop) ---

    #[test]
    fn process_block_ramps_gain_into_samples_like_the_old_inline_loop_did() {
        // Reproduce, by hand, exactly what `InputProcessor::process` used to do inline
        // (decide the gate gain via `step`, then ramp it across the frame from the last
        // frame's gain) and check `process_block` matches it sample-for-sample.
        let cfg = manual(0.5);
        let mut reference = Gate::new();
        let mut viaseam = Gate::new();
        let frame = [0.2f32; FRAME];

        for level_db in [SILENCE_DB, SPEECH_DB, SPEECH_DB, SILENCE_DB] {
            let vad = if level_db == SPEECH_DB { SPEECH_VAD } else { SILENCE_VAD };

            // Reference: the old inline shape (level_db computed by the caller from the
            // unmutated frame, gain decided, then a manual click-free ramp).
            let gain = reference.step(&cfg, vad, level_db);
            let mut expected = frame;
            let g0 = reference.prev_gain;
            for (i, c) in expected.iter_mut().enumerate() {
                let t = i as f32 / FRAME as f32;
                *c *= g0 + (gain - g0) * t;
            }
            reference.prev_gain = gain;

            // Seam: process_block recomputes level_db from the samples itself.
            let mut actual = frame;
            viaseam.process_block(&mut actual, &cfg, vad);

            assert_eq!(actual, expected, "process_block diverged at level_db={level_db}");
        }
    }

    #[test]
    fn process_block_bypasses_gate_entirely_when_disabled() {
        // cfg.gate == false: process_block must behave like the old `else { 1.0 }` branch
        // (full gain, no gate decision at all) regardless of level/vad.
        let cfg = InputConfig { gate: false, ..manual(0.5) };
        let mut g = Gate::new();
        let mut samples = [0.3f32; FRAME];
        let before = samples;
        g.process_block(&mut samples, &cfg, SILENCE_VAD);
        assert_eq!(samples, before, "gain must stay 1.0 (no-op) when the gate is off");
    }
}
