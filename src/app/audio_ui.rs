use super::*;

impl App {
    /// Refresh the cached per-channel levels (0..1): the mic from its full-chain
    /// capture's newest column (the CLEAN level — gated + auto-gained, exactly what
    /// the mic test's waveform shows and what a recording would contain), system
    /// audio from its raw meter file.
    pub(super) fn read_levels(&mut self) {
        use crate::audio::meters::read_meter_level;
        use crate::record::AudioChannel;
        self.mic_level = self
            .mic_chain
            .as_ref()
            .and_then(|t| t.shared.lock().ok().and_then(|g| g.0.back().map(|c| c.0)))
            .unwrap_or(0.0);
        // macOS (Bug B): keep the armed-idle metering capture's channel drained each tick
        // so it never backs up (the level is published internally, chunks are discarded).
        #[cfg(target_os = "macos")]
        self.drain_sys_idle_meter();
        self.sys_level = read_meter_level(AudioChannel::Sys);
    }

    /// Whether a channel's on-button level meter should be running: it's armed/green,
    /// i.e. video mode with that channel on (independent of recording — so you can see audio
    /// working before you start). The Audio page's sensitivity bar uses its own capture
    /// (see [`should_capture_mic_input`](Self::should_capture_mic_input)), not this.
    pub(super) fn meter_should_run(&self) -> (bool, bool) {
        let video = self.kind == Kind::Video;
        (video && self.record_mic, video && self.record_system_audio)
    }

    /// Whether the full-chain mic-test capture should run: for the test modal, or for the live
    /// Input Sensitivity bar on the Audio page (manual mode). The bar shows the gate's DECISION
    /// level, which only the full InputProcessor produces — so it needs this, not the raw meter.
    pub(super) fn should_capture_mic_input(&self) -> bool {
        use super::settings::{AudioVideoTab, ConfigTab};
        self.mic_test_modal_open
            || (self.settings.window.is_some()
                && self.settings.active() == ConfigTab::AudioVideo
                && self.settings.active_audio_video_tab() == AudioVideoTab::Audio
                && !self.input_sensitivity_auto)
    }

    /// Start/stop the mic-test capture to match [`should_capture_mic_input`]. Idempotent — call
    /// whenever the modal opens/closes or the sensitivity mode / active page changes.
    ///
    /// [`should_capture_mic_input`]: Self::should_capture_mic_input
    pub(super) fn sync_mic_input(&mut self) {
        match (self.should_capture_mic_input(), self.mic_test.is_some()) {
            (true, false) => self.open_mic_test(),
            (false, true) => self.close_mic_test(),
            _ => {}
        }
    }

    /// Refresh the Input Sensitivity bar level from the capture's newest column — the gate's
    /// decision level (denoised, pre-gate/gain) — so the bar tracks exactly what the threshold
    /// is compared against. 0 when the capture isn't running.
    pub(super) fn read_sens_level(&mut self) {
        self.sens_level = self
            .mic_test
            .as_ref()
            .and_then(|t| t.shared.lock().ok().and_then(|g| g.0.back().map(|c| c.2)))
            .unwrap_or(0.0);
    }

    /// Start/stop the per-channel meter captures to match `meter_should_run`.
    /// Idempotent — call it whenever the mode or a channel toggle changes.
    pub(super) fn sync_meters(&mut self) {
        use crate::audio::meters::{spawn_meter, stop_meter};
        use crate::record::AudioChannel;
        let (mic, sys) = self.meter_should_run();
        // Mic: the FULL input chain (same capture as the mic test) so the button's
        // level reflects every active filter, not the raw device.
        match (mic, self.mic_chain.is_some()) {
            (true, false) => self.mic_chain = self.spawn_mic_chain(),
            (false, true) => {
                self.stop_mic_chain();
            }
            _ => {}
        }
        // System audio: raw RMS sidecar (no filter chain applies to it). On macOS
        // `spawn_meter(Sys)` returns None by design (no pulse monitor) — the armed-idle
        // meter is a metering-only SCK capture instead, handled below.
        match (sys, self.sys_meter.is_some()) {
            (true, false) => self.sys_meter = spawn_meter(AudioChannel::Sys),
            (false, true) => {
                if let Some(mut c) = self.sys_meter.take() {
                    stop_meter(AudioChannel::Sys, &mut c);
                }
                self.sys_level = 0.0;
            }
            _ => {}
        }
        // macOS (DRAGON-130 Bug B): armed-idle system-audio metering. While the system
        // channel is armed AND no recording is in flight, run a metering-only
        // `MonitorCapture` (audio-only SCK) — its chunks are discarded (`try_send` drops
        // them, nothing reads the receiver), and it publishes the sys RMS to
        // `SYS_LEVEL_BITS` on its own thread. It MUST be stopped before a recording's own
        // capture starts (`stop_sys_idle_meter`, called from the record-start path) so
        // the two never fight over the single SCK system-audio stream.
        #[cfg(target_os = "macos")]
        {
            let want_idle = sys && self.recording.is_none();
            match (want_idle, self.sys_idle_meter.is_some()) {
                (true, false) => {
                    // Keep the receiver so the meter tick can DRAIN + discard chunks — the
                    // capture publishes the RMS internally (before the channel send), so an
                    // undrained bounded channel would just log "consumer backlog" noise.
                    self.sys_idle_meter = crate::audio::capture::MonitorCapture::start(None, None);
                }
                (false, true) => self.stop_sys_idle_meter(),
                _ => {}
            }
        }
    }

