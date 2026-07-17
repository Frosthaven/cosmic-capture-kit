//! Sidechain ducking (DRAGON-128, the mixer epic's phase-4 filter): lower the SYSTEM
//! track while the user is speaking, so voiceover stays intelligible over desktop
//! audio. Per DRAGON-122's design invariant this is *a filter computing gain, not a
//! control event*: it lives in the system stream's filter stage BEFORE the mixer
//! (`record::pump` feeds it and applies it to each system chunk), exactly like the
//! mic's own cleanup chain is baked into the mic track — it never touches the
//! control lane, whose `TrackGain` events remain the user's reversible mutes.
//!
//! The sidechain is the MIC TAP's samples — the post-gate, post-AGC signal
//! (`clean_mic::spawn_tap_reader_thread`'s output). Reading post-gate is the
//! invariant's "so noise doesn't duck": a closed gate emits digital silence, so
//! background noise that never opens the gate can never duck the system track, and
//! the AGC in front of the detector means speech sits in a known loudness band and
//! one fixed threshold is reliable.
//!
//! The envelope is deliberately gentle (slow attack, generous hold, slower release)
//! — ducking is a mixing convenience, not a gate; pumping would be worse than no
//! ducking at all. All smoothing is per-frame linear slewing at 48 kHz, so gain
//! changes are click-free by construction (the same reasoning as the gate's
//! intra-frame ramp and `mixer::control::GainRamp`'s 10 ms ramps).

/// Sidechain activity threshold, LINEAR RMS (≈ −45 dBFS). Post-gate/post-AGC speech
/// sits far above this (the AGC targets the meters' green band); a closed gate emits
/// exact silence, far below it. The gap is wide on both sides, so the exact value is
/// uncritical.
const SIDECHAIN_OPEN_RMS: f32 = 0.005_623; // 10^(-45/20)

/// Ducked gain for the system track while speech is active: −12 dB, the customary
/// voiceover duck — desktop audio stays clearly audible underneath, speech on top.
const DUCK_GAIN: f32 = 0.25;

/// Seconds the envelope takes to slew from unity down to [`DUCK_GAIN`].
const ATTACK_SECS: f32 = 0.10;

/// Sidechain frames (10 ms each) the duck holds after the last active frame, so
/// natural inter-word pauses don't flutter the system track.
const HOLD_FRAMES: u32 = 30; // 300 ms

/// Seconds the envelope takes to slew from [`DUCK_GAIN`] back up to unity.
const RELEASE_SECS: f32 = 0.50;

/// Failsafe (system-track frames at 48 kHz): if NO sidechain frame has been fed for
/// this long — the mic chain died mid-session; its taps just stop — the duck releases
/// rather than freezing at whatever the last decision was. A healthy mic feeds a
/// frame every 10 ms, so this never engages in normal operation.
const SIDECHAIN_STARVED_FRAMES: u64 = 48_000; // 1 s

/// Per-frame gain step while attacking (down) / releasing (up), covering the full
/// `1.0 → DUCK_GAIN` span in the configured seconds at 48 kHz.
const ATTACK_STEP: f32 = (1.0 - DUCK_GAIN) / (ATTACK_SECS * 48_000.0);
const RELEASE_STEP: f32 = (1.0 - DUCK_GAIN) / (RELEASE_SECS * 48_000.0);

/// The stateful ducker: [`feed_sidechain`](Self::feed_sidechain) with each mic tap
/// block (10 ms, mono), [`process`](Self::process) each system chunk (interleaved,
/// any length) — both on the pump's control thread, so no synchronization is needed.
pub(crate) struct Ducker {
    /// Sidechain frames left before the duck may release (reset by every active
    /// frame, decremented by inactive ones) — the hold stage.
    hold_left: u32,
    /// Current envelope gain (1.0 = untouched), advanced one step per SYSTEM frame.
    gain: f32,
    /// System frames processed since the last sidechain feed — the starvation
    /// failsafe's clock.
    frames_since_sidechain: u64,
}

impl Ducker {
    pub(crate) fn new() -> Self {
        Self { hold_left: 0, gain: 1.0, frames_since_sidechain: 0 }
    }

    /// Feed one mic tap block (post-gate mono PCM). `live` is whether the mic
    /// channel is currently toggled ON — a muted mic must not duck (its speech is
    /// cut from the recording at finalize, so ducking to it would carve audible
    /// holes in the system track for no on-file reason).
    pub(crate) fn feed_sidechain(&mut self, mic: &[f32], live: bool) {
        self.frames_since_sidechain = 0;
        let active = live && rms(mic) > SIDECHAIN_OPEN_RMS;
        if active {
            self.hold_left = HOLD_FRAMES;
        } else {
            self.hold_left = self.hold_left.saturating_sub(1);
        }
    }

