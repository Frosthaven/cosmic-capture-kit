//! VAD-driven automatic gain, tuned to sound NATURAL rather than levelled. It normalises your
//! average speaking level toward the meters' ideal band over ~a second, but — unlike a fast
//! leveller that pushes every syllable to the same spot — it lets the syllable-to-syllable
//! dynamics through, so the voice keeps its life (a healthy loudness range) instead of sounding
//! compressed/"in your face". A smooth safety stage rides on top: a frame never crosses the red
//! line (RMS) and never clips (sample peak), so loud moments are reined in cleanly at the very top.
//!
//! Driven by the chain's voice-activity probability so it only tracks actual speech (most reliable
//! with Advanced Voice Activity on; otherwise the RNNoise probability or a noise-floor gate stands
//! in). See agc.rs's history (DRAGON-79/81/83) and audio-levels.md for the metering rationale.

use crate::audio::config::{InputConfig, FRAME};
use crate::audio::meter::{db_to_lin, lin_to_db, rms};

use super::AudioFilter;

// All dBFS, on the meters' scale (see audio-levels.md). Bands: "Normal" body -24..-12, "Ideal
// Peaks" -12..-6, red above -6. We aim the AVERAGE at the top of the "Normal" band so the louder
// syllables rise into "Ideal Peaks" using the headroom above it — instead of parking the average
// in "Ideal" (centre of green), which leaves no room and makes every loud syllable hit the limiter
// (audible clamping) and the meter go red. Lower target = peaks breathe; loudness stays close.
const TARGET_DB: f32 = -14.0; // where the AVERAGE speaking level sits (top of the "Normal" band)
const NOISE_FLOOR_DB: f32 = -55.0; // below this it isn't speech worth tracking/boosting
const VAD_SPEECH: f32 = 0.5; // track the level only when we're this sure it's voice
const MAX_GAIN_DB: f32 = 36.0;
const MIN_GAIN_DB: f32 = -18.0; // allow pulling a hot mic DOWN into the band, not just up

// Level follower. A SLOW follower is the whole point: a near-steady gain preserves dynamics
// (output = input x G), where a fast one flattens them. But a brief FAST lock-on at each speech
// onset keeps DRAGON-81's win — the first word still lands — before it settles into slow tracking.
const LOCK_FRAMES: u32 = 16; // ~160 ms of fast lock-on at the start of speech
const GAP_FRAMES: u32 = 25; // ~250 ms of silence ends an utterance, so the next onset re-locks fast
const LOCK_COEF: f32 = 0.30; // follower coefficient while locking on (fast)
const TRACK_COEF: f32 = 0.008; // follower coefficient after lock (~1 s) — lets the dynamics through
const GAIN_COEF: f32 = 0.25; // makeup-gain smoothing toward (target - level)

// Safety stage after the makeup gain — two guarantees, both applied SMOOTHLY so they never click.
// (An earlier per-frame trim scaled each 10 ms frame independently, stepping the gain at every
// frame boundary — audible as static on loud words.) Never red: the frame's RMS is held under
// CEILING_DB (~1 dB under the -6 dBFS red), the reduction RAMPED from the previous frame so a
// sustained loud passage is pinned down continuously. Never clip: a per-sample peak limiter (fast
// attack, slow release) keeps every sample under PEAK_CEIL (~ -1 dBTP headroom) without stepping.
// Both reductions carry frame-to-frame, so the whole gain chain stays continuous.
const CEILING_DB: f32 = -7.0; // RMS ceiling — never red
const PEAK_CEIL_DB: f32 = -1.5; // sample-peak ceiling — never clip
const LIM_ATTACK: f32 = 0.20; // peak-limiter attack per sample (~0.3 ms — catches peaks, no click)
const LIM_RELEASE: f32 = 0.0002; // peak-limiter release per sample (~100 ms — smooth recovery)

/// Stateful auto-gain. One per stream; feed each cleaned 480-sample frame plus its VAD.
pub(crate) struct AutoGain {
    /// Slow-tracked speaking level — the thing we normalise (NOT a per-syllable peak).
    level_db: f32,
    /// Applied makeup gain.
    gain_db: f32,
    /// Consecutive voiced frames since the last real gap — drives the fast lock-on.
    voiced: u32,
    /// Consecutive non-voiced frames — once past GAP_FRAMES, the next onset re-locks fast.
    silent: u32,
    /// Last frame's makeup gain, for a click-free intra-frame ramp.
    prev_makeup: f32,
    /// Last frame's RMS-ceiling reduction (linear <= 1), ramped into each frame so the never-red
    /// trim doesn't step frame-to-frame (that stepping was audible as static).
    prev_rms_gr: f32,
    /// Per-sample peak-limiter gain reduction (linear <= 1); fast attack, slow release.
    peak_gr: f32,
}

