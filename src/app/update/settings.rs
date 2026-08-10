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
            SettingsMsg::SetWindowRecompositing(b) => {
                // The master switch only; the individual aesthetic preferences are
                // preserved so re-enabling restores them exactly.
                self.window_recompositing = b;
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
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            SettingsMsg::SetResident(b) => {
                // Residency is a SEPARATE tray/menu-bar RESIDENT process (macOS
                // `crate::daemon`, Linux `crate::daemon_linux`, Windows `crate::daemon`
                // (DRAGON-237)), so flipping this drives the resident's lifecycle directly —
                // the tray item appears/disappears immediately, no relaunch. The spawn / quit
                // plumbing is portable (a detached bare launch early-branches to the resident;
                // the resident-lock holder is signalled to stop — SIGTERM on unix, a named
                // event on Windows); only the launch-at-login backend differs per OS
                // (SMAppService / XDG autostart / HKCU Run).
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
                    // DRAGON-465: the token is `instance::RESIDENT_ARG`, the same constant
                    // `main` reads through `instance::daemon_intent_from_args`. It was a raw
                    // literal here, which is exactly how the post-update relaunch came to be
                    // spawned bare: an argv-building launcher that owns its own copy of the
                    // rule can silently stop agreeing with the one that reads it.
                    // `self_exe` (DRAGON-510): the resident is meant to outlive every
                    // capture process, including this one, so an AppImage mount path
                    // would be exactly the wrong thing to hand it.
                    if let Ok(exe) = crate::util::self_exe() {
                        match std::process::Command::new(&exe)
                            .arg(crate::instance::RESIDENT_ARG)
                            .spawn()
                        {
                            Ok(child) => log::info!("resident on: spawned resident (pid {})", child.id()),
                            Err(e) => log::warn!("resident on: resident spawn failed: {e}"),
                        }
                    }
                } else {
                    // Turn OFF: ask the running resident to exit (SIGTERM the resident-lock
                    // holder) so the tray item disappears now; harmless no-op if none up.
                    if crate::instance::signal_daemon_quit() {
                        log::info!("resident off: signalled the resident to exit");
                    }
                }
                // Launch-at-login is driven by BOTH toggles now (DRAGON-296): the OS login
                // item is registered iff the tray is on AND autostart is on. Turning the tray
                // off unregisters it (the login item makes no sense with no resident to launch);
                // turning it on registers it when the user hasn't opted autostart off.
                self.reconcile_login_item()
            }
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            SettingsMsg::SetAutostartOnLogin(b) => {
                // The "Automatically start on login" toggle (DRAGON-296). Only ever visible
                // while the tray is on (the row is hidden otherwise), so this just persists the
                // preference and re-derives the OS login-item state from both toggles.
                // DRAGON-618: ignore the click while a Background-portal request is still
                // outstanding. The answer to the LAST request is not known yet, so acting on
                // a new one would race two registrations and settle the toggle from whichever
                // replied last. Guarded here rather than by making the row inert in `view`,
                // because this covers every route into the setting, not just that one widget.
                #[cfg(target_os = "linux")]
                if self.autostart_pending {
                    log::debug!("autostart toggle ignored: a portal request is still in flight");
                    return Task::none();
                }
                self.autostart_on_login = b;
                self.save_state();
                self.reconcile_login_item()
            }
            // DRAGON-618: the Flatpak Background-portal request has come back. Settle the
            // toggle from what the portal actually granted, never from what was asked for:
            // the user can decline the dialog, and the request can fail outright.
            #[cfg(target_os = "linux")]
            SettingsMsg::AutostartPortalSettled(result) => {
                self.autostart_pending = false;
                let granted = crate::platform::linux_autostart::settled_toggle(&result);
                match &result {
                    Ok(_) => log::info!("autostart portal settled: registered={granted}"),
                    Err(e) => log::warn!("autostart portal request failed: {e}"),
                }
                // DRAGON-625: carry the REASON to the settings row, so a toggle that springs
                // back explains itself instead of just refusing. Cleared on success, because
                // the notice describes one attempt and must not outlive it.
                self.autostart_notice = match &result {
                    Ok(_) => None,
                    Err(e) => Some(e.clone()),
                };
                // Only the AUTOSTART preference is settled here. `resident` is untouched: the
                // portal was asked about launching at login, not about the tray.
                //
                // DRAGON-625: and only when the tray is ON. `settled_preference` holds that
                // guard, which the file path always had and this path shipped without: with
                // the tray off the item is unregistered BY DESIGN, so the truthful `Ok(false)`
                // coming back was overwriting the user's preference and destroying "start on
                // login" whenever the tray was toggled off and on.
                if let Some(next) = crate::platform::linux_autostart::settled_preference(
                    self.resident,
                    self.autostart_on_login,
                    &result,
                ) {
                    self.autostart_on_login = next;
                    self.save_state();
                }
                Task::none()
            }
            SettingsMsg::SetColorPickerOpacity(v) => {
                self.color_picker_overlay_opacity = v.clamp(0.0, 1.0);
                self.save_state();
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
            // Windows (DRAGON-238): the off-thread encoder probe finished — store the list
            // (idempotent) and refresh the Health nav icon, since the HwEncoder row only
            // becomes knowable now. The video/Health pages re-render off the filled cache.
            #[cfg(windows)]
            SettingsMsg::EncodersProbed(list) => {
                self.encoders.finish_probe(list);
                self.update_health_nav_icon();
                Task::none()
            }
            // DRAGON-564: the off-thread tool-version probe finished. A version never
            // changes a row's severity, so no nav-icon refresh; the message arriving is
            // what re-renders the Health rows with each present binary's version.
            SettingsMsg::ToolVersionsProbed(list) => {
                self.settings.tool_versions = Some(list);
                self.settings.tool_versions_probing = false;
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
                // The Forget row clears BOTH source-type slots. Honest forget
                // (DRAGON-570): this only drops our replay tokens, so the next
                // capture prompts again; the portal's mode-2 permission-store
                // entry outlives the app, and the log names the full removal.
                self.pw_restore_token.clear();
                log::info!(
                    "screencast permission forgotten (both restore-token slots cleared). {}",
                    crate::app::portal::FORGET_SCREEN_ACCESS_NOTE
                );
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
                // Where the overlay editor cannot exist (Windows 10, DRAGON-427; a Linux
                // session with no layer shell, `lab/flatpak`) the appearance is not the
                // user's to choose — the row is hidden there (the owner's third live test;
                // it was inert-with-a-warning before) and this arm is unreachable from
                // the UI, but the write is refused rather than merely hidden. Persisting
                // `false` would put the NEXT process (which re-reads the setting through
                // `effective_preview_windowed`) one bug away from an overlay editor that
                // cannot draw, or has no surface to draw on.
                if !crate::platform::overlay_preview_available() {
                    return Task::none();
                }
                self.preview_windowed = b;
                self.save_state();
                Task::none()
            }
            // DRAGON-478. Nothing to re-lay-out by hand: the next view build reads the flag
            // through `Tb`, and every sizing consumer reads it through
            // `PreviewSurface::chrome_h`, so an editor already open picks up the new bar
            // height on its own.
            SettingsMsg::SetPreviewToolbarLabels(b) => {
                self.preview_toolbar_labels = b;
                self.save_state();
                Task::none()
            }
            // DRAGON-419. `set_enabled` FIRST, so the "turned on" session header lands before
            // the config write it causes — the file then shows its own enabling as the first
            // thing that happened, which is what makes a mid-session toggle readable.
            SettingsMsg::SetDebugLogging(b) => {
                crate::diag::set_enabled(b);
                self.debug_logging = b;
                self.save_state();
                Task::none()
            }
            // DRAGON-540. The text is copied exactly as the row shows it, so what the user
            // reads and what they paste are the same thing.
            SettingsMsg::CopyHealthLocation(text) => {
                // `copy_text_task`, not `copy_text`: on a compositor with no data-control the
                // detached worker cannot serve a selection, so this row reported "Copied!" and
                // put nothing on the clipboard. The task routes the write through this settings
                // window instead, which is focused by definition when the button was pressed.
                let write = crate::share::copy_text_task(&text);
                self.settings.health_copied = Some((text, std::time::Instant::now()));
                write
            }
            SettingsMsg::HealthCopyTick => Task::none(),
            #[cfg(target_os = "macos")]
            SettingsMsg::OpenPermissionsWindow => {
                if let Some(id) = self.permissions.window {
                    return window::gain_focus(id);
                }
                self.permissions.probe = permissions::probe_now();
                let (id, task) = permissions::open_permissions_window();
                self.permissions.window = Some(id);
                task
            }
            SettingsMsg::SetPreviewCopyOnExit(b) => {
                self.preview_copy_on_exit = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetPreviewSaveOriginals(b) => {
                self.preview_save_originals = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetPreviewAskToSave(b) => {
                self.preview_ask_to_save = b;
                self.save_state();
                Task::none()
            }
            // DRAGON-420: the Video Editor group's three. Each writes its OWN field — never
            // the image one beside it — which is exactly what the independence tests pin.
            SettingsMsg::SetPreviewVideoCopyOnExit(b) => {
                self.preview_video_copy_on_exit = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetPreviewVideoSaveOriginals(b) => {
                self.preview_video_save_originals = b;
                self.save_state();
                Task::none()
            }
            SettingsMsg::SetPreviewVideoAskToSave(b) => {
                self.preview_video_ask_to_save = b;
                self.save_state();
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
            SettingsMsg::SetAppearanceContrastBoost(b) => {
                self.appearance_contrast_boost = b;
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
                // DRAGON-617: a refusal notice belongs to the attempt that earned it. Clearing
                // on every BEGIN (including the cancelling toggle) means it never lingers over
                // a row the user has moved on from, and a fresh attempt starts with a clean
                // helper line rather than last time's complaint.
                self.settings.rebind_refused = None;
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
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            SettingsMsg::SetCaptureHotkey(slot, spec) => {
                // Persist the raw text as typed. The daemon falls back to nothing if it
                // can't be parsed, so a half-typed spec is never dangerous.
                //
                // PrintScreen is recordable despite the daemon owning it: while the chord
                // recorder is armed, `sub_hotkey_suspend_ping` pings the daemon (SIGUSR2 on
                // macOS, the named SUSPEND event on Windows) every ~1s, which UN-registers
                // its capture hotkeys for ~3s (extended per ping, auto-resumed on expiry —
                // see `crate::daemon` SuspendWindow). So a PrintScreen pressed during
                // recording reaches THIS app and is captured like any other key, then the
                // daemon re-registers a couple of seconds after recording ends.
                //
                // DRAGON-452: the six slots share ONE keyboard, and only one of them can win
                // an OS registration, so a chord already held by ANOTHER slot must not be
                // accepted in silence. We STEAL rather than refuse: the recorded row gets the
                // chord and the previous owner is cleared, with both rows saying so. Refusing
                // would leave the user hunting for which of six rows already holds the chord;
                // stealing is the outcome they asked for, and the notice plus the loser's now
                // "Unbound" button make the cost visible and one click to undo.
                //
                // Only a chord being SET can conflict: clearing a row (the "x" / the reset)
                // frees a chord and can never take one. The check is the pure
                // `capture_hotkey_conflict`, which compares NORMALIZED chords, so a different
                // modifier order or case is still the same chord.
                let mut notice = crate::app::settings::CaptureHotkeyNotice {
                    slot,
                    taken_from: None,
                    os_refused: false,
                };
                let conflict = {
                    let specs = self.capture_hotkey_specs();
                    crate::shortcuts::capture_hotkey_conflict(&specs, slot.index(), &spec)
                };
                if let Some(loser) = conflict.map(|i| crate::app::CaptureHotkeySlot::ALL[i]) {
                    *self.capture_hotkey_slot_mut(loser) = String::new();
                    notice.taken_from = Some(loser);
                    log::info!(
                        "capture hotkey \"{spec}\" moved to {} and taken from {} (it was bound to both)",
                        slot.label(),
                        loser.label()
                    );
                }
                // DRAGON-452, the SECOND way a hotkey can silently do nothing: another APP
                // owns the chord globally, so `RegisterHotKey` refuses it. From the keyboard
                // that is indistinguishable from our own duplicate, so ask the OS now and say
                // so on the row. NO IPC is involved: the daemon is a separate process, and
                // this is a transient registration in THIS process that is released
                // immediately. It is asked exactly here, at record time, because the chord
                // recorder has the daemon's own hotkeys SUSPENDED (see
                // `sub_hotkey_suspend_ping`), which is the one moment our own registrations
                // cannot answer for someone else's. macOS has no equivalent probe: its daemon
                // registers through `global_hotkey`'s Carbon manager, and standing one up
                // inside this GUI process just to test a chord is not a thing we can verify
                // here, so mac gets the duplicate check + rows and leaves this to the daemon
                // log.
                //
                // No Windows-version gate: `RegisterHotKey`/`UnregisterHotKey` behave the
                // same all the way back through Windows 10, and the answer needs no IPC, so
                // it holds when the settings window is its own process (DRAGON-427's Win10
                // settings split) exactly as it does in-process.
                #[cfg(windows)]
                if !crate::daemon::hotkey_spec_is_cleared(&spec)
                    && let Err(reason) = crate::daemon::chord_is_free(&spec)
                {
                    notice.os_refused = true;
                    log::warn!("capture hotkey \"{spec}\" for {}: {reason}", slot.label());
                }
                // Nothing to report about a CLEARED row, so a clear also wipes a previous
                // notice: the rows go back to plain labels rather than keeping stale advice
                // about an edit the user has since undone.
                self.settings.capture_hotkey_notice =
                    (!crate::daemon::hotkey_spec_is_cleared(&spec)).then_some(notice);
                // DRAGON-295: route the spec to the slot the row edits (All In One / Active
                // Window / Active Monitor); each is an independent persisted spec.
                *self.capture_hotkey_slot_mut(slot) = spec;
                // Persist the new spec. On Windows this is the WHOLE job (DRAGON-259): the
                // running daemon's config-mtime poll notices the changed spec and re-registers
                // the global hotkey IN PLACE — no quit, no respawn. The old code SIGTERM'd +
                // respawned the daemon here, but the Windows single-instance mutex has no
                // acquire retry, so the respawn raced the still-terminating old daemon, found
                // the mutex held, and self-exited — the tray VANISHED on every hotkey change.
                self.save_state();
                // macOS keeps the restart-the-daemon path: its Carbon hotkey can't be
                // re-registered from this settings process, and its lock retry makes the
                // respawn race-safe. Restart when the new spec PARSES (a recorded chord or the
                // restore-default) OR is CLEARED (the "x" — the daemon should come up with no
                // global hotkey); a merely-invalid spec leaves the daemon on its last-good key
                // rather than resetting it out from under the user.
                #[cfg(target_os = "macos")]
                {
                    let changed = self.capture_hotkey_slot(slot);
                    let apply = crate::daemon::hotkey_spec_is_valid(changed)
                        || crate::daemon::hotkey_spec_is_cleared(changed);
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
                }
                Task::none()
            }
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            SettingsMsg::BeginCaptureHotkeyRebind(slot) => {
                // Toggle: clicking the row that's already recording THIS slot cancels it;
                // clicking a different row moves the recorder to it. Clear any in-app rebind
                // so the two capture modes are never armed at once.
                self.settings.rebinding = None;
                // DRAGON-452: the previous edit's notice belongs to the previous edit. Drop it
                // as soon as a row is armed, so what the rows say always describes the LAST
                // recorded chord and never an older one.
                self.settings.capture_hotkey_notice = None;
                self.settings.capture_hotkey_rebinding =
                    if self.settings.capture_hotkey_rebinding == Some(slot) {
                        None
                    } else {
                        Some(slot)
                    };
                // Just armed: send an IMMEDIATE suspend ping so the daemon un-registers its
                // hotkeys NOW (PrintScreen becomes recordable) rather than after the first
                // ~1s timer tick. The `sub_hotkey_suspend_ping` subscription keeps it
                // extended thereafter; the daemon auto-resumes once pings stop.
                if self.settings.capture_hotkey_rebinding.is_some() && self.resident {
                    crate::instance::signal_daemon_suspend_hotkey();
                }
                Task::none()
            }
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            SettingsMsg::SuspendDaemonHotkeyPing => {
                // Fire-and-forget: keep the daemon's hotkeys suspended while recording.
                // Only meaningful with a running daemon; a no-op otherwise.
                if self.settings.capture_hotkey_rebinding.is_some() && self.resident {
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
                // On a build with no update channel (a Flatpak, DRAGON-561) this message
                // should be unreachable: every sender is gated on `channel_available`
                // and the About page offers no check button. A stray one must still
                // never fetch update.json, so the gate holds here too.
                if !crate::update::channel_available() {
                    return Task::none();
                }
                // Non-blocking: the curl fetch runs on a detached worker; the result
                // lands back as `UpdateChecked`. Mark "Checking" so the About page
                // shows progress and a repeat click is a no-op while in flight.
                if matches!(self.update_status, crate::update::UpdateStatus::Checking) {
                    return Task::none();
                }
                self.update_status = crate::update::UpdateStatus::Checking;
                // DRAGON-499: ONE detached worker for the whole job, off the executor's
                // blocking pool. Every settings mint fires this check (see
                // `window_chrome`), so it is the likeliest job to still be running when
                // the window closes, and a `spawn_blocking` one pins the runtime's drop
                // on the main thread until it finishes: a slow network turned closing
                // settings into waiting for curl. See `app::background`.
                Task::perform(
                    off_thread(|| {
                        // Run the fetch, then hold "Checking..." until the interactive
                        // floor (DRAGON-177) so an instant result doesn't flip back and
                        // read as broken. Both halves on this one thread, so neither
                        // touches the UI thread; the fetch itself is unslowed.
                        let started = std::time::Instant::now();
                        let status = crate::update::check_now();
                        let remainder = crate::update::check_floor_remainder(
                            started.elapsed(),
                            crate::update::INTERACTIVE_CHECK_FLOOR,
                        );
                        if !remainder.is_zero() {
                            std::thread::sleep(remainder);
                        }
                        status
                    }),
                    |status| {
                        // A worker that died without answering reads exactly as the
                        // `JoinError` arm it replaces.
                        let status = status.unwrap_or_else(|| {
                            crate::update::UpdateStatus::Failed(
                                "The update check could not run.".to_string(),
                            )
                        });
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
                // The OTHER route onto About (the nav rail's `SetConfigTab`) fires the
                // same message, so a deep link and a click behave identically. A no-op
                // on every build with an update channel.
                self.update_settings(SettingsMsg::FetchReleaseNotes)
            }
            SettingsMsg::FetchReleaseNotes => {
                // DRAGON-605: the About page is showing on a build with no update channel
                // (a Flatpak), which means no check will ever fill in "What's new". Fetch
                // the notes on their own.
                //
                // NOT an update check by the back door. The result is a `ReleaseNotes`,
                // which carries no artifact at all, so it can produce no install button;
                // it never becomes an `UpdateStatus`, so it cannot tint the nav rail or
                // raise the DRAGON-177 dialog; and it never writes the manifest cache.
                // See `update.rs`'s module doc for the full reasoning, including why the
                // notes are fetched rather than baked into the build.
                if crate::update::notes_source() != crate::update::NotesSource::OwnFetch {
                    // A build WITH a channel already parsed these out of the manifest its
                    // check fetched; a second request would buy nothing.
                    return Task::none();
                }
                if self.release_notes_fetched {
                    return Task::none();
                }
                // Latched BEFORE the fetch, so re-visiting About while one is in flight
                // (or after one came back empty) costs no further requests.
                self.release_notes_fetched = true;
                log::info!("release notes: fetching (this build has no update channel)");
                Task::perform(
                    // Detached worker, off the executor's blocking pool, for the reason
                    // `CheckForUpdates` uses one (DRAGON-499): a slow network must never
                    // pin the runtime's drop when the settings window closes.
                    off_thread(crate::update::fetch_release_notes),
                    |notes| {
                        // A worker that died without answering reads as "no notes", the
                        // same quiet outcome every other failure has.
                        cosmic::Action::App(Msg::Settings(SettingsMsg::ReleaseNotesFetched(
                            notes.flatten(),
                        )))
                    },
                )
            }
            SettingsMsg::ReleaseNotesFetched(notes) => {
                // Parse the markdown once, here, exactly as `UpdateChecked` does and for
                // the same reason: the view cannot hold the borrow `markdown::view` needs.
                // `update_status` is deliberately NOT touched, so the About page keeps
                // offering no update controls at all.
                if let Some(notes) = notes {
                    self.update_notes = Some((
                        notes.version,
                        cosmic::widget::markdown::Content::parse(&notes.notes),
                    ));
                }
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
            SettingsMsg::UpdateDialogNow => {
                // Apply the checkbox (Don't remind me again -> notify_updates OFF),
                // dismiss the dialog, then run the platform update flow using the
                // dialog's own captured `info`: the one-click install where this build has
                // one, the release-page link where it does not. Same flows the About buttons
                // drive (no drift).
                let dismissed = self.dismiss_update_dialog();
                // Land on the About page so the install progress ("Installing...")
                // and the release notes are in view after the click.
                self.activate_config_tab(crate::app::ConfigTab::About);
                if !crate::update::one_click_install_available() {
                    // Open the project releases page, the exact same destination as the
                    // About page's "Open releases" link. On Linux this is the ZIP (BIN)
                    // build, which has no install location we own (DRAGON-532).
                    crate::platform::services::open_uri(crate::update::RELEASES_URL);
                    return Task::none();
                }
                // Install the update the dialog offered (mirrors `InstallUpdate`, but keyed
                // off the dialog's own info so it can't drift from a concurrently-cleared
                // status). A no-op without an artifact.
                let Some(info) = dismissed.map(|d| d.info) else {
                    return Task::none();
                };
                self.start_update_install(info)
            }
            SettingsMsg::UpdateDialogLater => {
                // Apply the checkbox and dismiss for this session; no update action.
                let _ = self.dismiss_update_dialog();
                Task::none()
            }
            // DRAGON-482: the Cloud Accounts page owns its own update body, next to the
            // state machine it drives and the pure decisions those transitions are made of
            // (`settings::pages::cloud`). The same split `update_preview` has, and for the
            // same reason: every one of these arms is a step of ONE flow.
            SettingsMsg::Cloud(msg) => self.update_cloud(msg),
            SettingsMsg::InstallUpdate => {
                // One-click install: macOS (dmg swap), Windows (silent MSI, DRAGON-287), or
                // a Linux AppImage (in-place file swap, DRAGON-532). The button that sends
                // this is only drawn where `one_click_install_available`, so a build without
                // one never gets here.
                let crate::update::UpdateStatus::Available(info) = self.update_status.clone()
                else {
                    return Task::none();
                };
                self.start_update_install(info)
            }
            SettingsMsg::UpdateInstallDone(outcome) => {
                self.update_installing = false;
                match outcome {
                    crate::update::InstallOutcome::Staged => {
                        // A detached helper is armed and waiting for this app AND the daemon
                        // to fully exit before finishing (mac: swap /Applications; Windows:
                        // msiexec; Linux: the file is already swapped, so it only relaunches)
                        // and bringing us back. Signal the daemon to quit, so its lock clears
                        // and the helper's wait completes, then exit this app.
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

    /// Start the platform's one-click install for `info`, off-thread, and mark the UI busy.
    ///
    /// ONE body for both entry points, the About page's install button (`InstallUpdate`) and
    /// the launch dialog's "Install now" (`UpdateDialogNow`), which used to carry a
    /// per-platform copy each: four near-identical blocks that had to be kept in step by
    /// hand. Callers do the guarding they need first (this returns a no-op task if there is
    /// nothing to install or an install is already running).
    fn start_update_install(
        &mut self,
        info: crate::update::UpdateInfo,
    ) -> Task<cosmic::Action<Msg>> {
        if info.artifact.is_none() || self.update_installing {
            return Task::none();
        }
        self.update_installing = true;
        // DRAGON-499: detached, like every other background job in the app.
        Task::perform(
            off_thread(move || crate::update::install(&info)),
            |outcome| {
                let outcome = outcome.unwrap_or_else(|| {
                    crate::update::InstallOutcome::Failed(
                        "The update install could not run.".to_string(),
                    )
                });
                cosmic::Action::App(Msg::Settings(SettingsMsg::UpdateInstallDone(outcome)))
            },
        )
    }

    /// macOS/Windows (DRAGON-295): the persisted spec string for one of the three global
    /// capture-hotkey slots. Central accessor so the message handler + settings row + the
    /// daemon-restart decision all read the same field per slot. Read on BOTH daemon OSes
    /// since DRAGON-452 (the duplicate check reads every slot through
    /// [`Self::capture_hotkey_specs`]); macOS additionally reads it for the daemon-restart
    /// decision.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(in crate::app) fn capture_hotkey_slot(&self, slot: crate::app::CaptureHotkeySlot) -> &str {
        match slot {
            crate::app::CaptureHotkeySlot::AllInOne => &self.capture_hotkey,
            crate::app::CaptureHotkeySlot::ActiveWindow => &self.capture_active_window_hotkey,
            crate::app::CaptureHotkeySlot::ActiveMonitor => &self.capture_active_monitor_hotkey,
            crate::app::CaptureHotkeySlot::AllInOneNoEditor => &self.capture_no_editor_hotkey,
            crate::app::CaptureHotkeySlot::ActiveWindowNoEditor => {
                &self.capture_active_window_no_editor_hotkey
            }
            crate::app::CaptureHotkeySlot::ActiveMonitorNoEditor => {
                &self.capture_active_monitor_no_editor_hotkey
            }
            crate::app::CaptureHotkeySlot::ColorPicker => &self.color_picker_hotkey,
        }
    }

    /// Mutable counterpart to [`Self::capture_hotkey_slot`] (DRAGON-295).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(in crate::app) fn capture_hotkey_slot_mut(
        &mut self,
        slot: crate::app::CaptureHotkeySlot,
    ) -> &mut String {
        match slot {
            crate::app::CaptureHotkeySlot::AllInOne => &mut self.capture_hotkey,
            crate::app::CaptureHotkeySlot::ActiveWindow => &mut self.capture_active_window_hotkey,
            crate::app::CaptureHotkeySlot::ActiveMonitor => &mut self.capture_active_monitor_hotkey,
            crate::app::CaptureHotkeySlot::AllInOneNoEditor => &mut self.capture_no_editor_hotkey,
            crate::app::CaptureHotkeySlot::ActiveWindowNoEditor => {
                &mut self.capture_active_window_no_editor_hotkey
            }
            crate::app::CaptureHotkeySlot::ActiveMonitorNoEditor => {
                &mut self.capture_active_monitor_no_editor_hotkey
            }
            crate::app::CaptureHotkeySlot::ColorPicker => &mut self.color_picker_hotkey,
        }
    }

    /// DRAGON-452: every capture-hotkey slot's current spec, in [`CaptureHotkeySlot::ALL`]
    /// order — the input the pure [`crate::shortcuts::capture_hotkey_conflict`] check reads.
    /// One accessor so the check can never see a different set of slots than the rows do.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(in crate::app) fn capture_hotkey_specs(&self) -> [&str; 7] {
        crate::app::CaptureHotkeySlot::ALL.map(|slot| self.capture_hotkey_slot(slot))
    }

    /// DRAGON-628: read the OS login item when the settings window opens, so the
    /// "Automatically start on login" row shows what the machine will really do.
    ///
    /// **This READS and never writes.** No file, no registry value, no portal request, no
    /// correction of the stored preference. Opening a window is not a request for anything,
    /// and every write this could have done turns out to be wrong on some platform:
    ///
    /// * A Flatpak's registration goes through the Background portal, so a "reconcile" there
    ///   IS a `RequestBackground` call, asynchronous and possibly interactive. On COSMIC,
    ///   which ships no Background backend, it fails, sets the notice and flips the toggle
    ///   off, which would have happened every single time the window opened.
    /// * An unbundled macOS dev binary cannot register at all, so `set` always errors, and
    ///   the failure path would have rewritten the user's stored preference to `false` on
    ///   every open.
    /// * A registration that is absent may have been removed on purpose. Re-creating it
    ///   because a window opened overrides a choice we have no evidence was withdrawn.
    ///
    /// The REPAIR lives in the resident daemons instead
    /// (`platform::autostart_repair_at_daemon_start`), which is a better home for it anyway:
    /// the daemon is the thing autostart exists to launch. So this path is purely honest
    /// display, and it cannot have side effects to get wrong.
    ///
    /// Portable on the outside: the body branches by `cfg`, so both settings-window mints
    /// (`App::open_settings` for the in-process convert, `OpenSettingsAtStartup` for a
    /// standalone `--settings` process) call it with no `cfg` of their own. BOTH need it.
    /// That is not belt and braces: the Cloud page's reload was wired to only one of them and
    /// the other listed no accounts at all, which on macOS is the only mint there is.
    pub(in crate::app) fn autostart_settings_opened(&mut self) {
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        {
            let registered =
                crate::platform::autostart_registration().map(|r| r.is_live());
            self.autostart_registered = registered;
            // Nothing about a login item we cannot honour may be silent (DRAGON-628). The
            // debug log is where a support question gets answered, and "the row says off
            // although the setting says on" is unanswerable without this line.
            match crate::platform::autostart_row(self.autostart_on_login, registered) {
                crate::platform::AutostartRow::Agrees(_) => {}
                crate::platform::AutostartRow::Unobservable(pref) => log::debug!(
                    "autostart: this build cannot read the login item; the row shows the \
                     stored preference ({pref})"
                ),
                crate::platform::AutostartRow::Disagrees { shown, preference } => log::warn!(
                    "autostart: the login item is registered={shown} but the setting says \
                     {preference}; the row shows what the next login will really do"
                ),
            }
        }
    }

    /// Reconcile the OS "launch at login" item with the current settings (DRAGON-296).
    ///
    /// The single place both the "System tray icon" (`resident`) handler and the
    /// "Automatically start on login" (`autostart_on_login`) handler route through, so the
    /// desired login-item state is derived from ONE rule in ONE spot: the item is registered
    /// iff `resident && autostart_on_login` (a resident to launch, and the user opted in),
    /// and unregistered otherwise. Platform-agnostic on the outside — each OS's login-item
    /// backend hides behind a `is_enabled()`/`set(bool)` seam with the SAME signature
    /// (macOS `SMAppService`, Linux XDG autostart `.desktop`, Windows HKCU `Run`), so this
    /// body is byte-identical across platforms bar the `#[cfg]`-selected module path.
    /// Best-effort: only writes when the current state differs, and never panics.
    ///
    /// **A failed write takes the toggle back down with it** (DRAGON-618). Registering can
    /// genuinely fail, and a Flatpak is the case that made this matter: writing the host's
    /// autostart entry needs a filesystem grant a shippable manifest does not carry, so the
    /// write returns a permission error. Leaving `autostart_on_login` on after that would
    /// show the user a setting that is not true, which is the same silent lie the old
    /// wrong-directory bug told, just one layer up. So the toggle is re-derived from what the
    /// OS actually reports and persisted, and the user sees it fall back.
    ///
    /// Only when `resident` is on, because that is the only case where the login item's
    /// absence proves anything: with the tray off the item is unregistered BY DESIGN, and the
    /// user's autostart preference is being kept for when they turn the tray back on.
    ///
    /// **Only a user TOGGLE reaches this** (DRAGON-628). Nothing else in the app reconciles:
    /// opening the settings window only reads (`autostart_settings_opened`), and the
    /// unprompted repair of a stale registration belongs to the resident daemons
    /// (`platform::autostart_repair_at_daemon_start`). So the preference correction below is
    /// always answering a click somebody just made.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub(in crate::app) fn reconcile_login_item(&mut self) -> Task<cosmic::Action<Msg>> {
        // The ONE expression of "should there be a login item", shared with the daemons.
        let want = crate::platform::autostart_wanted(self.resident, self.autostart_on_login);
        // Each seam exposes `is_enabled()` (query the OS/file/registry) + `set(bool)`
        // (register/unregister), returning an honest `Result<(), String>`. Cfg-select the
        // one for this OS; the reconcile logic below is shared.
        #[cfg(target_os = "macos")]
        use crate::platform::mac::login_item as login;
        #[cfg(target_os = "linux")]
        use crate::platform::linux_autostart as login;
        #[cfg(target_os = "windows")]
        use crate::platform::windows_autostart as login;

        // DRAGON-618: a Flatpak does not own the host's autostart directory, so registration
        // is the Background portal's job rather than a file write. That makes it ASYNC and
        // possibly interactive, which is why this function returns a `Task` at all; every
        // other platform and package kind still takes the synchronous path below and is
        // byte-identical to what it was.
        //
        // No `is_enabled()` early-return on this path, deliberately. The portal writes the
        // entry on the host where a sandbox cannot see it, so `is_enabled()` has no honest
        // answer here and is never asked. Skipping the check costs one portal round trip per
        // user click, and the portal remembers its decision per app id, so a repeat request
        // for an already-granted app is normally silent.
        #[cfg(target_os = "linux")]
        if login::autostart_mechanism(crate::util::package_kind())
            == login::Mechanism::BackgroundPortal
        {
            // DRAGON-628: unobservable here by construction, so the row falls back to the
            // preference, which `settled_preference` fills in from the portal's real answer
            // when this request comes back.
            self.autostart_registered = None;
            self.autostart_pending = true;
            return Task::perform(login::request_background_autostart(want), |r| {
                cosmic::Action::App(Msg::Settings(SettingsMsg::AutostartPortalSettled(r)))
            });
        }

        if login::is_enabled() == want {
            // Already in the desired state; no write. The probe just answered, so record it
            // rather than asking the OS the same question twice.
            self.autostart_registered = Some(want);
            return Task::none();
        }
        match login::set(want) {
            Ok(()) => log::info!(
                "login item reconciled: {} (resident={}, autostart={})",
                if want { "registered" } else { "unregistered" },
                self.resident,
                self.autostart_on_login
            ),
            Err(e) => {
                log::warn!("login item reconcile ({want}) failed: {e}");
                // Re-derive the toggle from what the OS really reports, so the settings row
                // stops claiming something the write did not achieve. See the doc above for
                // why this is gated on `resident`, and for why only a click gets here at all.
                if self.resident {
                    let actual = login::is_enabled();
                    if self.autostart_on_login != actual {
                        self.autostart_on_login = actual;
                        self.save_state();
                        log::info!(
                            "autostart toggle corrected to {actual}: the login item write did \
                             not land"
                        );
                    }
                }
            }
        }
        // DRAGON-628: the row renders this, so the ONE function in the app that can change
        // the login item is the one that keeps the displayed state honest. Re-probed rather
        // than assumed `want`, because the write above may have failed.
        self.autostart_registered = crate::platform::autostart_registration().map(|r| r.is_live());
        Task::none()
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
            crate::widgets::icons::handle(crate::app::settings::ABOUT_UPDATE_ICON)
                .icon()
                .class(cosmic::theme::Svg::custom(|theme| cosmic::widget::svg::Style {
                    color: Some(theme::success(theme)),
                }))
        } else {
            crate::widgets::icons::handle("help-about-symbolic").icon()
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
            self.appearance_contrast_boost,
        )
    }
}
