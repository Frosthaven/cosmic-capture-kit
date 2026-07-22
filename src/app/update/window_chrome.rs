//! `WindowChromeMsg` handling — CSD window controls, close paths, quit.
//! Split from `application.rs` (DRAGON-115).

use super::super::*;
// `core()` is a `cosmic::Application` trait method — bring the trait into scope.
use cosmic::Application as _;

/// Settings-window width (logical px) at or above which the left nav rail is
/// auto-EXPANDED, and below which it auto-COLLAPSES to the icon rail. Applied on
/// window spawn AND every resize, on all platforms (the rule is width-based and
/// universal, no cfg fork). Value is the user's reference settings width so the
/// rail stays expanded at that size and narrowing collapses it. The manual
/// `ToggleConfigNav` still works between resizes; a resize re-derives from width.
pub(in crate::app) const NAV_AUTO_COLLAPSE_W: f32 = 990.0;

/// Pure width rule: should the settings nav rail be expanded at this logical
/// width? (Kept a tiny free fn so the boundary is unit-testable.)
pub(in crate::app) fn nav_should_open(width: f32) -> bool {
    width >= NAV_AUTO_COLLAPSE_W
}

impl App {
    /// Derive `nav_open` from the width the settings window will actually spawn
    /// at. Mirrors `open_config_window`'s clamp: the saved width (or the default)
    /// floored at `MIN_W` (800.0). Call this at every spawn site BEFORE the window
    /// is minted so the first paint carries the right rail + toggle icon (the icon
    /// derives from `nav_open` in `config_window_view`, so it follows for free).
    pub(in crate::app) fn apply_nav_auto_collapse_on_spawn(&mut self) {
        // Same effective width `open_config_window` computes: saved width, else the
        // default, floored at MIN_W = 800.0 (kept in sync with open_config_window).
        let spawn_w = self
            .settings_size
            .map(|(w, _)| w as f32)
            .unwrap_or(800.0)
            .max(800.0);
        self.settings.nav_open = nav_should_open(spawn_w);
    }

    /// Adopt a settings-window resize `(w, h)` (logical px) as the remembered size, and
    /// re-derive the left nav rail from the width. Auto collapse/expand (all platforms):
    /// narrower than the breakpoint collapses to the icon rail, at/above it expands. The toggle
    /// icon derives from `nav_open` in the view, so it flips for free on the next rebuild; a
    /// manual `ToggleConfigNav` still works between resizes (a resize re-derives from width).
    fn adopt_settings_size(&mut self, w: f32, h: f32) {
        self.settings_size = Some((w.round() as u32, h.round() as u32));
        self.settings.nav_open = nav_should_open(w);
    }