impl AutoGain {
    pub(crate) fn new() -> Self {
        // Start the follower BELOW any real speech so the first word snaps it up to the true level
        // at once (the fast lock-on), and `silent` primed so that very first onset locks fast.
        Self {
            level_db: NOISE_FLOOR_DB,
            gain_db: 0.0,
            voiced: 0,
            silent: GAP_FRAMES,
            prev_makeup: 1.0,
            prev_rms_gr: 1.0,
            peak_gr: 1.0,
        }
    }

    /// Gain `frame` in place toward the ideal band while keeping its dynamics. `vad` (0..1) gates
    /// level tracking on speech. Guarantees the frame never clips (held under the peak ceiling).
    pub(crate) fn process(&mut self, frame: &mut [f32; FRAME], vad: f32) {
        let lvl_db = lin_to_db(rms(frame));
        if vad >= VAD_SPEECH && lvl_db > NOISE_FLOOR_DB {
            self.silent = 0;
            self.voiced = self.voiced.saturating_add(1);
            // Fast lock-on at the very start of speech (so the first word lands), then track slowly
            // so the gain stops chasing individual syllables and the natural dynamics survive.
            let coef = if self.voiced <= LOCK_FRAMES { LOCK_COEF } else { TRACK_COEF };
            self.level_db += (lvl_db - self.level_db) * coef;
            let want = (TARGET_DB - self.level_db).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
            self.gain_db += (want - self.gain_db) * GAIN_COEF;
        } else {
            self.silent = self.silent.saturating_add(1);
            if self.silent >= GAP_FRAMES {
                self.voiced = 0; // a real pause: re-lock fast next time (the level itself holds)
            }
        }

        // Apply the makeup gain, ramping across the frame from last frame's value (click-free).
        let makeup = db_to_lin(self.gain_db);
        let m0 = self.prev_makeup;
        for (i, s) in frame.iter_mut().enumerate() {
            let t = i as f32 / FRAME as f32;
            *s *= m0 + (makeup - m0) * t;
        }
        self.prev_makeup = makeup;

        // Never red: pull the frame's RMS under the ceiling, the reduction RAMPED from last frame's
        // so a sustained loud passage is held down continuously instead of stepping every frame
        // (that step was the static). Sustained loud lands on the ceiling; a loud onset eases in.
        let lvl = rms(frame);
        let rms_gr = if lvl > 0.0 { (db_to_lin(CEILING_DB) / lvl).min(1.0) } else { 1.0 };
        let r0 = self.prev_rms_gr;
        for (i, s) in frame.iter_mut().enumerate() {
            let t = i as f32 / FRAME as f32;
            *s *= r0 + (rms_gr - r0) * t;
        }
        self.prev_rms_gr = rms_gr;

        // Never clip: a smooth per-sample peak limiter — fast attack catches a peak within a
        // fraction of a ms, slow release eases back — holding every sample under the ceiling
        // without the clicks a per-frame trim makes.
        let ceil = db_to_lin(PEAK_CEIL_DB);
        for s in frame.iter_mut() {
            let target = (ceil / s.abs().max(1e-9)).min(1.0);
            self.peak_gr += if target < self.peak_gr {
                (target - self.peak_gr) * LIM_ATTACK
            } else {
                (1.0 - self.peak_gr) * LIM_RELEASE
            };
            *s = (*s * self.peak_gr).clamp(-1.0, 1.0);
        }
    }
}