    /// macOS (DRAGON-130 Bug B): stop the armed-idle system-audio metering capture, if
    /// running, and flatten the published level. Called both from `sync_meters` (when the
    /// channel disarms) and from the record-start path BEFORE the owned capture starts —
    /// so the metering-only stream is released before the recording claims the SCK
    /// system-audio stream (they must never run at once). The `stop()` is bounded (≤2s).
    #[cfg(target_os = "macos")]
    pub(super) fn stop_sys_idle_meter(&mut self) {
        if let Some((c, _rx)) = self.sys_idle_meter.take() {
            let _ = c.stop();
            crate::audio::meters::publish_sys_level(0.0);
            self.sys_level = 0.0;
        }
    }

    /// macOS (Bug B): drain + discard the armed-idle metering capture's chunks so its
    /// bounded channel never fills (an undrained channel logs "consumer backlog" noise).
    /// The capture publishes the meter level internally; we only need to keep the pipe
    /// flowing, so the drained chunks are dropped. No-op when the idle meter isn't running.
    #[cfg(target_os = "macos")]
    pub(super) fn drain_sys_idle_meter(&mut self) {
        if let Some((_, rx)) = self.sys_idle_meter.as_ref() {
            while rx.try_recv().is_ok() {}
        }
    }

    /// Spawn the mic button's full-chain level capture (a tiny clean_mic buffer —
    /// only the newest column is read).
    fn spawn_mic_chain(&self) -> Option<super::MicTest> {
        crate::audio::clean_mic::spawn_mic_test(
            &self.mic_device,
            8,
            self.input_config(),
            &self.speaker_device,
        )
        .map(|(child, shared)| super::MicTest {
            child,
            shared,
            produced: 0,
            stall_ticks: 0,
        })
    }

    /// Kill the mic chain capture and zero its level.
    pub(super) fn stop_mic_chain(&mut self) {
        if let Some(mut t) = self.mic_chain.take() {
            let _ = t.child.kill();
            let _ = t.child.wait();
        }
        self.mic_level = 0.0;
    }

    /// Dropdown index for the chosen mic (0 = System / automatic; otherwise the
    /// device's position in `mic_devices`, +1 for the leading "System" entry).
    pub(super) fn mic_device_index(&self) -> usize {
        if self.mic_device.is_empty() {
            0
        } else {
            self.mic_devices
                .iter()
                .position(|(n, _)| n == &self.mic_device)
                .map(|i| i + 1)
                .unwrap_or(0)
        }
    }

    /// Re-enumerate input devices and rebuild the dropdown labels. Called when the
    /// settings window opens so freshly plugged mics show up.
    pub(super) fn refresh_mic_devices(&mut self) {
        self.mic_devices = crate::audio::devices::list_input_sources();
        self.mic_device_labels = std::iter::once("System (automatic)".to_string())
            .chain(self.mic_devices.iter().map(|(_, d)| d.clone()))
            .collect();
    }

    /// Dropdown index for the chosen speaker (0 = System / automatic; otherwise the
    /// sink's position in `speaker_devices`, +1 for the leading "System" entry).
    /// Linux-only caller (the Output picker; macOS has no output section, DRAGON-132).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub(super) fn speaker_device_index(&self) -> usize {
        if self.speaker_device.is_empty() {
            0
        } else {
            self.speaker_devices
                .iter()
                .position(|(n, _)| n == &self.speaker_device)
                .map(|i| i + 1)
                .unwrap_or(0)
        }
    }

    /// Re-enumerate output sinks and rebuild the speaker dropdown labels.
    pub(super) fn refresh_speaker_devices(&mut self) {
        self.speaker_devices = crate::audio::devices::list_output_sinks();
        self.speaker_device_labels = std::iter::once("System (automatic)".to_string())
            .chain(self.speaker_devices.iter().map(|(_, d)| d.clone()))
            .collect();
    }

    /// Enumerate mic input + speaker output devices now the settings window is
    /// up, so the dropdowns reflect whatever is currently plugged in. Called
    /// when the config window opens.
    pub(super) fn refresh_audio_devices(&mut self) {
        self.refresh_mic_devices();
        self.refresh_speaker_devices();
    }

    /// Restart the mic level capture if one is running (after a device or filter
    /// change), so its readings reflect the newly selected source + chain config.
    pub(super) fn restart_mic_meter(&mut self) {
        if self.mic_chain.is_some() {
            self.stop_mic_chain();
            self.mic_chain = self.spawn_mic_chain();
        }
    }

    /// Open the live mic test: start a capture from the current device into a rolling
    /// waveform buffer. No-op (leaves `mic_test` None) if ffmpeg won't start.
    pub(super) fn open_mic_test(&mut self) {
        // Buffer the full waveform width of peak-envelope columns (≈2.2s at 100/s); the
        // canvas right-aligns to this same capacity so bars stay a fixed width and fill
        // in from the right.
        let columns = super::settings::MIC_WAVE_COLUMNS;
        if let Some((child, shared)) = crate::audio::clean_mic::spawn_mic_test(
            &self.mic_device,
            columns,
            self.input_config(),
            &self.speaker_device,
        ) {
            self.mic_test = Some(super::MicTest {
                child,
                shared,
                produced: 0,
                stall_ticks: 0,
            });
        }
    }

    /// Close the live mic test: kill the capture process and drop its state.
    pub(super) fn close_mic_test(&mut self) {
        if let Some(mut t) = self.mic_test.take() {
            let _ = t.child.kill();
            let _ = t.child.wait();
        }
    }

    /// Re-point the running mic captures at the current audio config (close +
    /// reopen), so a settings change (device, DSP toggle, …) is reflected in the
    /// live waveform AND the toolbar's full-chain button meter. No-ops when not
    /// running.
    pub(super) fn restart_mic_test_if_open(&mut self) {
        if self.mic_test.is_some() {
            self.close_mic_test();
            self.open_mic_test();
        }
        // The toolbar's mic meter runs the same chain — keep it config-accurate too.
        self.restart_mic_meter();
    }
}
