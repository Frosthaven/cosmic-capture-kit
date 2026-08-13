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
                // DRAGON-659/661: has the worker settled yet? `settled_at`, the end of its
                // opening phase, is media 0: it anchors the elapsed clock where the file
                // begins AND raises the tray, which is the same "this is live" claim the
                // record chip's STOP face makes (DRAGON-673 collapsed the two onto this one
                // signal; the anchor used to read the first frame, a whole countdown early).
                // Both latches are idempotent, so this costs one `Option` test per tick for
                // the rest of the recording.
                //
                // The rebuild is only owed when the tray took the toolbar's place, so it
                // follows the TRAY half: the input regions were cut at promotion, when
                // `tray_hides_toolbar` was still false, so without this the hidden toolbar's
                // zone would keep eating clicks. Every other session skips it (the flag is
                // off by default).
                let warm_task = if self.adopt_warm_start() && self.tray_hides_toolbar {
                    self.recreate_active_overlays()
                } else {
                    Task::none()
                };
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
                        self.clear_recording_state();
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
                                // DRAGON-419 (silent-exit path S7). The worker's reason string
                                // is the whole diagnosis for a recording that produced nothing
                                // — it names the pre-flight stage or the muxer failure — and it
                                // was going nowhere but stderr.
                                log::warn!("recording failed: {e}");
                                crate::diag::note_failure(
                                    crate::diag::Failure::RecordingFailed,
                                    &format!("worker reported: {e}"),
                                );
                                // DRAGON-415: the worker's own reason is the whole diagnosis
                                // for a recording that produced nothing, so the alert repeats
                                // it verbatim rather than inventing a generic one.
                                self.fail_session()
                            }
                        }
                    }
                    // Still recording. DRAGON-423: this poll is the ONE place in the process
                    // a wedged worker cannot block, so it is where the session-level bound
                    // is judged — is this recording still making progress?
                    None => match self.observe_recording_progress() {
                        Some(stall) => self.abandon_wedged_recording(stall),
                        None => Task::none(),
                    },
                };
                Task::batch([hotkey_task, warm_task, main])
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
                // The `mut` is used by the recording-control `extend` just below, which is
                // `cfg(target_os = "linux")` only. So off Linux the binding is honestly
                // immutable, and the attribute says exactly where rather than blanketing
                // the file. macOS and Windows both hold a ZERO-warning clippy bar.
                #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
                let mut events = self.tray.as_ref().map(|t| t.poll()).unwrap_or_default();
                // DRAGON-583: recording-control commands from ANOTHER process (a Linux
                // global hotkey running `--toggle-mic` and friends) arrive on the same
                // tick, as the same five `TrayEvent`s the resident's own menu commands
                // already become. Mapped exactly as `tray.rs`'s resident reader maps them,
                // so a hotkey, a resident menu click and a tray click are one code path
                // from here down.
                #[cfg(target_os = "linux")]
                if let Some(inlet) = self.record_control.as_ref() {
                    events.extend(inlet.drain().into_iter().map(|cmd| match cmd {
                        crate::daemon_ipc::Command::TogglePause => {
                            crate::tray::TrayEvent::TogglePause
                        }
                        crate::daemon_ipc::Command::ToggleMic => crate::tray::TrayEvent::ToggleMic,
                        crate::daemon_ipc::Command::ToggleSystemAudio => {
                            crate::tray::TrayEvent::ToggleSystemAudio
                        }
                        crate::daemon_ipc::Command::Stop => crate::tray::TrayEvent::Stop,
                        crate::daemon_ipc::Command::Cancel => crate::tray::TrayEvent::Cancel,
                    }));
                }
                let mut task = Task::none();
                for ev in events {
                    let msg = match ev {
                        crate::tray::TrayEvent::Stop => RecordingMsg::StopRecording,
                        crate::tray::TrayEvent::TogglePause => RecordingMsg::TogglePause,
                        crate::tray::TrayEvent::ToggleMic => RecordingMsg::ToggleMic,
                        crate::tray::TrayEvent::ToggleSystemAudio => RecordingMsg::ToggleSystemAudio,
                        // DRAGON-558: an "Audio Recording" radio pick carries the COMPLETE
                        // arm state. The app owns the arms, so it computes the toggle diff
                        // against its own pair and runs each flip through the existing
                        // handlers — persist, meters, tray refresh and the push-to-talk
                        // rule all apply exactly as if the toggles were clicked one by one.
                        crate::tray::TrayEvent::AudioArms(choice) => {
                            let current = (self.record_mic, self.record_system_audio);
                            for action in
                                crate::recording_ui::audio_pick_live_actions(current, choice)
                            {
                                let msg = match action {
                                    crate::recording_ui::RecordingAction::ToggleMic => {
                                        RecordingMsg::ToggleMic
                                    }
                                    crate::recording_ui::RecordingAction::ToggleSystemAudio => {
                                        RecordingMsg::ToggleSystemAudio
                                    }
                                    _ => continue,
                                };
                                task = Task::batch([task, self.update_recording(msg)]);
                            }
                            continue;
                        }
                        // DRAGON-574: a "Countdown Timer" radio pick carries the preset
                        // index. The app owns the delay state, so it routes through the
                        // SAME `PickDelay` handler as the overlay's delay chip — persist
                        // and the live chip follow in one place.
                        crate::tray::TrayEvent::CountdownPick(idx) => {
                            task = Task::batch([
                                task,
                                self.update_capture(CaptureMsg::PickDelay(idx)),
                            ]);
                            continue;
                        }
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