impl AudioFilter for AutoGain {
    /// Thin adapter onto the unchanged `process` method above — AGC needs only `vad`, so
    /// `cfg` is accepted (to match [`Gate`](super::gate::Gate)'s real call shape, the
    /// other linear stage) and ignored.
    fn process_block(&mut self, samples: &mut [f32; FRAME], _cfg: &InputConfig, vad: f32) {
        self.process(samples, vad);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::meter::level_to_meter;

    // A steady tone frame at a given dBFS (RMS), as the chain would hand us post-cleanup.
    fn tone(db: f32) -> [f32; FRAME] {
        let amp = db_to_lin(db) * std::f32::consts::SQRT_2; // sine RMS = amp/√2
        let mut f = [0.0; FRAME];
        for (n, s) in f.iter_mut().enumerate() {
            *s = amp * (2.0 * std::f32::consts::PI * 220.0 * n as f32 / 48_000.0).sin();
        }
        f
    }

    // Run a constant-level voice through the gain for `frames` and return the final meter level.
    fn settle(input_db: f32, frames: usize) -> f32 {
        let mut ag = AutoGain::new();
        let mut last = 0.0;
        for _ in 0..frames {
            let mut f = tone(input_db);
            ag.process(&mut f, 1.0);
            last = level_to_meter(rms(&f));
        }
        last
    }

    #[test]
    fn quiet_voice_normalises_to_the_body_band() {
        // -28 dBFS mic: the AVERAGE should normalise to the top of the "Normal" body band (~0.77),
        // leaving headroom above for peaks — not parked up in "Ideal Peaks" (0.80..0.90).
        let m = settle(-28.0, 200);
        assert!((0.73..=0.80).contains(&m), "settled meter = {m}");
    }

    #[test]
    fn first_short_word_is_boosted_to_the_body_band() {
        // A short first word (~300 ms = 30 frames) of a quiet mic should already reach the body
        // band — the fast lock-on lands it without waiting for the slow follower.
        let m = settle(-28.0, 30);
        assert!(m >= 0.72, "first short word only reached {m}, should already be at the body level");
    }

    #[test]
    fn hot_voice_is_pulled_down_into_band() {
        // A hot -3 dBFS mic must be brought DOWN to the body band, not left hot.
        let m = settle(-3.0, 200);
        assert!((0.73..=0.80).contains(&m), "hot mic settled at {m}, should sit in the body band");
    }

    #[test]
    fn sustained_loud_stays_under_red_and_never_clips() {
        // Settle quiet (gain rides high), then sustained loud: the smooth safety stage must hold it
        // BOTH under the red line (RMS) and under the clip ceiling (peak) once it's locked in.
        let mut ag = AutoGain::new();
        for _ in 0..200 {
            ag.process(&mut tone(-28.0), 1.0);
        }
        let mut loud = tone(-2.0);
        for _ in 0..8 {
            loud = tone(-2.0);
            ag.process(&mut loud, 1.0);
        }
        assert!(level_to_meter(rms(&loud)) < 0.90, "sustained loud crossed into the red");
        let peak = loud.iter().fold(0f32, |m, &s| m.max(s.abs()));
        assert!(peak <= db_to_lin(PEAK_CEIL_DB) * 1.05, "peak {peak} exceeded the clip ceiling");
        assert!(loud.iter().all(|s| s.abs() < 1.0), "a sample clipped to full scale");
    }

    #[test]
    fn dynamics_are_preserved_not_flattened() {
        // The point of the rework: after locking onto a level, a louder frame and a quieter frame
        // keep their difference at the output (a leveller would flatten them toward equal).
        let mut ag = AutoGain::new();
        for _ in 0..80 {
            ag.process(&mut tone(-30.0), 1.0); // lock onto a quiet voice (gain well below limiter)
        }
        let mut loud = tone(-30.0);
        let mut soft = tone(-42.0);
        ag.process(&mut loud, 1.0);
        ag.process(&mut soft, 1.0);
        let diff = lin_to_db(rms(&loud)) - lin_to_db(rms(&soft));
        // Input difference is 12 dB; a faithful (uncompressed) gain keeps most of it.
        assert!(diff >= 8.0, "output difference collapsed to {diff} dB — too compressed");
    }

    #[test]
    fn process_block_matches_process_exactly() {
        // The AudioFilter seam must be a pure pass-through onto `process` (DRAGON-124):
        // same output for the same input, cfg ignored.
        let cfg = InputConfig {
            noise_suppression: false,
            echo_cancellation: false,
            auto_gain: true,
            gate: false,
            gate_auto: true,
            gate_threshold: 0.5,
            advanced_vad: false,
        };
        let mut direct = AutoGain::new();
        let mut viaseam = AutoGain::new();
        for db in [-28.0, -28.0, -3.0, -28.0] {
            let mut a = tone(db);
            let mut b = tone(db);
            direct.process(&mut a, 1.0);
            AudioFilter::process_block(&mut viaseam, &mut b, &cfg, 1.0);
            assert_eq!(a, b);
        }
    }
}
