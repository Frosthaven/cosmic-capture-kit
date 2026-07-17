//! `RecordingMsg` handling — the live recording's controls, polls, and tray.
//! Split from `application.rs` (DRAGON-115).

use super::super::*;

impl App {
    pub(in crate::app) fn update_recording(&mut self, message: RecordingMsg) -> Task<cosmic::Action<Msg>> {
        match message {
            RecordingMsg::SetAudioSyncOffset(s) => {
                // Free-form (allows a lone "-" mid-typing); the last value that parses
                // to -1000..=1000 ms wins.
                if self.audio_sync_offset_ms.edit(s, -1000..=1000) {
                    self.save_state();
                }
                Task::none()
            }
            RecordingMsg::SetAudioSyncAuto(b) => {
                self.audio_sync_auto = b;
                self.save_state();
                Task::none()
            }
            RecordingMsg::StopRecording => self.stop_recording(),
            RecordingMsg::TogglePause => self.toggle_pause(),
            RecordingMsg::CancelRecording => self.cancel_recording(),
            RecordingMsg::RecordingPoll => {
                self.read_levels(); // keep the on-button meters live during recording
                // Portal hotkey events (PTT hold / stop) arrive on their own thread,
                // stamped at signal time; apply them on this poll's cadence.
                let hotkey_task = self.drain_portal_hotkeys();
                let done = self
                    .recording
                    .as_ref()
                    .and_then(|r| r.done.lock().ok().and_then(|g| g.clone()));
                let main = match done {
                    Some(result) => {
                        // Auto-calibrate the A/V offset from this recording's measured
                        // latency (median), so the user never has to hand-tune it. The
                        // worker's median is only the lag the app can SEE (frame →
                        // encoder); the persisted calibration base is the end-to-end
                        // delivery lag it can't, measured once via `--calibrate-sync`
                        // (DRAGON-119) — their sum is the real offset to compensate.
                        if self.audio_sync_auto
                            && let Some(handle) = &self.recording
                            && let Some(raw) =
                                handle.measured_offset_ms.lock().ok().and_then(|g| *g)
                        {
                            let ms = (raw + self.av_calibration_base_ms).clamp(-1000, 1000);
                            if ms != self.audio_sync_offset_ms.value {
                                self.audio_sync_offset_ms.set_value(ms);
                                self.save_state();
                            }
                        }
                        self.recording = None;
                        self.recording_started = None;
                        self.recording_paused_at = None;
                        self.recording_paused_accum = std::time::Duration::ZERO;
                        self.end_recording_tray();
                        self.mic_level = 0.0;
                        self.sys_level = 0.0;
                        // `recording_path` was the temp capture (the worker deletes
                        // it during finalize); drop our reference and clean up if it
                        // somehow survived.
                        if let Some(temp) = self.recording_path.take() {
                            let _ = std::fs::remove_file(temp);
                        }
                        if self.recording_cancelled {
                            // Discard the finalized file; no save, no notification.
                            if let Ok(p) = &result {
                                let _ = std::fs::remove_file(p);
                            }
                            self.recording_cancelled = false;
                            return self.finish_session();
                        }
                        match result {
                            // Saved already; show it in the preview overlay (or share
                            // directly when preview is off / already exited).
                            Ok(path) => {
                                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                                // Recording pre-opened the spinner, so present_capture reuses
                                // that surface; the poster load re-fits it (video dims come
                                // from ffprobe, not known here).
                                self.present_capture(path, size, true, None)
                            }
                            Err(e) => {
                                log::warn!("recording failed: {e}");
                                self.finish_session()
                            }
                        }
                    }
                    None => Task::none(),
                };
                Task::batch([hotkey_task, main])
            }
            RecordingMsg::MeterTick => {
                self.read_levels();
                Task::none()
            }
            RecordingMsg::ToggleMic => {
                // During a recording in push-to-talk mode the mic can't be toggled —
                // it's hold-to-talk only. Before recording it toggles the arm normally.
                if self.ptt_active() && self.recording.is_some() {
                    return Task::none();
                }
                self.record_mic = !self.record_mic;
                self.save_state();
                self.log_audio_toggle(crate::record::AudioChannel::Mic, self.record_mic);
                self.sync_meters();
                self.refresh_tray_audio();
                Task::none()
            }
            RecordingMsg::ToggleSystemAudio => {
                self.record_system_audio = !self.record_system_audio;
                self.save_state();
                self.log_audio_toggle(crate::record::AudioChannel::Sys, self.record_system_audio);
                self.sync_meters();
                self.refresh_tray_audio();
                Task::none()
            }
            RecordingMsg::TrayPoll => {
                // Drain the tray menu clicks and dispatch each. The recording controls map
                // to their `RecordingMsg`; the idle session icon's Quit (DRAGON-174) ends the
                // whole capture session (there is no resident to quit on a child-owned icon).
                let events = self.tray.as_ref().map(|t| t.poll()).unwrap_or_default();
                let mut task = Task::none();
                for ev in events {
                    let msg = match ev {
                        crate::tray::TrayEvent::Stop => RecordingMsg::StopRecording,
                        crate::tray::TrayEvent::TogglePause => RecordingMsg::TogglePause,
                        crate::tray::TrayEvent::ToggleMic => RecordingMsg::ToggleMic,
                        crate::tray::TrayEvent::ToggleSystemAudio => RecordingMsg::ToggleSystemAudio,
                        crate::tray::TrayEvent::Cancel => RecordingMsg::CancelRecording,
                        crate::tray::TrayEvent::Quit => {
                            task = Task::batch([task, self.finish_session()]);
                            continue;
                        }
                    };
                    task = Task::batch([task, self.update_recording(msg)]);
                }
                task
            }
        }
    }

    /// Push the current mic / system-audio state to the tray menu checkmarks (no-op
    /// when there's no tray).
    fn refresh_tray_audio(&self) {
        if let Some(tray) = &self.tray {
            tray.set_audio(self.record_mic, self.record_system_audio);
        }
    }
}
