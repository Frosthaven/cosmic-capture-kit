//! Audio settings page section builder.

use super::super::*;
use super::super::row::{gated_row, num_input, toggle, Item, SectionSpec, Severity};
use super::super::deps::DepId;
use super::super::mic_test::{provided_by, provided_by_earshot, provided_by_sonora, sensitivity_control};

/// A plain helper description line stacked above a "Provided by …" credit row, for the
/// rows that both describe what they do and credit the package behind them.
fn desc_with_credit(text: &'static str, credit: Element<'static, Msg>) -> Element<'static, Msg> {
    widget::column(vec![widget::text::caption(text).into(), credit])
        .spacing(2.0)
        .into()
}

impl crate::app::App {
    /// Audio settings page: device selection (output + input), processing
    /// (noise suppression, echo cancellation), the mic test, and A/V sync.
    pub(in crate::app::settings) fn audio_sections(&self) -> Vec<SectionSpec<'_>> {
        let d = crate::state::defaults();
        let mut secs: Vec<SectionSpec<'_>> = Vec::new();

        // Surface the pactl note only when there's a problem; the Health page lists it
        // regardless. (The device pickers below are gated when it's missing.)
        if let Some(note) = self.dep(DepId::Pactl).note_if_issue() {
            secs.push(SectionSpec { title: "Devices", items: vec![note] });
        }

        // Output: the speaker sink (also the echo-cancellation reference). The picker
        // is meaningless without pactl (nothing to enumerate), so gate it then.
        // macOS has NO Output section at all (DRAGON-132): the setting exists to pick
        // which sink's monitor feeds the AEC far-end, and macOS has no per-device
        // loopback — the ScreenCaptureKit system-audio mix serves that role, and
        // playback follows the system default output, so a picker would do nothing.
        #[cfg(not(target_os = "macos"))]
        {
            let output_device = if self.dep(DepId::Pactl).is_present() {
                Item::new(
                    "Output device",
                    "",
                    widget::dropdown(
                        &self.speaker_device_labels,
                        Some(self.speaker_device_index()),
                        |a0| Msg::Settings(SettingsMsg::SetSpeakerDevice(a0)),
                    ),
                )
                .reset_with(self.speaker_device_index(), 0usize, |a0| Msg::Settings(SettingsMsg::SetSpeakerDevice(a0)))
            } else {
                gated_row("Output device", "System (automatic)", Severity::Warn)
            };
            secs.push(SectionSpec { title: "Output", items: vec![output_device] });
        }

