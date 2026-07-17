//! `WindowChromeMsg` handling — CSD window controls, close paths, quit.
//! Split from `application.rs` (DRAGON-115).

use super::super::*;
// `core()` is a `cosmic::Application` trait method — bring the trait into scope.
use cosmic::Application as _;

impl App {
    pub(in crate::app) fn update_window_chrome(&mut self, message: WindowChromeMsg) -> Task<cosmic::Action<Msg>> {
        match message {
            #[cfg(target_os = "linux")]
            WindowChromeMsg::Output(ev, output) => self.on_output(*ev, output),
            #[cfg(not(target_os = "linux"))]
            WindowChromeMsg::SeedOutputs => self.seed_outputs_mac(),
            #[cfg(not(target_os = "linux"))]
            WindowChromeMsg::OverlayOpened(id, attempt) => self.configure_overlay(id, attempt),
            #[cfg(target_os = "macos")]
            WindowChromeMsg::PreviewOpened(id, attempt) => self.finalize_preview_window(id, attempt),
            #[cfg(target_os = "macos")]
            WindowChromeMsg::PreviewOverlayOpened(id, pos, size, attempt) => {
                self.finalize_preview_overlay(id, pos, size, attempt)
            }
            #[cfg(not(target_os = "linux"))]
            WindowChromeMsg::SeedOverlays => self.seed_overlays_mac(),
            WindowChromeMsg::OpenGear => self.open_settings(),
            WindowChromeMsg::OpenUrl(url) => {
                crate::platform::services::open_uri(url);
                Task::none()
            }
            WindowChromeMsg::OpenUrlOwned(url) => {
                crate::platform::services::open_uri(&url);
                Task::none()
            }
            WindowChromeMsg::Ignore => Task::none(),
            WindowChromeMsg::KeyPressed(modifiers, key) => self.handle_key(modifiers, key),
            WindowChromeMsg::KeyReleased(modifiers, key) => {
                // Push-to-talk release: re-mute the mic when the held mic key is let go.
                if self.ptt_held
                    && self
                        .keymap
                        .get(crate::shortcuts::Action::RecordToggleMic)
                        .is_some_and(|sc| sc.matches(modifiers, &key))
                {
                    self.ptt_held = false;
                    self.log_audio_toggle(crate::record::AudioChannel::Mic, false);
                }
                Task::none()
            }
            WindowChromeMsg::SetConfigTab(entity) => {
                self.settings.nav.activate(entity);
                // Start/stop the full-chain capture for the Audio page's sensitivity bar.
                self.sync_mic_input();
                // DRAGON-187: navigating TO the About tab NO LONGER re-fires an update
                // check. The launch-time check (settings window open) already refreshes
                // the notes/version; the manual "Check for updates" button remains the
                // way to re-check on demand, so opening About no longer costs a request.
                Task::none()
            }
            WindowChromeMsg::RequestReset(scope) => {
                self.settings.pending_reset = Some(scope);
                Task::none()
            }
            WindowChromeMsg::CancelReset => {
                self.settings.pending_reset = None;
                Task::none()
            }
            WindowChromeMsg::ConfirmReset => {
                if let Some(scope) = self.settings.pending_reset.take() {
                    match scope {
                        ResetScope::Factory => self.factory_reset(),
                        ResetScope::Page(tab) => self.reset_page(tab),
                    }
                    // A reset can change the appearance overrides (both Factory and the
                    // General page carry them; DRAGON-139) — re-apply the theme live so
                    // the running app follows the reset without a relaunch.
                    return self.apply_appearance_task();
                }
                Task::none()
            }
            WindowChromeMsg::ConfigWindowOpened(id) => {
                self.refresh_audio_devices();
                self.sync_mic_input(); // live sensitivity bar if opened to the Audio page (manual)
                if std::env::var_os("CCK_MIC_TEST").is_some() {
                    self.mic_test_modal_open = true;
                    self.sync_mic_input();
                }
                let title_task = self.set_window_title(settings::WINDOW_TITLE.to_string(), id);
                // macOS (DRAGON-135): once the async title lands, centre the native
                // traffic lights against the CSD header (polled by title).
                #[cfg(target_os = "macos")]
                return Task::batch([
                    window::gain_focus(id),
                    title_task,
                    Task::done(cosmic::Action::App(Msg::WindowChrome(
                        WindowChromeMsg::MacCenterTitlebar(settings::WINDOW_TITLE, 0),
                    ))),
                ]);
                #[cfg(not(target_os = "macos"))]
                title_task
            }
            WindowChromeMsg::ConfigWindowResized(id, w, h) => {
                // Remember the settings window's size so it reopens at the size it was
                // closed at (persisted on close).
                if Some(id) == self.settings.window && w >= 1.0 && h >= 1.0 {
                    self.settings_size = Some((w.round() as u32, h.round() as u32));
                }
                // Learn the preview overlay's monitor size (needed for `--preview`, which
                // opens on the active output before its size is known). The returned task
                // clears the windowed preview's transient max_size hint on first configure.
                self.preview_resized(id, w.round() as u32, h.round() as u32)
            }
            WindowChromeMsg::ConfigWindowDrag => match self.settings.window {
                Some(id) => window::drag(id),
                None => Task::none(),
            },
            WindowChromeMsg::ConfigWindowMaximize => match self.settings.window {
                Some(id) => window::toggle_maximize(id),
                None => Task::none(),
            },
            WindowChromeMsg::ConfigWindowMinimize => match self.settings.window {
                Some(id) => window::minimize(id, true),
                None => Task::none(),
            },
            WindowChromeMsg::PermissionsWindowOpened(id) => {
                let title_task = self.set_window_title(permissions::WINDOW_TITLE.to_string(), id);
                // macOS (DRAGON-135): same traffic-light centering as the settings window.
                #[cfg(target_os = "macos")]
                return Task::batch([
                    window::gain_focus(id),
                    title_task,
                    Task::done(cosmic::Action::App(Msg::WindowChrome(
                        WindowChromeMsg::MacCenterTitlebar(permissions::WINDOW_TITLE, 0),
                    ))),
                ]);
                #[cfg(not(target_os = "macos"))]
                title_task
            }
            WindowChromeMsg::ClosePermissionsWindow => match self.permissions.window {
                Some(id) => window::close(id),
                None => Task::none(),
            },
            WindowChromeMsg::PermissionsWindowDrag => match self.permissions.window {
                Some(id) => window::drag(id),
                None => Task::none(),
            },
            WindowChromeMsg::ToggleConfigNav => {
                self.settings.nav_open = !self.settings.nav_open;
                Task::none()
            }
            WindowChromeMsg::ConfigSearchActivate => {
                // Triggered by the header search button or Ctrl+F / Ctrl+K; only
                // meaningful while the settings window is open.
                if self.settings.window.is_none() {
                    return Task::none();
                }
                self.settings.search_active = true;
                let id = self.settings.search_id.clone();
                // Focus the field, and select any existing text so a new query
                // replaces it. (Applied after the view rebuilds with the input.)
                Task::batch([
                    widget::text_input::focus(id.clone()),
                    widget::text_input::select_all(id),
                ])
            }
            WindowChromeMsg::ConfigSearchInput(q) => {
                self.settings.search = q;
                Task::none()
            }
            WindowChromeMsg::ConfigSearchClear => {
                self.settings.search.clear();
                self.settings.search_active = false;
                Task::none()
            }
            WindowChromeMsg::CloseConfigWindow => {
                if let Some(id) = self.settings.window {
                    window::close(id)
                } else {
                    Task::none()
                }
            }
            #[cfg(target_os = "macos")]
            WindowChromeMsg::MacCenterTitlebar(title, attempt) => {
                // Same poll shape as `finalize_preview_window`: the NSWindow is found
                // by title, and the title is set by an async task that may not have
                // landed yet — retry briefly, then give up loudly.
                const MAX_ATTEMPTS: u8 = 30;
                const RETRY_MS: u64 = 40;
                if crate::platform::mac::window::center_titlebar_buttons(title) {
                    Task::none()
                } else if attempt >= MAX_ATTEMPTS {
                    log::warn!(
                        "titlebar centering never matched '{title}' after {MAX_ATTEMPTS} \
                         attempts; the traffic lights stay at their default position"
                    );
                    Task::none()
                } else {
                    Task::perform(
                        async move {
                            tokio::time::sleep(std::time::Duration::from_millis(RETRY_MS)).await;
                        },
                        move |()| {
                            cosmic::Action::App(Msg::WindowChrome(
                                WindowChromeMsg::MacCenterTitlebar(title, attempt + 1),
                            ))
                        },
                    )
                }
            }
            #[cfg(target_os = "macos")]
            WindowChromeMsg::SettingsFocusPoke => {
                // A blocked second `--settings` launch asked us to come forward. The
                // poke tick fires every 300ms while the pane is open; only act when
                // the file is present (removing it consumes the request).
                let poke = crate::platform::compositor::settings_focus_poke_path();
                if std::fs::metadata(&poke).is_ok() {
                    let _ = std::fs::remove_file(&poke);
                    if let Some(id) = self.settings.window {
                        // Activation from the OWNING process with a visible window
                        // sticks (winit's gain_focus does activate + makeKey).
                        return window::gain_focus(id);
                    }
                }
                Task::none()
            }
            #[cfg(target_os = "macos")]
            WindowChromeMsg::CursorEnteredWindow(id) => {
                // Keyboard focus follows the pointer across the per-display capture
                // overlays, so Escape / shortcuts work on whichever display the user
                // moved to. Selection phase only: during countdown/recording the
                // overlays are click-through and stealing focus from the app being
                // recorded would be actively harmful. Non-overlay windows (settings,
                // preview, permissions) keep normal click-to-focus.
                if self.countdown.is_none()
                    && self.recording.is_none()
                    && !self.capture_live
                    && self.outputs.iter().any(|o| o.id == id)
                {
                    window::gain_focus(id)
                } else {
                    Task::none()
                }
            }
            WindowChromeMsg::WindowCloseRequested(id) => {
                if Some(id) == self.settings.window || Some(id) == self.permissions.window {
                    window::close(id)
                } else if self.is_preview_window(id) {
                    self.update_preview(PreviewMsg::Cancel)
                } else {
                    Task::none()
                }
            }
            WindowChromeMsg::WindowClosed(id) => {
                if Some(id) == self.settings.window {
                    // Persist the size it was closed at, then end the instance — it
                    // never returns to the capture overlay / region picker.
                    self.mic_test_modal_open = false;
                    self.close_mic_test(); // stop any live mic-test capture first
                    self.save_state();
                    self.settings.window = None;
                    return self.finish_session();
                }
                if Some(id) == self.permissions.window {
                    // The permission-checker window closed — end this instance (it,
                    // like `--settings`, never returns to a capture overlay).
                    self.permissions.window = None;
                    return self.finish_session();
                }
                Task::none()
            }
            WindowChromeMsg::Close => {
                // Escape is a contextual step-back rather than a hard quit:
                //   settings open  → close settings
                //   countdown      → cancel timer, back to region select
                //   otherwise      → quit the tool (DRAGON-228: any picker closes
                //                    outright — no step-back to region select)
                if self
                    .settings
                    .window
                    .is_some_and(|w| self.core().focused_window() == Some(w))
                {
                    // Escape in the (focused) settings window collapses an open search.
                    // It no longer closes the window (use the ✕) — Cancel/close is an
                    // overlay-only shortcut.
                    if self.settings.search_active {
                        self.settings.search.clear();
                        self.settings.search_active = false;
                    }
                    Task::none()
                } else if self.recording.is_some() {
                    // Stop & save the recording (the worker finalizes; the poll
                    // opens the player).
                    self.stop_recording()
                } else if self.countdown.is_some() {
                    self.countdown = None;
                    self.pending = None;
                    self.capture_live = false;
                    self.mode = Mode::Region;
                    self.restore_interactive_overlays()
                } else {
                    self.teardown()
                }
            }
            // The toolbar ✕ ALWAYS quits outright — even in macOS resident mode,
            // where a finished session otherwise idles instead of exiting.
            WindowChromeMsg::Quit => self.quit_now(),
        }
    }

}
