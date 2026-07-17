//! Input-cleanup configuration (the Audio settings DTO) and the chain's processing-latency
//! model — kept together because the latency is derived purely from which stages the config
//! enables.

/// 10 ms frame at 48 kHz (480 samples): the rate nnnoiseless and the WebRTC processor
/// both use, and one waveform column per frame (100/sec).
pub const FRAME: usize = nnnoiseless::DenoiseState::FRAME_SIZE;

/// Which cleanup stages are active. Mirrors the Audio settings toggles.
#[derive(Clone, Copy)]
pub struct InputConfig {
    pub noise_suppression: bool,
    pub echo_cancellation: bool,
    pub auto_gain: bool,
    /// Voice gate ("Input Sensitivity") enabled.
    pub gate: bool,
    /// Gate threshold mode: true = auto (track the noise floor), false = manual slider.
    pub gate_auto: bool,
    /// Manual gate threshold, 0..1 on the meter dBFS scale (`(dbfs+60)/60`).
    pub gate_threshold: f32,
    /// Use the earshot neural VAD ("Advanced Voice Activity") for the gate's speech
    /// decision instead of the RNNoise probability.
    pub advanced_vad: bool,
}

/// The fixed processing latency (ms) the cleanup chain adds to the mic vs the raw input,
/// so the recording A/V sync can compensate it (the cleaned mic arrives this much later).
/// Deterministic per enabled stage: RNNoise overlap-add 10 + WebRTC NS overlap-add 6
/// (both gated by `noise_suppression`) + AEC3 block framing 4 (`echo_cancellation`) + the
/// 3-band split/synthesis filterbank 0.5 (present whenever NS or AEC is on). AGC2, the
/// VAD, the gate, and the high-pass add no fixed delay. (NS+AEC=20.5, NS=16.5, AEC=4.5,
/// AGC-only/off=0.)
pub fn processing_latency_ms(cfg: &InputConfig) -> f64 {
    let mut l = 0.0;
    if cfg.noise_suppression {
        l += 10.0 + 6.0; // RNNoise + WebRTC NS overlap-add
    }
    if cfg.echo_cancellation {
        l += 4.0; // AEC3 block framing
    }
    if cfg.noise_suppression || cfg.echo_cancellation {
        l += 0.5; // 3-band split/synthesis filterbank
    }
    l
}

/// The device ffmpeg captures the microphone from — a PulseAudio source name on Linux,
/// an avfoundation device NAME on macOS (the id from
/// [`crate::audio::devices::list_input_sources`], used as `-f avfoundation -i ":<name>"`;
/// avfoundation matches non-numeric input strings by exact name).
/// `None` = the system default source (auto). Set from the persisted `mic_device`
/// setting; read fresh at every ffmpeg/meter spawn so changing the device takes effect on
/// the next recording or meter without threading it through the whole recording pipeline.
static MIC_SOURCE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Set the mic capture source (empty/blank = system default / auto).
pub fn set_mic_source(name: &str) {
    if let Ok(mut g) = MIC_SOURCE.lock() {
        *g = (!name.trim().is_empty()).then(|| name.trim().to_string());
    }
}

/// The mic capture source for ffmpeg `-i` (a PulseAudio source name on Linux; an
/// avfoundation device name on macOS, where the literal `default` is avfoundation's
/// own default-device keyword). Defaults to `default` — the system's configured
/// default source.
pub fn mic_source() -> String {
    MIC_SOURCE
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| "default".to_string())
}

/// The mic cleanup config + speaker (for echo's far-end) used by the next recording.
/// Mirrors `MIC_SOURCE`: the app pushes the current audio settings, read fresh at spawn.
static RECORDING_MIC_CONFIG: std::sync::Mutex<Option<(InputConfig, String)>> =
    std::sync::Mutex::new(None);

/// Set the cleaned-mic recording config (InputProcessor settings + speaker) for the next
/// recording. Pushed by the app whenever the audio settings change.
pub fn set_recording_mic_config(cfg: InputConfig, speaker: &str) {
    if let Ok(mut g) = RECORDING_MIC_CONFIG.lock() {
        *g = Some((cfg, speaker.to_string()));
    }
}

/// Whether the next recording ducks the system track under mic speech (DRAGON-128).
/// Mirrors `RECORDING_MIC_CONFIG`'s idiom (pushed by the app at recording start, read
/// fresh where the pump is configured) — an atomic since it's a lone flag.
static RECORDING_DUCK_SYSTEM: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Set whether the next recording ducks the system track while the mic hears speech.
pub fn set_recording_duck_system(on: bool) {
    RECORDING_DUCK_SYSTEM.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the next recording ducks the system track. Falls back to off if the app
/// hasn't pushed settings yet (headless/diagnostic contexts — mirrors
/// `recording_mic_config`'s all-off passthrough fallback; the persisted setting
/// itself defaults ON and is pushed at every recording start).
pub fn recording_duck_system() -> bool {
    RECORDING_DUCK_SYSTEM.load(std::sync::atomic::Ordering::Relaxed)
}

/// The cleaned-mic config for the next recording. Defaults to an all-off passthrough (a
/// mono copy of the raw mic) if the app hasn't pushed settings yet.
pub fn recording_mic_config() -> (InputConfig, String) {
    RECORDING_MIC_CONFIG.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| {
        (
            InputConfig {
                noise_suppression: false,
                echo_cancellation: false,
                auto_gain: false,
                gate: false,
                gate_auto: true,
                gate_threshold: 0.5,
                advanced_vad: false,
            },
            String::new(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn cfg(noise_suppression: bool, echo_cancellation: bool, auto_gain: bool) -> InputConfig {
        InputConfig {
            noise_suppression,
            echo_cancellation,
            auto_gain,
            gate: false,
            gate_auto: true,
            gate_threshold: 0.5,
            advanced_vad: false,
        }
    }

    #[rstest]
    #[case(false, false, false, 0.0)] // all off -> no added delay
    #[case(true, false, false, 16.5)] // RNNoise 10 + WebRTC NS 6 + filterbank 0.5
    #[case(false, true, false, 4.5)] // AEC3 4 + filterbank 0.5
    #[case(true, true, false, 20.5)] // NS + AEC together
    #[case(false, false, true, 0.0)] // AGC adds no fixed latency
    fn latency_model(
        #[case] ns: bool,
        #[case] aec: bool,
        #[case] agc: bool,
        #[case] want_ms: f64,
    ) {
        assert_eq!(processing_latency_ms(&cfg(ns, aec, agc)), want_ms);
    }
}
