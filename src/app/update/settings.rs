//! `SettingsMsg` handling — every settings-window control.
//! Split from `application.rs` (DRAGON-115).

use super::super::*;
use cosmic::widget::color_picker::ColorPickerUpdate;

impl App {
    pub(in crate::app) fn update_settings(&mut self, message: SettingsMsg) -> Task<cosmic::Action<Msg>> {
        match message {
            SettingsMsg::SetCaptureCursor(b) => {
                self.capture_cursor = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetCaptureTransparency(b) => {
                self.capture_transparency = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetCaptureWallpaper(b) => {
                self.capture_wallpaper = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetWindowFocusAppearance(i) => {
                // 0 = Active, 1 = Inactive (DRAGON-191).
                self.window_single_active = i == 0;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetSelectionBoxThickness(w) => {
                self.selection_box_thickness = w.clamp(1, 8);
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetActiveBorderWidth(w) => {
                self.active_border_width = w.min(10);
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetInactiveBorderWidth(w) => {
                self.inactive_border_width = w.min(10);
                self.save_state();
                Task::none()
            }
            SettingsMsg::ResetActiveBorder => {
                let d = crate::state::defaults();
                self.active_border_color = d.active_border_color; // None = follow accent
                self.active_border_width = d.active_border_width;
                self.save_state();
                Task::none()
            }
            SettingsMsg::ResetInactiveBorder => {
                let d = crate::state::defaults();
                self.inactive_border_color = d.inactive_border_color;
                self.inactive_border_width = d.inactive_border_width;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetWindowDropShadow(b) => {
                self.window_drop_shadow = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::ToggleBorderColorEditor(target, open) => {
                if open {
                    // Seed the picker with the target border's current colour (the
                    // resolved accent when the Active border follows it, so the picker
                    // opens on what's shown).
                    let initial = match target {
                        crate::app::BorderColorTarget::Active => self
                            .active_border_color
                            .unwrap_or_else(crate::decoration::accent_rgba),
                        crate::app::BorderColorTarget::Inactive => self.inactive_border_color,
                    };
                    let [r, g, b, _] = initial;
                    let seed = cosmic::iced::Color::from_rgb(
                        r as f32 / 255.0,
                        g as f32 / 255.0,
                        b as f32 / 255.0,
                    );
                    self.settings.border_picker = cosmic::widget::ColorPickerModel::new(
                        "Hex",
                        "RGB",
                        None,
                        Some(seed),
                    );
                }
                self.settings.border_editor = if open { Some(target) } else { None };
                Task::none()
            }
            SettingsMsg::BorderColorPicker(u) => {
                let target = self.settings.border_editor;
                // Save/Reset apply + persist a colour; Save/Reset/Cancel all close.
                let close = matches!(
                    u,
                    ColorPickerUpdate::AppliedColor
                        | ColorPickerUpdate::Reset
                        | ColorPickerUpdate::Cancel
                );
                let applied = matches!(u, ColorPickerUpdate::AppliedColor);
                let reset = matches!(u, ColorPickerUpdate::Reset);
                let picker_task =
                    self.settings.border_picker.update::<Msg>(u).map(cosmic::Action::App);
                if close {
                    self.settings.border_editor = None;
                }
                if let Some(target) = target
                    && (applied || reset)
                {
                    let to_byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                    let chosen = self
                        .settings
                        .border_picker
                        .get_applied_color()
                        .map(|c| [to_byte(c.r), to_byte(c.g), to_byte(c.b), 255]);
                    match target {
                        crate::app::BorderColorTarget::Active => {
                            // Reset clears the Active colour back to "follow the accent"
                            // (None); an applied colour pins the custom value.
                            self.active_border_color = if reset { None } else { chosen };
                        }
                        crate::app::BorderColorTarget::Inactive => {
                            // The Inactive border is always concrete; Reset restores its
                            // default (0xff414550).
                            if let Some(c) = chosen.filter(|_| applied) {
                                self.inactive_border_color = c;
                            } else if reset {
                                self.inactive_border_color =
                                    crate::state::defaults().inactive_border_color;
                            }
                        }
                    }
                    self.save_state();
                }
                picker_task
            }
            // Transparency multiplier parked (linear-light over() makes it redundant):
            // Msg::SetWindowTransparencyMultiplier(v) => {
            //     self.window_transparency_multiplier = v.clamp(0.0, 1.0);
            //     self.save_state();
            //     Task::none()
            // }
            SettingsMsg::SetWindowPadding(b) => {
                self.window_padding = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetWindowPaddingPx(s) => {
                if let Ok(v) = s.trim().parse::<u32>() {
                    self.window_padding_px.value = v.min(512);
                }
                self.window_padding_px.set_text(s);
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetFreeze(b) => {
                // Immediate: the snapshot is always grabbed at launch, so this just
                // shows/hides the frozen background on the next render.
                self.freeze = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetAllowMultiple(b) => {
                // Applies next launch (the instance lock is taken in main()).
                self.allow_multiple = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetGeneralTab(entity) => {
                // In-page General tab (DRAGON-138) — pure view state, not persisted.
                self.settings.general_tab.activate(entity);
                Task::none()
            }
            SettingsMsg::SetCaptureTab(entity) => {
                // In-page Capture Modes tab (DRAGON-140) — pure view state, not persisted.
                self.settings.capture_tab.activate(entity);
                Task::none()
            }
            SettingsMsg::SetAudioVideoTab(entity) => {
                // In-page Audio & Video tab (DRAGON-141) — not persisted. Unlike the other
                // in-page strips this drives a capture stream: the live mic sensitivity bar
                // is gated on the Audio tab being active (see `should_capture_mic_input`), so
                // switching tabs must start/stop it exactly like switching nav pages did
                // (mirrors the `SetConfigTab` handler's `sync_mic_input`) — otherwise a meter
                // is left holding a capture stream after leaving the Audio tab.
                self.settings.audio_video_tab.activate(entity);
                self.sync_mic_input();
                Task::none()
            }
            SettingsMsg::SetShortcutsTab(entity) => {
                // In-page Keyboard Shortcuts tab (DRAGON-142) — pure view state, not
                // persisted. An in-flight rebind capture targets a row that may no
                // longer be visible; drop it so the next keypress can't silently bind
                // an off-screen action.
                self.settings.shortcuts_tab.activate(entity);
                self.settings.rebinding = None;
                Task::none()
            }
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            SettingsMsg::SetResident(b) => {
                // Residency is a SEPARATE tray/menu-bar RESIDENT process (macOS
                // `crate::daemon`, Linux `crate::daemon_linux`, DRAGON-173), so flipping
                // this drives the resident's lifecycle directly — the tray item
                // appears/disappears immediately, no relaunch. The spawn / SIGTERM
                // plumbing is portable (a detached bare launch early-branches to the
                // resident; SIGTERM the resident lock-holder to stop it); only the
                // launch-at-login backend differs per OS (SMAppService vs XDG autostart).
                self.resident = b;
                self.save_state();
                if b {
                    // Turn ON: spawn the resident DETACHED so its tray item comes up at
                    // once (this settings process is a one-shot GUI child, not the
                    // resident). The explicit `resident` argument declares DAEMON intent
                    // (DRAGON-181): a bare launch is capture-intent and would also spawn
                    // a capture child — toggling the setting must only raise the tray.
                    // (On macOS the extra argument is inert: the bare check ignores it
                    // and the launch still early-branches to the daemon.) Its own
                    // single-instance lock makes a redundant spawn a harmless no-op.
                    if let Ok(exe) = std::env::current_exe() {
                        match std::process::Command::new(&exe).arg("resident").spawn() {
                            Ok(child) => log::info!("resident on: spawned resident (pid {})", child.id()),
                            Err(e) => log::warn!("resident on: resident spawn failed: {e}"),
                        }
                    }
                    // Launch-at-login is assumed TRUE whenever background mode is on (no
                    // separate toggle, DRAGON-158): register it so the resident comes back
                    // after a reboot/login. macOS uses SMAppService; Linux writes an XDG
                    // autostart `.desktop`. Best-effort: log on failure, never panic.
                    #[cfg(target_os = "macos")]
                    if !crate::platform::mac::login_item::is_enabled() {
                        if let Err(e) = crate::platform::mac::login_item::set(true) {
                            log::warn!("resident on: login-item register failed: {e}");
                        } else {
                            log::info!("resident on: registered login item");
                        }
                    }
                    #[cfg(target_os = "linux")]
                    if !crate::platform::linux_autostart::is_enabled() {
                        if let Err(e) = crate::platform::linux_autostart::set(true) {
                            log::warn!("resident on: autostart entry write failed: {e}");
                        } else {
                            log::info!("resident on: wrote XDG autostart entry");
                        }
                    }
                } else {
                    // Turn OFF: ask the running resident to exit (SIGTERM the resident-lock
                    // holder) so the tray item disappears now; harmless no-op if none up.
                    if crate::instance::signal_daemon_quit() {
                        log::info!("resident off: signalled the resident to exit");
                    }
                    // Launch-at-login only makes sense in resident mode (DRAGON-158): turn
                    // it off too, else the OS keeps launching the resident at login.
                    // Best-effort: log on failure, never panic.
                    #[cfg(target_os = "macos")]
                    if crate::platform::mac::login_item::is_enabled() {
                        if let Err(e) = crate::platform::mac::login_item::set(false) {
                            log::warn!("resident off: login-item unregister failed: {e}");
                        } else {
                            log::info!("resident off: unregistered login item");
                        }
                    }
                    #[cfg(target_os = "linux")]
                    if crate::platform::linux_autostart::is_enabled() {
                        if let Err(e) = crate::platform::linux_autostart::set(false) {
                            log::warn!("resident off: autostart entry remove failed: {e}");
                        } else {
                            log::info!("resident off: removed XDG autostart entry");
                        }
                    }
                }
                Task::none()
            }
            SettingsMsg::SetRegionOpacity(v) => {
                self.region_overlay_opacity = v.clamp(0.0, 1.0);
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetActiveOpacity(v) => {
                self.active_overlay_opacity = v.clamp(0.0, 1.0);
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetPreviewOpacity(v) => {
                self.preview_overlay_opacity = v.clamp(0.0, 1.0);
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetRecordFps(s) => {
                // Free-form field; the last value that parses to 1..=240 wins.
                if self.record_fps.edit(s, 1..=240) {
                    self.save_state();
                }
                Task::none()
            }
            SettingsMsg::SetRecordBitrate(s) => {
                // Free-form field; the last value that parses to 100..=500000 Kbps wins.
                if self.record_bitrate_kbps.edit(s, 100..=500_000) {
                    self.save_state();
                }
                Task::none()
            }
            SettingsMsg::SetRecordResPreset(idx) => {
                self.record_res_preset = idx.min(RES_CUSTOM) as u8;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetNvencPreset(idx) => {
                if let Some(p) = crate::encode::NVENC_PRESETS.get(idx) {
                    self.nvenc_preset = p.to_string();
                    self.save_state();
                }
                Task::none()
            }
            SettingsMsg::SetX264Preset(idx) => {
                if let Some(p) = crate::encode::X264_PRESETS.get(idx) {
                    self.x264_preset = p.to_string();
                    self.save_state();
                }
                Task::none()
            }
            SettingsMsg::SetVaapiPreset(idx) => {
                if let Some(cl) = crate::encode::VAAPI_CL_VALUES.get(idx) {
                    self.vaapi_compression_level = *cl;
                    self.save_state();
                }
                Task::none()
            }
            #[cfg(feature = "zero-copy")]
            SettingsMsg::SetRecordZeroCopy(on) => {
                self.record_zero_copy = on;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetRecordCodec(idx) => {
                if let Some(c) = crate::encode::CODEC_VALUES.get(idx) {
                    self.record_codec = c.to_string();
                    self.save_state();
                }
                Task::none()
            }
            SettingsMsg::SetRecordMaxWidth(s) => {
                if self.record_max_width.edit(s, 2..=8192) {
                    self.save_state();
                }
                Task::none()
            }
            SettingsMsg::SetRecordMaxHeight(s) => {
                if self.record_max_height.edit(s, 2..=8192) {
                    self.save_state();
                }
                Task::none()
            }
            SettingsMsg::SetPreferredEncoder(i) => {
                if let Some(id) = self.encoders().get(i).map(|e| e.id.clone()) {
                    self.set_preferred_encoder(id);
                    self.save_state();
                }
                Task::none()
            }
            SettingsMsg::SetBenchMonitor(i) => {
                if i < self.bench_monitors.len() {
                    self.bench_monitor_idx = i;
                }
                Task::none()
            }
            SettingsMsg::RunBenchmark => self.spawn_encoder_bench(),
            SettingsMsg::BenchPoll => Task::none(),
            SettingsMsg::SetRecordDir(s) => {
                self.record_dir = s;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetRecordBackend(id) => {
                self.record_backend = id;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetScreenshotBackend(id) => {
                self.screenshot_backend = id;
                self.save_state();
                Task::none()
            }
            SettingsMsg::ResetScreencastPermission => {
                self.pw_restore_token = None;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetScreenshotDir(s) => {
                self.screenshot_dir = s;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetCovermarkText(s) => {
                self.covermark_text = s;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetPushToTalk(b) => {
                self.push_to_talk = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetHideToolbarFullscreen(b) => {
                self.hide_toolbar_fullscreen = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetPreviewWindowed(b) => {
                self.preview_windowed = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetAutoClosePreview(b) => {
                self.auto_close_preview = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetPreviewFloatCosmic(b) => {
                self.preview_float_cosmic = b;
                self.save_state();
                // Register / remove the COSMIC tiling exception so the change takes
                // effect on the next windowed preview (idempotent; no-op off COSMIC).
                // The writer lives in the Linux-only COSMIC profile now (DRAGON-220);
                // off Linux it was already an internal no-op, so gating the call is
                // byte-identical (the setter + save_state above still run everywhere).
                #[cfg(target_os = "linux")]
                crate::platform::linux::cosmic::quirks::set_cosmic_preview_float(b);
                Task::none()
            }
            SettingsMsg::PickDir(target) => {
                // The file chooser is opened from the settings window (its own
                // toplevel), so we don't touch the capture overlay at all here.
                Task::perform(pick_folder(), move |opt| {
                    cosmic::Action::App(Msg::Settings(SettingsMsg::DirPicked(target, opt)))
                })
            }
            SettingsMsg::DirPicked(target, opt) => {
                if let Some(path) = opt {
                    let s = path.to_string_lossy().into_owned();
                    match target {
                        DirTarget::Screenshot => self.screenshot_dir = s,
                        DirTarget::Recording => self.record_dir = s,
                    }
                    self.save_state();
                }
                // Stay in the settings window; closing the picker does nothing else.
                Task::none()
            }
            SettingsMsg::SetNoiseReduction(on) => {
                self.noise_reduction = on;
                self.save_state();
                // Re-point any live mic test so the waveform reflects the change.
                self.restart_mic_test_if_open();
                Task::none()
            }
            SettingsMsg::SetMicDevice(idx) => {
                self.mic_device = if idx == 0 {
                    String::new()
                } else {
                    self.mic_devices
                        .get(idx - 1)
                        .map(|(n, _)| n.clone())
                        .unwrap_or_default()
                };
                crate::audio::config::set_mic_source(&self.mic_device);
                self.save_state();
                self.restart_mic_meter();
                // A device change while testing should re-point the live waveform too.
                self.restart_mic_test_if_open();
                Task::none()
            }
            SettingsMsg::SetEchoCancellation(on) => {
                self.echo_cancellation = on;
                self.save_state();
                // Re-point any live mic test so the waveform reflects the change.
                self.restart_mic_test_if_open();
                Task::none()
            }
            SettingsMsg::SetSpeakerDevice(idx) => {
                self.speaker_device = if idx == 0 {
                    String::new()
                } else {
                    self.speaker_devices
                        .get(idx - 1)
                        .map(|(n, _)| n.clone())
                        .unwrap_or_default()
                };
                self.save_state();
                // Re-point the echo reference if a test is running.
                self.restart_mic_test_if_open();
                Task::none()
            }
            SettingsMsg::SetInputSensitivityAuto(on) => {
                self.input_sensitivity_auto = on;
                self.save_state();
                // The chain config changed and the capture's need may have (manual mode shows
                // the live bar): drop any running capture, then reopen it with the new config if
                // the modal or the bar still wants it.
                self.close_mic_test();
                self.sync_mic_input();
                Task::none()
            }
            SettingsMsg::SetInputSensitivity(v) => {
                // Slider drag: just store + persist; no mic-test respawn (it would
                // restart ffmpeg on every tick). The live bar reads the mic meter.
                self.input_sensitivity = v.clamp(0.0, 1.0);
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetAutoGain(on) => {
                self.auto_gain = on;
                self.save_state();
                self.restart_mic_test_if_open();
                Task::none()
            }
            SettingsMsg::SetAdvancedVad(on) => {
                self.advanced_vad = on;
                self.save_state();
                self.restart_mic_test_if_open();
                Task::none()
            }
            SettingsMsg::OpenMicTest => {
                self.mic_test_modal_open = true;
                self.sync_mic_input(); // start the capture if the bar wasn't already running it
                Task::none()
            }
            SettingsMsg::CloseMicTest => {
                self.mic_test_modal_open = false;
                self.sync_mic_input(); // keep it running only if the sensitivity bar still needs it
                Task::none()
            }
            SettingsMsg::MicTestTick => {
                // Refresh the live Input Sensitivity bar from the capture's decision level.
                self.read_sens_level();
                // Watchdog only (the canvas reads the buffer itself at vsync): if the
                // reader stops advancing after data had been flowing (ffmpeg hiccup, or a
                // DSP panic that killed the reader thread / poisoned its lock), auto-restart
                // the capture so the graph recovers without the user reopening the modal.
                let mut restart = false;
                if let Some(t) = &mut self.mic_test {
                    if let Ok(g) = t.shared.lock() {
                        if g.1 > t.produced {
                            t.produced = g.1;
                            t.stall_ticks = 0;
                        } else if t.produced > 0 {
                            t.stall_ticks += 1; // flowed before, now stalled
                        }
                    } else {
                        t.stall_ticks += 1; // poisoned lock = reader thread panicked
                    }
                    restart = t.stall_ticks >= 8; // ~1.6s at the 200ms watchdog tick
                }
                if restart {
                    self.close_mic_test();
                    self.open_mic_test();
                }
                Task::none()
            }
            SettingsMsg::SetCopyToClipboard(b) => {
                self.copy_to_clipboard = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetClipboardMaxMb(s) => {
                if self.clipboard_max_mb.edit(s, 1..=1024) {
                    self.save_state();
                }
                Task::none()
            }
            SettingsMsg::SetPreviewAfterCapture(b) => {
                self.preview_after_capture = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetMuteOthersDuringPreview(b) => {
                self.mute_others_during_preview = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetDuckSystemAudio(b) => {
                self.duck_system_audio = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetUseSystemAppearance(b) => {
                // Turning ON reverts the live theme to following the system; the
                // override values are kept (but ignored) so toggling OFF restores them.
                self.appearance_use_system = b;
                if b {
                    // The custom-accent sidebar is only reachable via the override rows,
                    // which vanish now — close it so it can't linger.
                    self.settings.accent_editor_open = false;
                }
                self.save_state();
                self.apply_appearance_task()
            }
            SettingsMsg::SetAppearanceMode(m) => {
                self.appearance_mode = m.min(2);
                self.save_state();
                self.apply_appearance_task()
            }
            SettingsMsg::SetAppearanceAccent(c) => {
                self.appearance_accent = c;
                self.save_state();
                self.apply_appearance_task()
            }
            SettingsMsg::SetAppearanceRoundness(r) => {
                self.appearance_roundness = r.min(2);
                self.save_state();
                self.apply_appearance_task()
            }
            SettingsMsg::ToggleAccentEditor(open) => {
                if open {
                    // Seed the picker with the current accent so it opens on the live
                    // colour (or the base theme's accent when no override is set).
                    let initial = self
                        .appearance_accent
                        .map(|[r, g, b]| cosmic::iced::Color::from_rgb(r, g, b))
                        .unwrap_or_else(|| theme::accent(&cosmic::theme::active()));
                    self.settings.accent_picker = cosmic::widget::ColorPickerModel::new(
                        "Hex",
                        "RGB",
                        None,
                        Some(initial),
                    );
                }
                self.settings.accent_editor_open = open;
                Task::none()
            }
            SettingsMsg::AccentPicker(u) => {
                // Save/Reset apply + persist a colour; Save/Reset/Cancel all close.
                let close = matches!(
                    u,
                    ColorPickerUpdate::AppliedColor
                        | ColorPickerUpdate::Reset
                        | ColorPickerUpdate::Cancel
                );
                let apply = matches!(u, ColorPickerUpdate::AppliedColor | ColorPickerUpdate::Reset);
                // Drive the picker model (also handles the "copy to clipboard" task).
                let picker_task = self.settings.accent_picker.update::<Msg>(u).map(cosmic::Action::App);
                if close {
                    self.settings.accent_editor_open = false;
                }
                if apply {
                    // Reset clears to the model's fallback (None here) → no override;
                    // Applied keeps the chosen colour.
                    self.appearance_accent = self
                        .settings
                        .accent_picker
                        .get_applied_color()
                        .map(|c| [c.r, c.g, c.b]);
                    self.save_state();
                    return Task::batch([picker_task, self.apply_appearance_task()]);
                }
                picker_task
            }
            SettingsMsg::BeginRebind(action) => {
                // Toggle: clicking the row that's already capturing cancels it.
                self.settings.rebinding = if self.settings.rebinding == Some(action) {
                    None
                } else {
                    Some(action)
                };
                Task::none()
            }
            SettingsMsg::SetShortcut(action, shortcut) => {
                self.keymap.set(action, shortcut);
                self.settings.rebinding = None;
                self.save_state();
                Task::none()
            }
            SettingsMsg::UnbindShortcut(action) => {
                self.keymap.unbind(action);
                self.settings.rebinding = None;
                self.save_state();
                Task::none()
            }
            #[cfg(target_os = "macos")]
            SettingsMsg::SetCaptureHotkey(spec) => {
                // Persist the raw text as typed. The daemon falls back to the default if
                // it can't be parsed, so a half-typed spec is never dangerous. We restart
                // the running daemon when the new spec PARSES (a recorded chord or the
                // restore-default), OR is CLEARED (the "x" — the daemon should come up
                // with no global hotkey); a merely-invalid spec leaves the daemon on its
                // last-good key rather than resetting it out from under the user.
                //
                // PrintScreen is recordable despite the daemon owning it: while the chord
                // recorder is armed, `sub_hotkey_suspend_ping` pings the daemon (SIGUSR2)
                // every ~1s, which UN-registers its PrintScreen (+ F13) Carbon hotkey for
                // ~3s (extended per ping, auto-resumed on expiry — see `crate::daemon`
                // SuspendWindow). So a PrintScreen pressed during recording reaches THIS
                // app and is captured like any other key, then the daemon re-registers a
                // couple of seconds after recording ends. Restore-default remains as a
                // no-keypress way to re-select PrintScreen.
                let apply = crate::daemon::hotkey_spec_is_valid(&spec)
                    || crate::daemon::hotkey_spec_is_cleared(&spec);
                self.capture_hotkey = spec;
                self.save_state();
                if apply && self.resident {
                    // Restart-the-daemon (the SetResident plumbing pattern): SIGTERM the
                    // running daemon so it exits, then spawn a fresh detached daemon that
                    // re-reads the now-persisted spec at startup. If none is running the
                    // signal is a harmless no-op and the respawn just brings one up (it
                    // early-branches to `daemon::run` because `resident` is persisted on).
                    if crate::instance::signal_daemon_quit() {
                        log::info!("capture hotkey changed: signalled the daemon to restart");
                    }
                    if let Ok(exe) = std::env::current_exe() {
                        match std::process::Command::new(&exe).spawn() {
                            Ok(child) => {
                                log::info!("capture hotkey changed: respawned daemon (pid {})", child.id())
                            }
                            Err(e) => log::warn!("capture hotkey changed: daemon respawn failed: {e}"),
                        }
                    }
                }
                Task::none()
            }
            #[cfg(target_os = "macos")]
            SettingsMsg::BeginCaptureHotkeyRebind => {
                // Toggle: clicking the row that's already recording cancels it. Clear any
                // in-app rebind so the two capture modes are never armed at once.
                self.settings.rebinding = None;
                self.settings.capture_hotkey_rebinding = !self.settings.capture_hotkey_rebinding;
                // Just armed: send an IMMEDIATE suspend ping so the daemon un-registers
                // its hotkey NOW (PrintScreen becomes recordable) rather than after the
                // first ~1s timer tick. The `sub_hotkey_suspend_ping` subscription keeps
                // it extended thereafter; the daemon auto-resumes once pings stop.
                if self.settings.capture_hotkey_rebinding && self.resident {
                    crate::instance::signal_daemon_suspend_hotkey();
                }
                Task::none()
            }
            #[cfg(target_os = "macos")]
            SettingsMsg::SuspendDaemonHotkeyPing => {
                // Fire-and-forget: keep the daemon's hotkey suspended while recording.
                // Only meaningful with a running daemon; a no-op otherwise.
                if self.settings.capture_hotkey_rebinding && self.resident {
                    crate::instance::signal_daemon_suspend_hotkey();
                }
                Task::none()
            }
            #[cfg(target_os = "macos")]
            SettingsMsg::OpenTccPane(pane) => {
                // Deep-link into the relevant Privacy & Security pane; best-effort.
                crate::platform::mac::tcc::open_privacy_pane(pane);
                Task::none()
            }
            #[cfg(target_os = "macos")]
            SettingsMsg::RequestMicTcc => {
                // Fires the one-shot OS mic prompt (only when NotDetermined; a standing
                // decision just returns it). The Health row re-probes on next render.
                crate::platform::mac::tcc::request_mic();
                Task::none()
            }
            #[cfg(target_os = "macos")]
            SettingsMsg::RequestScreenTcc => {
                // Fires the one-shot OS Screen Recording prompt, then marks it spent
                // (the same flag init's first-run flow sets) so neither the row nor a
                // later capture launch offers/fires it again — from here on, System
                // Settings is the honest recovery. Written straight to disk (the flag
                // is a lifecycle marker, not a live setting cached on App); the grant
                // itself only applies to a fresh launch.
                crate::platform::mac::tcc::request_screen_capture();
                let mut p = crate::state::load();
                p.mac_first_run_seen = true;
                crate::state::save(&p);
                Task::none()
            }
            SettingsMsg::CheckForUpdates => {
                // Non-blocking: the curl fetch runs on a blocking pool; the result
                // lands back as `UpdateChecked`. Mark "Checking" so the About page
                // shows progress and a repeat click is a no-op while in flight.
                if matches!(self.update_status, crate::update::UpdateStatus::Checking) {
                    return Task::none();
                }
                self.update_status = crate::update::UpdateStatus::Checking;
                Task::perform(
                    async {
                        // Run the fetch, then hold "Checking..." until the interactive
                        // floor (DRAGON-177) so an instant result doesn't flip back and
                        // read as broken. The floor sleep runs on the blocking pool, so
                        // it never touches the UI thread; the fetch itself is unslowed.
                        let started = std::time::Instant::now();
                        let status = tokio::task::spawn_blocking(crate::update::check_now)
                            .await
                            .unwrap_or_else(|_| {
                                crate::update::UpdateStatus::Failed(
                                    "The update check could not run.".to_string(),
                                )
                            });
                        let remainder = crate::update::check_floor_remainder(
                            started.elapsed(),
                            crate::update::INTERACTIVE_CHECK_FLOOR,
                        );
                        if !remainder.is_zero() {
                            let _ = tokio::task::spawn_blocking(move || {
                                std::thread::sleep(remainder)
                            })
                            .await;
                        }
                        status
                    },
                    |status| {
                        cosmic::Action::App(Msg::Settings(SettingsMsg::UpdateChecked(status)))
                    },
                )
            }
            SettingsMsg::UpdateChecked(status) => {
                self.update_status = status;
                // DRAGON-177: parse the release notes into markdown once, here (not
                // per-frame in the view, which would re-parse every draw and can't
                // hold the borrow the widget needs). Both Available AND UpToDate
                // carry notes (the manifest's; when up to date they describe the
                // INSTALLED version), so the About changelog is always visible.
                // A result WITHOUT notes (Failed, or a manifest with empty notes)
                // leaves the previous block in place - stale notes beat a blink-out.
                if let Some((version, notes)) = self
                    .update_status
                    .notes_and_version()
                    .filter(|(_, notes)| !notes.trim().is_empty())
                {
                    self.update_notes = Some((
                        version.to_string(),
                        cosmic::widget::markdown::Content::parse(notes),
                    ));
                }
                // Refresh the About nav entry (glyph + colour) so the expanded rail
                // lights up the moment the check resolves while settings is open.
                self.update_about_nav_icon();
                // DRAGON-177: raise the launch-time "a new update is available" dialog
                // when the check resolves Available AND the notify setting is on. Only
                // when a settings window is actually open (the sole surface that runs
                // this check — a capture launch never gets here, so the dialog can never
                // interrupt a capture) and not already showing (a repeat check while the
                // pane is open must not re-pop it once dismissed this session). Also
                // suppressed when the active page is About: it already carries the same
                // controls, so the popup there is redundant (and never re-armed later).
                let on_about = self.settings.active() == crate::app::ConfigTab::About;
                if self.settings.window.is_some()
                    && self.update_dialog.is_none()
                    && !self.update_dialog_decided
                    && let Some(info) = crate::update::dialog_for_status(
                        &self.update_status,
                        self.notify_updates,
                        false,
                    )
                {
                    // Decide ONCE per session (the pure gate is evaluated page-blind,
                    // the page rule applied here): whether shown or About-suppressed,
                    // a later re-check (the cache seed is followed by a network
                    // refresh ~2s behind it, and About visits re-check) must not
                    // re-pop the dialog.
                    self.update_dialog_decided = true;
                    if !on_about {
                        self.update_dialog =
                            Some(crate::app::UpdateDialog { info, dont_remind: false });
                    }
                }
                Task::none()
            }
            SettingsMsg::ShowAboutPage => {
                // Post-update relaunch (or the CCK_SETTINGS_TAB=about spawn): land the
                // user on About so the new version's "What's new" is immediately visible.
                self.activate_config_tab(crate::app::ConfigTab::About);
                Task::none()
            }
            SettingsMsg::SetNotifyUpdates(on) => {
                self.notify_updates = on;
                self.save_state();
                Task::none()
            }
            SettingsMsg::UpdateDialogRemindToggled(checked) => {
                if let Some(d) = self.update_dialog.as_mut() {
                    d.dont_remind = checked;
                }
                Task::none()
            }
            #[cfg_attr(target_os = "macos", allow(clippy::needless_return))]
            SettingsMsg::UpdateDialogNow => {
                // Apply the checkbox (Don't remind me again -> notify_updates OFF),
                // dismiss the dialog, then run the platform update flow using the
                // dialog's own captured `info`: the macOS one-click install, or the
                // Linux release-page link. Same flows the About buttons drive (no drift).
                let dismissed = self.dismiss_update_dialog();
                // Land on the About page so the install progress ("Installing...")
                // and the release notes are in view after the click.
                self.activate_config_tab(crate::app::ConfigTab::About);
                #[cfg(target_os = "macos")]
                {
                    // Install the update the dialog offered (mirrors `InstallUpdate`,
                    // but keyed off the dialog's own info so it can't drift from a
                    // concurrently-cleared status). A no-op without an artifact.
                    let Some(info) = dismissed.map(|d| d.info) else {
                        return Task::none();
                    };
                    if info.artifact.is_none() || self.update_installing {
                        return Task::none();
                    }
                    self.update_installing = true;
                    return Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || {
                                crate::update::install_macos(&info)
                            })
                            .await
                            .unwrap_or_else(|_| {
                                crate::update::InstallOutcome::Failed(
                                    "The update install could not run.".to_string(),
                                )
                            })
                        },
                        |outcome| {
                            cosmic::Action::App(Msg::Settings(SettingsMsg::UpdateInstallDone(
                                outcome,
                            )))
                        },
                    );
                }
                #[cfg(not(target_os = "macos"))]
                {
                    // Linux has no one-click yet: open the project releases page, the
                    // exact same destination as the About page's "Open releases" link.
                    let _ = dismissed;
                    crate::platform::services::open_uri(crate::update::RELEASES_URL);
                    Task::none()
                }
            }
            SettingsMsg::UpdateDialogLater => {
                // Apply the checkbox and dismiss for this session; no update action.
                let _ = self.dismiss_update_dialog();
                Task::none()
            }
            // The macOS block ends in `return`, but the `#[cfg(not)]` tail after it
            // makes the block a statement (not the arm's tail expr), so the return is
            // required there; Linux compiles only the tail. Mirrors `seed_outputs_mac`.
            #[cfg_attr(target_os = "macos", allow(clippy::needless_return))]
            SettingsMsg::InstallUpdate => {
                // One-click install (macOS only, no published Linux artifact yet).
                #[cfg(target_os = "macos")]
                {
                    let crate::update::UpdateStatus::Available(info) = self.update_status.clone()
                    else {
                        return Task::none();
                    };
                    if info.artifact.is_none() || self.update_installing {
                        return Task::none();
                    }
                    self.update_installing = true;
                    return Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || {
                                crate::update::install_macos(&info)
                            })
                            .await
                            .unwrap_or_else(|_| {
                                crate::update::InstallOutcome::Failed(
                                    "The update install could not run.".to_string(),
                                )
                            })
                        },
                        |outcome| {
                            cosmic::Action::App(Msg::Settings(SettingsMsg::UpdateInstallDone(
                                outcome,
                            )))
                        },
                    );
                }
                #[cfg(not(target_os = "macos"))]
                Task::none()
            }
            SettingsMsg::UpdateInstallDone(outcome) => {
                self.update_installing = false;
                match outcome {
                    crate::update::InstallOutcome::Staged => {
                        // The swap helper is armed and waiting for this app AND the
                        // daemon to fully exit before swapping /Applications and
                        // relaunching. Signal the daemon to quit (so its lock clears
                        // and the helper's wait completes), then exit this app.
                        #[cfg(target_os = "macos")]
                        crate::instance::signal_daemon_quit();
                        self.quit_now()
                    }
                    crate::update::InstallOutcome::Failed(reason) => {
                        // Surface the reason on the About page by folding it into the
                        // cached status; the page renders it inline.
                        self.update_status =
                            crate::update::UpdateStatus::Failed(reason);
                        self.update_about_nav_icon();
                        Task::none()
                    }
                }
            }
        }
    }

    /// Dismiss the launch-time update dialog (DRAGON-177), first applying its
    /// "Don't remind me again" checkbox: if checked, `notify_updates` is turned OFF
    /// and persisted (the About toggle and the checkbox are two views of the one
    /// setting). Returns the dismissed dialog (so "Update Now" can act on its
    /// `info`), or `None` when no dialog was open.
    fn dismiss_update_dialog(&mut self) -> Option<crate::app::UpdateDialog> {
        let dialog = self.update_dialog.take()?;
        if dialog.dont_remind && self.notify_updates {
            self.notify_updates = false;
            self.save_state();
        }
        Some(dialog)
    }

    /// Refresh the About nav entry's stored icon (DRAGON-175): a success-tinted
    /// download glyph when an update is available, the plain about glyph otherwise.
    /// Mirrors `update_health_nav_icon` so the EXPANDED rail matches the collapsed
    /// rail's live state.
    pub(in crate::app) fn update_about_nav_icon(&mut self) {
        let icon = if self.update_status.is_available() {
            cosmic::widget::icon::from_name(crate::app::settings::ABOUT_UPDATE_ICON)
                .icon()
                .class(cosmic::theme::Svg::custom(|theme| cosmic::widget::svg::Style {
                    color: Some(theme::success(theme)),
                }))
        } else {
            cosmic::widget::icon::from_name("help-about-symbolic").icon()
        };
        self.settings.nav.icon_set(self.settings.about, icon);
    }

    // `action_msg`/`handle_key` (+ the preview-modal key nesting) moved to
    // keyboard.rs; `WindowChromeMsg::KeyPressed` below calls `self.handle_key(..)`.

    /// Rebuild + apply the process-global theme for the current appearance settings
    /// (DRAGON-139). The ONE place setters and init route through, so the
    /// build/apply/revert logic — and its portability contract — lives in exactly
    /// one spot ([`theme::apply_appearance`]).
    pub(in crate::app) fn apply_appearance_task(&self) -> Task<cosmic::Action<Msg>> {
        theme::apply_appearance(
            self.appearance_use_system,
            self.appearance_mode,
            self.appearance_accent,
            self.appearance_roundness,
        )
    }
}