    /// Apply the envelope to one system chunk (interleaved, `channels` samples per
    /// frame), advancing the gain one slew step per frame — chunk boundaries are
    /// invisible to the envelope, so arbitrary chunk sizes stay click-free.
    pub(crate) fn process(&mut self, samples: &mut [f32], channels: usize) {
        let ch = channels.max(1);
        let target = if self.ducking() { DUCK_GAIN } else { 1.0 };
        for frame in samples.chunks_mut(ch) {
            self.frames_since_sidechain = self.frames_since_sidechain.saturating_add(1);
            if self.gain > target {
                self.gain = (self.gain - ATTACK_STEP).max(target);
            } else if self.gain < target {
                self.gain = (self.gain + RELEASE_STEP).min(target);
            }
            if self.gain < 1.0 {
                for s in frame {
                    *s *= self.gain;
                }
            }
        }
    }

    /// Whether the envelope's target is currently the ducked gain: inside the hold
    /// window AND the sidechain is actually alive (the starvation failsafe).
    fn ducking(&self) -> bool {
        self.hold_left > 0 && self.frames_since_sidechain < SIDECHAIN_STARVED_FRAMES
    }
}

/// Plain linear RMS over a block (not the meters' dBFS mapping — the threshold above
/// is already expressed linearly).
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FRAME;

    /// A loud (speech-level) sidechain frame and a silent one.
    fn loud() -> Vec<f32> {
        vec![0.1; FRAME]
    }
    fn silent() -> Vec<f32> {
        vec![0.0; FRAME]
    }

    /// Run `n` sidechain frames and process matching 10 ms stereo system blocks,
    /// returning the final envelope gain.
    fn run(d: &mut Ducker, sidechain: &[f32], live: bool, n: usize) -> f32 {
        for _ in 0..n {
            d.feed_sidechain(sidechain, live);
            let mut sys = vec![1.0f32; FRAME * 2];
            d.process(&mut sys, 2);
        }
        d.gain
    }

    #[test]
    fn silence_never_ducks() {
        let mut d = Ducker::new();
        let g = run(&mut d, &silent(), true, 100);
        assert_eq!(g, 1.0);
    }

    #[test]
    fn speech_ducks_to_the_configured_gain_within_the_attack_window() {
        let mut d = Ducker::new();
        // 100 ms of speech = the full attack span.
        let g = run(&mut d, &loud(), true, 10);
        assert!((g - DUCK_GAIN).abs() < 1e-3, "gain {g} should reach {DUCK_GAIN}");
    }

    #[test]
    fn muted_mic_never_ducks() {
        let mut d = Ducker::new();
        let g = run(&mut d, &loud(), false, 50);
        assert_eq!(g, 1.0);
    }

    #[test]
    fn short_pauses_hold_the_duck_then_release_fully() {
        let mut d = Ducker::new();
        run(&mut d, &loud(), true, 20);
        // A pause shorter than the hold keeps the duck fully engaged.
        let g = run(&mut d, &silent(), true, (HOLD_FRAMES as usize) - 5);
        assert!((g - DUCK_GAIN).abs() < 1e-3, "gain {g} should still be ducked in the hold");
        // Past the hold + the full release span, the envelope is back at unity.
        let frames = HOLD_FRAMES as usize + (RELEASE_SECS * 100.0) as usize + 5;
        let g = run(&mut d, &silent(), true, frames);
        assert_eq!(g, 1.0);
    }

    #[test]
    fn envelope_scales_both_channels_and_is_click_free() {
        let mut d = Ducker::new();
        d.feed_sidechain(&loud(), true);
        let mut sys = vec![1.0f32; FRAME * 2];
        d.process(&mut sys, 2);
        // Both channels of each frame carry the SAME gain, and consecutive frames
        // never step more than the attack slew.
        let mut prev = 1.0f32;
        for f in sys.chunks(2) {
            assert_eq!(f[0], f[1]);
            assert!((prev - f[0]).abs() <= ATTACK_STEP + 1e-6);
            prev = f[0];
        }
        assert!(prev < 1.0, "the envelope should have started attacking");
    }

    #[test]
    fn a_dead_sidechain_releases_instead_of_freezing_ducked() {
        let mut d = Ducker::new();
        run(&mut d, &loud(), true, 20);
        assert!((d.gain - DUCK_GAIN).abs() < 1e-3);
        // The mic chain dies: no more sidechain feeds, but system audio keeps
        // flowing. Past the starvation window the duck must release on its own.
        let mut sys = vec![1.0f32; FRAME * 2];
        for _ in 0..((SIDECHAIN_STARVED_FRAMES / FRAME as u64) as usize
            + (RELEASE_SECS * 100.0) as usize
            + 5)
        {
            sys.fill(1.0);
            d.process(&mut sys, 2);
        }
        assert_eq!(d.gain, 1.0);
    }
}