        // Input: the mic + its cleanup chain + the test. The device picker is gated
        // while devices can't be enumerated (Linux: no pactl; macOS: no ffmpeg — the
        // avfoundation inventory shells it); the rest still works on the default.
        let input_device = if self.dep(DepId::Pactl).is_present() {
            Item::new(
                "Input device",
                "",
                widget::dropdown(
                    &self.mic_device_labels,
                    Some(self.mic_device_index()),
                    |a0| Msg::Settings(SettingsMsg::SetMicDevice(a0)),
                ),
            )
            .reset_with(self.mic_device_index(), 0usize, |a0| Msg::Settings(SettingsMsg::SetMicDevice(a0)))
        } else {
            gated_row("Input device", "System (automatic)", Severity::Warn)
        };
        // Input: the device picker, then the cleanup chain listed in the ORDER IT
        // RUNS — echo removal, noise suppression, the voice gate (input sensitivity)
        // decided on your natural level, then auto-gain making up the level, then the
        // speech detector both of those rely on — and finally the live mic test that
        // hears the whole chain at once.
        let mut filters = vec![
            input_device,
            Item::new(
                "Echo cancellation",
                "",
                toggle(self.echo_cancellation, |a0| Msg::Settings(SettingsMsg::SetEchoCancellation(a0))),
            )
            .reset_with(self.echo_cancellation, d.echo_cancellation, |a0| Msg::Settings(SettingsMsg::SetEchoCancellation(a0)))
            .desc_el(desc_with_credit("Cancels speaker sound picked up by the mic.", provided_by_sonora())),
            Item::new(
                "Noise Suppression",
                "",
                toggle(self.noise_reduction, |a0| Msg::Settings(SettingsMsg::SetNoiseReduction(a0))),
            )
            .reset_with(self.noise_reduction, d.noise_reduction, |a0| Msg::Settings(SettingsMsg::SetNoiseReduction(a0)))
            .desc_el(desc_with_credit("Reduces background noise.", provided_by())),
            Item::new(
                "Automatic Input Sensitivity",
                "Controls how much sound your microphone records.",
                toggle(self.input_sensitivity_auto, |a0| Msg::Settings(SettingsMsg::SetInputSensitivityAuto(a0))),
            )
            .reset_with(self.input_sensitivity_auto, d.input_sensitivity_auto, |a0| Msg::Settings(SettingsMsg::SetInputSensitivityAuto(a0))),
        ];
        if !self.input_sensitivity_auto {
            filters.push(
                Item::new(
                    "Input Sensitivity Threshold",
                    "",
                    sensitivity_control(self.sens_level, self.input_sensitivity, theme_is_dark()),
                )
                .reset_with(self.input_sensitivity, d.input_sensitivity, |a0| Msg::Settings(SettingsMsg::SetInputSensitivity(a0))),
            );
        }
        filters.push(
            Item::new(
                "Automatic Gain Control",
                "Lifts quiet speech into the ideal range and holds it there, without crossing into too-loud.",
                toggle(self.auto_gain, |a0| Msg::Settings(SettingsMsg::SetAutoGain(a0))),
            )
            .reset_with(self.auto_gain, d.auto_gain, |a0| Msg::Settings(SettingsMsg::SetAutoGain(a0))),
        );
        filters.push(
            Item::new(
                "Advanced Voice Activity",
                "",
                toggle(self.advanced_vad, |a0| Msg::Settings(SettingsMsg::SetAdvancedVad(a0))),
            )
            .reset_with(self.advanced_vad, d.advanced_vad, |a0| Msg::Settings(SettingsMsg::SetAdvancedVad(a0)))
            .desc_el(desc_with_credit("Smarter detection of when you're speaking.", provided_by_earshot())),
        );
        // Push-to-talk is hidden while unavailable (no way to deliver the hold
        // hotkey unfocused on COSMIC yet — see `recording::PTT_AVAILABLE`); the
        // persisted setting is kept for when it returns.
        if crate::app::recording::PTT_AVAILABLE {
            filters.push(
                Item::new(
                    "Push to talk",
                    "Hold the mic button to talk instead of pressing to toggle.",
                    toggle(self.push_to_talk, |a0| Msg::Settings(SettingsMsg::SetPushToTalk(a0))),
                )
                .reset_with(self.push_to_talk, d.push_to_talk, |a0| Msg::Settings(SettingsMsg::SetPushToTalk(a0))),
            );
        }
        filters.push(Item::new(
            "Microphone test",
            "",
            widget::button::standard("Test Microphone").on_press(Msg::Settings(SettingsMsg::OpenMicTest)),
        ));
        secs.push(SectionSpec { title: "Input", items: filters });

        // Mixing: how the captured audio lines up with the video (and other players).
        // The manual offset only applies (and only shows) when auto-sync is off.
        let mut mixing = vec![
            Item::new(
                "Pause other media during preview editor",
                "When you capture content that contains audio, this will attempt to pause other \
                 audio while editing. Paused audio will resume after editing is completed.",
                toggle(self.mute_others_during_preview, |a0| Msg::Settings(SettingsMsg::SetMuteOthersDuringPreview(a0))),
            )
            .reset_with(
                self.mute_others_during_preview,
                d.mute_others_during_preview,
                |a0| Msg::Settings(SettingsMsg::SetMuteOthersDuringPreview(a0)),
            ),
            Item::new(
                "Automatically duck system audio",
                "Automatically reduces recorded system volume when speaking.",
                toggle(self.duck_system_audio, |a0| Msg::Settings(SettingsMsg::SetDuckSystemAudio(a0))),
            )
            .reset_with(
                self.duck_system_audio,
                d.duck_system_audio,
                |a0| Msg::Settings(SettingsMsg::SetDuckSystemAudio(a0)),
            ),
            Item::new(
                "Automatically sync with video",
                "",
                toggle(self.audio_sync_auto, |a0| Msg::Recording(RecordingMsg::SetAudioSyncAuto(a0))),
            )
            .reset_with(self.audio_sync_auto, d.audio_sync_auto, |a0| Msg::Recording(RecordingMsg::SetAudioSyncAuto(a0))),
        ];
        if !self.audio_sync_auto {
            mixing.push(
                Item::new(
                    "Audio sync offset",
                    "+ms delays audio (if sound is ahead of video), −ms advances it.",
                    num_input("0", &self.audio_sync_offset_ms.text, Some(|a0| Msg::Recording(RecordingMsg::SetAudioSyncOffset(a0)))),
                )
                .suffix("ms")
                .reset_with(
                    self.audio_sync_offset_ms.text.clone(),
                    d.audio_sync_offset_ms.to_string(),
                    |a0| Msg::Recording(RecordingMsg::SetAudioSyncOffset(a0)),
                ),
            );
        }
        secs.push(SectionSpec { title: "Mixing", items: mixing });
        secs
    }
}