    pub(in crate::app) fn update_window_chrome(&mut self, message: WindowChromeMsg) -> Task<cosmic::Action<Msg>> {
        match message {
            #[cfg(target_os = "linux")]
            WindowChromeMsg::Output(ev, output) => self.on_output(*ev, output),
            #[cfg(not(target_os = "linux"))]
            WindowChromeMsg::SeedOutputs => self.seed_outputs_mac(),
            #[cfg(not(target_os = "linux"))]
            WindowChromeMsg::OverlayOpened(id, attempt) => self.configure_overlay(id, attempt),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            WindowChromeMsg::PreviewOpened(id, attempt) => self.finalize_preview_window(id, attempt),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            WindowChromeMsg::PreviewOverlayOpened(id, pos, size, attempt) => {
                self.finalize_preview_overlay(id, pos, size, attempt)
            }
            #[cfg(not(target_os = "linux"))]
            WindowChromeMsg::SeedOverlays => self.seed_overlays_mac(),
            WindowChromeMsg::OpenGear => self.open_settings(),
            WindowChromeMsg::OpenSettingsAtStartup { about } => {
                // Startup settings-window mint, deferred one message-drain past the launch
                // appearance apply so the FIRST paint carries the resolved accent with no
                // flash (DRAGON-268 follow-up). The `set_theme` queued from `init` was
                // drained just before this message in the SAME `update` pass, so the global
                // theme is already the launch-resolved one when the window-open task's
                // `WindowCreated` builds the window state below. Already-open (a duplicate
                // drain) is a no-op guard.
                if self.settings.window.is_some() {
                    return Task::none();
                }
                // Auto-collapse the nav rail by spawn width before minting (icon follows).
                self.apply_nav_auto_collapse_on_spawn();
                let (id, task) = settings::open_config_window(self.settings_size);
                self.settings.window = Some(id);
                // Windows (DRAGON-313): seed the OS title into the framework's per-window title map
                // NOW — before the returned open task runs — so the winit toplevel is BORN TITLED
                // (`with_title` at `CreateWindowExW`, read from `program.title(id)`), and komorebi's
                // first SHOW WinEvent already carries the title (see `open_settings` / the
                // `open_config_window` note). Only the synchronous map insert matters; the returned
                // async set_title is dropped (`ConfigWindowOpened` re-sets it). mac/Linux keep their
                // async-only title path, byte-identical.
                #[cfg(windows)]
                let _ = self.set_window_title(settings::WINDOW_TITLE.to_string(), id);
                let mut tasks = vec![task];
                // DRAGON-177: seed the update state from the on-disk manifest cache FIRST
                // (notes, nav tint, launch dialog render instantly); the network check
                // follows right behind and refreshes the seed.
                if let Some(seed) = crate::update::seeded_status_from_cache() {
                    tasks.push(Task::done(cosmic::Action::App(Msg::Settings(
                        SettingsMsg::UpdateChecked(seed),
                    ))));
                }
                // DRAGON-175: a settings window is showing, so kick off a background update
                // check (non-blocking; lights up the About nav + fills the About page).
                tasks.push(Task::done(cosmic::Action::App(Msg::Settings(
                    SettingsMsg::CheckForUpdates,
                ))));
                if about {
                    tasks.push(Task::done(cosmic::Action::App(Msg::Settings(
                        SettingsMsg::ShowAboutPage,
                    ))));
                }
                Task::batch(tasks)
            }
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
                // Windows: the settings window opens HIDDEN + BORN TITLED (DRAGON-313).
                // `ConfigWindowFloat` runs the title-polled follow-up to CENTER the still-hidden
                // window then natively show it (`SW_SHOW`, like the preview's `show_centered`),
                // install the native caption buttons + Mica, and settle the size — the same
                // title-polled follow-up shape as the mac titlebar centering above. Batched with the
                // title task so the poll begins as the title is being (re-)applied.
                #[cfg(windows)]
                return Task::batch([
                    title_task,
                    // DRAGON-238: probe the encoder list OFF the UI thread now (the video
                    // page's ffmpeg `-encoders` + hardware probe-encodes take seconds); the
                    // result fills the cache before the user reaches the Video tab.
                    self.kick_encoder_probe(),
                    Task::done(cosmic::Action::App(Msg::WindowChrome(
                        WindowChromeMsg::ConfigWindowFloat(id, 0),
                    ))),
                ]);
                #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
                title_task
            }
            #[cfg(windows)]
            WindowChromeMsg::ConfigWindowFloat(id, attempt) => {
                // Only the current settings window qualifies.
                if self.settings.window != Some(id) {
                    return Task::none();
                }
                // The window is created HIDDEN + BORN TITLED (DRAGON-313 fix2), so this poll runs
                // BEFORE it is shown. The title is set at creation but may lag this first poll by a
                // tick; retry briefly until `center_settings_window` matches it (same 30 x 40ms
                // budget as the overlay's `configure_overlay`). On match it CENTERS the still-hidden
                // window and NATIVELY shows it (`SW_SHOW`) — the settings twin of the preview's
                // `show_centered`. komorebi's first titled SHOW then sees the already-centered window
                // and tiles it; because nothing repositions it after the show, the tiler's placement
                // wins cleanly (the earlier open-VISIBLE path centered POST-show and fought it).
                const MAX_ATTEMPTS: u8 = 30;
                const RETRY_MS: u64 = 40;
                if crate::platform::windows::window::center_settings_window(settings::WINDOW_TITLE) {
                    // The window matched its born-set title and was centered + shown → arm the
                    // liveness watchdog (DRAGON-246): from here on, a disappearance of the
                    // titled window means a genuine vanish-without-`Closed` (out-of-band
                    // destroy), and `sub_settings_liveness` ends the instance so the settings
                    // mutex + pid file never leak. Set ONLY on a real match (not on the
                    // give-up branch below, where the window may not exist), so the
                    // guard can never fire during the open-hidden → center-and-show phase.
                    self.settings_shown_confirmed = true;
                    // Native DWM caption buttons (DRAGON-284): now that the settings window is shown,
                    // extend the top frame + install the caption subclass so the native min/max/close
                    // render top-right over our owned header. Idempotent (once per HWND). The caption
                    // carve completes the ±0 size round-trip `center_settings_window` pre-inflated for.
                    crate::platform::windows::caption::install_native_caption_buttons(
                        settings::WINDOW_TITLE,
                    );
                    // Mica backdrop (DRAGON-267): apply the DWM Mica material — gated on the SAME
                    // frosted-windows signal Linux uses (`self.glass` is `Some(frosted_windows)` only
                    // on Win11 22H2+ and not `CCK_NO_GLASS`-disabled), so the unified toggle turns it
                    // off too. A no-op otherwise. The chrome already paints translucent from `self.glass`.
                    if self.glass.is_some_and(|g| g.frosted_windows) {
                        crate::platform::windows::window::apply_mica(settings::WINDOW_TITLE);
                    }
                    // DRAGON-299/313: the sub-min re-assert in `ConfigWindowResized` remains a
                    // defensive net against any post-show winit soft-size; schedule the SETTLE that
                    // flips resize-tracking on once any open-time transient resizes have been consumed
                    // (drift-free). `center_settings_window`'s `SW_SHOW` already activated the window,
                    // so no separate focus command is needed.
                    Task::perform(
                        async move {
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        },
                        move |()| {
                            cosmic::Action::App(Msg::WindowChrome(
                                WindowChromeMsg::ConfigWindowResettle(id),
                            ))
                        },
                    )
                } else if attempt >= MAX_ATTEMPTS {
                    log::warn!(
                        "settings window never matched its title after {MAX_ATTEMPTS} \
                         attempts — it may stay hidden"
                    );
                    Task::none()
                } else {
                    Task::perform(
                        async move {
                            tokio::time::sleep(std::time::Duration::from_millis(RETRY_MS)).await;
                        },
                        move |()| {
                            cosmic::Action::App(Msg::WindowChrome(
                                WindowChromeMsg::ConfigWindowFloat(id, attempt + 1),
                            ))
                        },
                    )
                }
            }
            #[cfg(windows)]
            WindowChromeMsg::ConfigWindowResettle(id) => {
                // DRAGON-299: the one-time post-show winit stomp has settled (the resize handler
                // re-asserted the intended size on it, and that re-assert's resize was consumed
                // while tracking was off). Flip tracking on so subsequent USER resizes update the
                // remembered size; guard on the current window so a stale timer can't arm it.
                if self.settings.window == Some(id) {
                    self.settings_size_ready = true;
                }
                Task::none()
            }
            #[cfg(windows)]
            WindowChromeMsg::PreviewFinalizeTick => {
                // DRAGON-281: re-drive the preview's native show/place from a timer
                // subscription (which pumps even while cck is a background process) — the
                // one-shot `window::open` follow-up that normally triggers the finalize is
                // NOT delivered when cck lost the foreground mid-countdown, leaving the
                // preview HWND created but hidden forever. Read the CURRENT open surface and
                // call the SAME finalize the follow-up would have (`PreviewOpened` →
                // `finalize_preview_window` for the windowed preview, `PreviewOverlayOpened`
                // → `finalize_preview_overlay` for the overlay preview). Both no-op once the
                // window id no longer matches or the show is confirmed; on success they set
                // `preview_shown_confirmed`, which stops `sub_preview_finalize`. Reading self
                // (not a captured id) keeps it stale-proof, like `SettingsLivenessTick`.
                match self.preview.as_ref().map(|p| (p.window, p.surface.is_window())) {
                    Some((id, true)) => self.finalize_preview_window(id, 0),
                    Some((id, false)) => {
                        // Recompute the overlay's target rect exactly as the open path did
                        // (`preview_overlay_rect(preview_output)`), the same way
                        // `finalize_preview_window` recomputes it internally.
                        let name = self.preview_output.as_ref().map(|(n, _)| n.clone());
                        let (pos, size) =
                            crate::platform::windows::window::preview_overlay_rect(name.as_deref());
                        self.finalize_preview_overlay(id, pos, size, 0)
                    }
                    None => Task::none(),
                }
            }
            #[cfg(windows)]
            WindowChromeMsg::SettingsLivenessTick => {
                // DRAGON-246: periodic watchdog for the "settings window vanished without a
                // Closed event" zombie. If the settings window is open, was CONFIRMED shown,
                // and its titled top-level is no longer present in THIS process, it was
                // destroyed out-of-band — no iced `window::Event::Closed` reached us, so
                // `WindowClosed` → `finish_session` never ran and this --settings child would
                // linger holding the settings mutex + pid. Re-dispatch the exact `WindowClosed`
                // path (save_state + finish_session + the Windows hard-exit backstop) so the
                // instance ends just as a normal ✕-close would. The pure `settings_should_end`
                // decision keeps the shown-then-gone vs not-yet-shown logic unit-tested.
                let title_present =
                    crate::platform::windows::window::find_by_title(settings::WINDOW_TITLE)
                        .is_some();
                if crate::platform::windows::window::settings_should_end(
                    self.settings.window.is_some(),
                    self.settings_shown_confirmed,
                    title_present,
                ) && let Some(id) = self.settings.window
                {
                    log::warn!(
                        "DRAGON-246: settings window vanished without a Closed event — ending \
                         the instance (the settings mutex/pid would otherwise leak)"
                    );
                    return Task::done(cosmic::Action::App(Msg::WindowChrome(
                        WindowChromeMsg::WindowClosed(id),
                    )));
                }
                Task::none()
            }
            WindowChromeMsg::ConfigWindowResized(id, w, h) => {
                // Remember the settings window's size so it reopens at the size it was
                // closed at (persisted on close).
                if Some(id) == self.settings.window && w >= 1.0 && h >= 1.0 {
                    // Windows (DRAGON-299/313): the settings window opens hidden and is centered +
                    // natively shown (`center_settings_window`, the preview's `show_centered` twin)
                    // on the FIRST poll now that the title is born-set, so the DRAGON-299 sub-min
                    // size STOMP is not observed. These two guards remain as a DEFENSIVE net, both
                    // keyed off the enforced minimum (a real USER resize can never go below it — the
                    // resize border enforces min_size — so nothing below is ever legitimate):
                    //   * ANY sub-min resize is treated as a stomp — NEVER adopt it; re-assert the
                    //     intended (persisted, unpolluted) size natively. Unconditional on
                    //     `settings_size_ready` so it can't race the settle timer.
                    //   * a ≥min resize is adopted only once the window has SETTLED
                    //     (`settings_size_ready`, flipped by `ConfigWindowResettle`) — before that
                    //     it's an open-time transient (incl. any re-assert's own resize), ignored so
                    //     none is persisted as the remembered size.
                    // mac/Linux open the settings window visible + compositor-managed (no such
                    // stomp), so they adopt every resize as before.
                    #[cfg(windows)]
                    {
                        if w < settings::MIN_W || h < settings::MIN_H {
                            let (tw, th) = self
                                .settings_size
                                .map(|(a, b)| {
                                    (a.max(settings::MIN_W as u32), b.max(settings::MIN_H as u32))
                                })
                                .unwrap_or((settings::MIN_W as u32, settings::MIN_H as u32));
                            crate::platform::windows::window::resize_settings_to(
                                settings::WINDOW_TITLE,
                                (tw, th),
                            );
                        } else if self.settings_size_ready {
                            self.adopt_settings_size(w, h);
                        }
                    }
                    #[cfg(not(windows))]
                    self.adopt_settings_size(w, h);
                }
                // PROVISIONAL (DRAGON-268 follow-up, Task 2 — fullscreen-too-bright): a
                // fullscreen enter/exit fires a resize, so match the settings / windowed-
                // preview window's backdrop to its fullscreen state here — opaque dark theme
                // pane in fullscreen (the 0.75 page tint over the bright vibrancy no-backdrop
                // fallback is what reads too bright), vibrancy restored out of fullscreen.
                // Idempotent + self-correcting; scoped to our CSD toplevels by title.
                #[cfg(target_os = "macos")]
                {
                    let dark = crate::app::theme::background_base_rgba();
                    crate::platform::mac::window::match_window_fullscreen_backdrop(
                        settings::WINDOW_TITLE,
                        dark,
                    );
                    crate::platform::mac::window::match_window_fullscreen_backdrop(
                        crate::app::shell::PREVIEW_WINDOW_TITLE,
                        dark,
                    );
                    // DRAGON-268 follow-up (fullscreen header vanish): learn whether each
                    // CSD toplevel is in native fullscreen NOW (a fullscreen enter/exit
                    // fires this resize), so the view can drop the traffic-light inset (the
                    // lights auto-hide in fullscreen) and keep the preview's Close reachable.
                    // The `match_...backdrop` calls above already detached the auto-hiding
                    // toolbar so the app's own header renders flush at the fullscreen top.
                    self.settings_fullscreen =
                        crate::platform::mac::window::window_is_fullscreen_by_title(
                            settings::WINDOW_TITLE,
                        );
                    self.preview_fullscreen =
                        crate::platform::mac::window::window_is_fullscreen_by_title(
                            crate::app::shell::PREVIEW_WINDOW_TITLE,
                        );
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
            WindowChromeMsg::ConfigWindowMaximize => {
                // Windows (DRAGON-258): the settings window is a frameless, natively-managed
                // toplevel, so iced's `window::toggle_maximize` is a no-op for it — the CSD
                // control did nothing. Route to the native Win32 helper (keyed on the same
                // title the other Windows settings helpers use); keep Linux/mac on iced.
                #[cfg(windows)]
                crate::platform::windows::window::toggle_maximize(settings::WINDOW_TITLE);
                #[cfg(windows)]
                return Task::none();
                #[cfg(not(windows))]
                match self.settings.window {
                    Some(id) => window::toggle_maximize(id),
                    None => Task::none(),
                }
            }
            WindowChromeMsg::ConfigWindowMinimize => {
                // Windows (DRAGON-258): iced's `window::minimize` is likewise a no-op for the
                // frameless settings toplevel — native `ShowWindow(SW_MINIMIZE)` instead.
                #[cfg(windows)]
                crate::platform::windows::window::minimize(settings::WINDOW_TITLE);
                #[cfg(windows)]
                return Task::none();
                #[cfg(not(windows))]
                match self.settings.window {
                    Some(id) => window::minimize(id, true),
                    None => Task::none(),
                }
            }
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
                    // Window vibrancy (DRAGON-268): the settings window is now up and
                    // matched — reveal the winit-inserted vibrancy by clearing its Metal
                    // layer. Gated on the SAME frosted-windows signal Linux/Windows use
                    // (`self.glass`, `Some(frosted_windows)` unless `CCK_NO_GLASS`), so the
                    // unified toggle turns it off too. The chrome already paints translucent
                    // from `self.glass`; a no-op when glass is off.
                    if self.glass.is_some_and(|g| g.frosted_windows) {
                        crate::platform::mac::window::enable_window_vibrancy(title);
                    }
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
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            WindowChromeMsg::SettingsFocusPoke => {
                // A blocked second `--settings` launch asked us to come forward. The
                // poke tick fires every 300ms while the pane is open; only act when
                // the file is present (removing it consumes the request).
                let poke = crate::platform::compositor::settings_focus_poke_path();
                if std::fs::metadata(&poke).is_ok() {
                    let _ = std::fs::remove_file(&poke);
                    if let Some(id) = self.settings.window {
                        // Windows (DRAGON-246): the pane's window may be HIDDEN / minimized /
                        // orphaned — the REAL bug: an external `SW_HIDE` or a lost native show
                        // leaves iced's id valid but the HWND off-screen, while this process
                        // lives on holding the settings mutex + pid, so every later "Settings"
                        // click self-exits on the held lock. Natively RESTORE + foreground the
                        // window FIRST so the `gain_focus` below keys a genuinely visible pane.
                        #[cfg(windows)]
                        crate::platform::windows::window::show_and_focus(settings::WINDOW_TITLE);
                        // Activation from the OWNING process with a visible window
                        // sticks (winit's gain_focus does activate + makeKey).
                        return window::gain_focus(id);
                    }
                    // Windows (DRAGON-246): a poke arrived but this holder has NO settings
                    // window — it was truly destroyed, not merely hidden — so end the instance
                    // and let the blocked launcher's retry open a FRESH pane instead of finding
                    // the mutex still held by a windowless zombie. (On macOS this arm is
                    // stripped: its self-activation path is only ever poked while a window
                    // exists, so it keeps the historical `Task::none()` fall-through.)
                    #[cfg(windows)]
                    return self.finish_session();
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

#[cfg(test)]
mod tests {
    use super::{nav_should_open, NAV_AUTO_COLLAPSE_W};

    #[test]
    fn nav_collapses_below_breakpoint_expands_at_or_above() {
        assert_eq!(NAV_AUTO_COLLAPSE_W, 990.0);
        // Just below the breakpoint: collapse.
        assert!(!nav_should_open(989.0));
        // Exactly the breakpoint: expand.
        assert!(nav_should_open(990.0));
        // Just above: expand.
        assert!(nav_should_open(991.0));
        // Sanity at extremes.
        assert!(!nav_should_open(460.0));
        assert!(nav_should_open(1920.0));
    }
}
