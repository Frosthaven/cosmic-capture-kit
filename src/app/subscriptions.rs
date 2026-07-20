use super::*;

// Each tick lives behind its own named, documented trigger condition; `subscriptions`
// composes them into the one `Subscription` the `cosmic::Application::subscription`
// trait method returns (application.rs). Every condition/interval is unchanged from
// the single big method this replaced — only the shape (one fn per tick) is new.
impl App {
    /// Every live subscription for the current state, batched together. Called by
    /// `Application::subscription`.
    pub(super) fn subscriptions(&self) -> Subscription<Msg> {
        let base = Subscription::batch(
            [
                self.sub_global_events(),
                self.sub_countdown(),
                self.sub_toast(),
                self.sub_pixel_capture(),
                self.sub_loading_tick(),
                self.sub_playback_poll(),
                self.sub_tray_poll(),
                self.sub_recording_poll(),
                self.sub_meter_tick(),
                self.sub_mic_test(),
                self.sub_bench(),
                self.sub_scan_poll(),
                self.sub_permission_poll(),
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                self.sub_hotkey_suspend_ping(),
                #[cfg(windows)]
                self.sub_settings_liveness(),
                #[cfg(windows)]
                self.sub_preview_finalize(),
                #[cfg(target_os = "macos")]
                self.sub_preview_pinch(),
                // DRAGON-212: the frozen-flats grab is deferred on both platforms now.
                self.sub_frozen_ready(),
                // DRAGON-213: the launch-locked cursor rides its own launch thread.
                self.sub_cursor_ready(),
                #[cfg(target_os = "macos")]
                self.sub_wallpaper_ready(),
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                self.sub_passthrough(),
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                self.sub_settings_poke(),
            ]
            .into_iter()
            .flatten(),
        );
        // macOS residency lives in a separate menu-bar daemon (DRAGON-130), not the
        // app, so there are no in-app resident subscriptions to fold in — the batch
        // above is the whole set on every platform.
        base
    }

    /// Window/keyboard/output events forwarded into `update` — always live (not
    /// conditional on any state).
    fn sub_global_events(&self) -> Option<Subscription<Msg>> {
        Some(event::listen_with(|e, _, id| match e {
            // The settings toplevel can be closed by its ✕ or the WM.
            Event::Window(window::Event::CloseRequested) => {
                Some(Msg::WindowChrome(WindowChromeMsg::WindowCloseRequested(id)))
            }
            Event::Window(window::Event::Closed) => Some(Msg::WindowChrome(WindowChromeMsg::WindowClosed(id))),
            // macOS: keyboard focus follows the pointer across the per-display capture
            // overlays (guarded to overlay windows, selection phase only, in `update`).
            #[cfg(target_os = "macos")]
            Event::Mouse(cosmic::iced::mouse::Event::CursorEntered) => {
                Some(Msg::WindowChrome(WindowChromeMsg::CursorEnteredWindow(id)))
            }
            Event::Window(window::Event::Resized(size)) => {
                Some(Msg::WindowChrome(WindowChromeMsg::ConfigWindowResized(id, size.width, size.height)))
            }
            // Wayland output hotplug → per-monitor overlays. Linux-only (cctk);
            // macOS derives outputs from NSScreen/SCK (DRAGON-94 phase 2).
            #[cfg(target_os = "linux")]
            Event::PlatformSpecific(event::PlatformSpecific::Wayland(
                event::wayland::Event::Output(o, out),
            )) => Some(Msg::WindowChrome(WindowChromeMsg::Output(Box::new(o), out))),
            // Forward raw key presses; the live keymap resolves them to actions in
            // `update` (the subscription closure is `'static`, so it can't read the
            // current bindings — and rebinds must take effect immediately).
            Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                key, modifiers, ..
            }) => Some(Msg::WindowChrome(WindowChromeMsg::KeyPressed(modifiers, key))),
            // Releases matter only for push-to-talk (release → re-mute the mic).
            Event::Keyboard(cosmic::iced::keyboard::Event::KeyReleased {
                key, modifiers, ..
            }) => Some(Msg::WindowChrome(WindowChromeMsg::KeyReleased(modifiers, key))),
            _ => None,
        }))
    }

    /// macOS (DRAGON-130) / Windows (DRAGON-237): while the "Start Capture" chord recorder is
    /// armed, ping the resident daemon every ~1s to SUSPEND its global hotkey, so a
    /// PrintScreen press reaches this app to be recorded instead of spawning a capture. The
    /// daemon auto-resumes ~3s after the pings stop (see `crate::daemon` SuspendWindow), so no
    /// explicit resume is needed when recording ends or the window closes.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn sub_hotkey_suspend_ping(&self) -> Option<Subscription<Msg>> {
        if self.settings.capture_hotkey_rebinding {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_secs(1))
                    .map(|_| Msg::Settings(SettingsMsg::SuspendDaemonHotkeyPing)),
            )
        } else {
            None
        }
    }

    /// Windows (DRAGON-246): while the settings window is open AND has been CONFIRMED shown,
    /// poll (~1s) that its titled top-level still exists in this process. If it vanished
    /// without an iced `Closed` event — which would leave `finish_session` un-run and this
    /// --settings child a zombie holding the settings mutex + pid, so every later daemon
    /// "Settings" click self-exits on the held lock — the tick ends the instance via the
    /// normal `WindowClosed` path. Off until `settings_shown_confirmed`, so it can never fire
    /// during the open-hidden → float-and-show phase. No macOS/Linux analog (their settings
    /// close reliably reaches `WindowClosed`), so it stays byte-identical there.
    #[cfg(windows)]
    fn sub_settings_liveness(&self) -> Option<Subscription<Msg>> {
        if self.settings.window.is_some() && self.settings_shown_confirmed {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_secs(1))
                    .map(|_| Msg::WindowChrome(WindowChromeMsg::SettingsLivenessTick)),
            )
        } else {
            None
        }
    }

    /// Windows (DRAGON-281): while a post-capture preview surface exists whose native
    /// show/place has NOT been confirmed, re-drive its finalize every ~80ms. The preview is
    /// minted HIDDEN (komorebi opt-out) and shown only by the one-shot `window::open`
    /// follow-up (`PreviewOpened`/`PreviewOverlayOpened`) — which is NOT delivered while cck
    /// is a BACKGROUND process. That happens when the user focuses another window during the
    /// (click-through, DRAGON-276) countdown: the capture commits with cck backgrounded, and
    /// the preview HWND stays hidden forever (the "capture saved but the editor never
    /// appears" bug). Timer subscriptions DO pump while backgrounded (the countdown itself
    /// fires the same way), so this is the reliable re-driver — the exact shape of
    /// `sub_settings_liveness` (DRAGON-246). Off the moment `preview_shown_confirmed` matches
    /// the open surface's id, so it can never keep ticking under the (heavy) preview and peg
    /// the UI thread (cf. the DRAGON-247 meter-tick note at `sub_meter_tick`). No
    /// macOS/Linux analog (their open follow-up is delivered), so it stays byte-identical.
    #[cfg(windows)]
    fn sub_preview_finalize(&self) -> Option<Subscription<Msg>> {
        let unconfirmed = self
            .preview
            .as_ref()
            .is_some_and(|p| self.preview_shown_confirmed != Some(p.window));
        if unconfirmed {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(80))
                    .map(|_| Msg::WindowChrome(WindowChromeMsg::PreviewFinalizeTick)),
            )
        } else {
            None
        }
    }

    /// macOS (DRAGON-151) / Windows (DRAGON-276): while the countdown/recording
    /// overlays are click-through, poll the pointer against the toolbar-chip rects
    /// (~60ms — hover latency, not animation) so the hovered overlay re-solidifies
    /// and the chip stays clickable.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn sub_passthrough(&self) -> Option<Subscription<Msg>> {
        if self.passthrough_active {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(60))
                    .map(|_| Msg::Capture(CaptureMsg::PassthroughPoll)),
            )
        } else {
            None
        }
    }

    /// macOS (DRAGON-153) / Windows (DRAGON-246): while the settings pane is open, watch for
    /// the focus poke file a blocked second `--settings` launch touches (cross-process
    /// activation is cooperative-only on macOS 14+ and foreground-locked on Windows, so the
    /// pane must raise — and on Windows un-hide — ITSELF). 300ms is human-reaction latency for
    /// a rare event; the poll is a bare stat.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn sub_settings_poke(&self) -> Option<Subscription<Msg>> {
        if self.settings.window.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(300))
                    .map(|_| Msg::WindowChrome(WindowChromeMsg::SettingsFocusPoke)),
            )
        } else {
            None
        }
    }

    /// Precapture countdown ticks once a second.
    fn sub_countdown(&self) -> Option<Subscription<Msg>> {
        if self.countdown.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_secs(1))
                    .map(|_| Msg::Capture(CaptureMsg::Tick)),
            )
        } else {
            None
        }
    }

    /// Auto-dismiss the transient overlay message; the timer (re)starts when a
    /// toast appears, so its first tick clears it ~4s later.
    fn sub_toast(&self) -> Option<Subscription<Msg>> {
        if self.toast.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_secs(4))
                    .map(|_| Msg::Capture(CaptureMsg::DismissToast)),
            )
        } else {
            None
        }
    }

    /// One short tick to let the torn-down overlay clear the screen before we
    /// grab output pixels (window capture is overlay-independent, but a uniform
    /// delay is harmless).
    fn sub_pixel_capture(&self) -> Option<Subscription<Msg>> {
        if self.capturing.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(200))
                    .map(|_| Msg::Capture(CaptureMsg::DoPixelCapture)),
            )
        } else {
            None
        }
    }

    /// Poll the pre-capture result, then run down the warmup frames that keep the
    /// overlay up until the picker is ready. (The loading spinner self-animates via
    /// libcosmic's `indeterminate_circular`, so no tick is needed to draw it.)
    fn sub_loading_tick(&self) -> Option<Subscription<Msg>> {
        if self.windows_loading || self.window_warmup > 0 {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(50))
                    .map(|_| Msg::Capture(CaptureMsg::LoadingTick)),
            )
        } else {
            None
        }
    }

    /// DRAGON-148 option C / DRAGON-212 (both platforms): drain the DEFERRED frozen-flats
    /// grab into `self.frozen` the moment its thread posts. Runs only while `frozen_pending`
    /// (the grab is in flight); the overlay is already mapped and showing the live screen,
    /// and this poll flips it to the still as soon as the flats land. Cheap (a mutex
    /// try-drain), and stops the tick after the single delivery.
    fn sub_frozen_ready(&self) -> Option<Subscription<Msg>> {
        if self.frozen_pending {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(16))
                    .map(|_| Msg::Capture(CaptureMsg::FrozenReady)),
            )
        } else {
            None
        }
    }

    /// DRAGON-213: drain the DEDICATED launch cursor grab into `frozen_cursor` the moment
    /// its thread posts. Runs only while `cursor_pending` (both platforms); the grab is
    /// small/fast and fires at launch, so it lands almost immediately — this poll flips the
    /// region/monitor selector's on-overlay cursor indicator on as soon as the sprite is
    /// ready. Cheap (a mutex try-drain), and stops the tick after the single delivery.
    fn sub_cursor_ready(&self) -> Option<Subscription<Msg>> {
        if self.cursor_pending {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(16))
                    .map(|_| Msg::Capture(CaptureMsg::CursorReady)),
            )
        } else {
            None
        }
    }

    /// macOS (DRAGON-200): drain the DEFERRED per-output picker wallpaper into
    /// `self.wallpaper_handles` the moment its thread posts. Runs only while
    /// `wallpaper_pending` (the grab is in flight); the wallpaper is resolved AFTER
    /// the frozen flats, so the region still is never delayed by it, and the window
    /// picker (if entered before this lands) swaps its dark fill for the real
    /// wallpaper as soon as the pixels arrive. Cheap (a mutex try-drain), and stops
    /// the tick after the single delivery.
    #[cfg(target_os = "macos")]
    fn sub_wallpaper_ready(&self) -> Option<Subscription<Msg>> {
        if self.wallpaper_pending {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(50))
                    .map(|_| Msg::Capture(CaptureMsg::WallpaperReady)),
            )
        } else {
            None
        }
    }

    /// macOS (DRAGON-147): drive trackpad pinch-to-zoom while a preview is open by
    /// draining the gesture recognizer's accumulated magnification (`PinchPoll`).
    /// Only ticks while a preview exists; the poll is a cheap drain that no-ops when
    /// nothing is pending, so ~60 Hz here just bounds pinch latency.
    #[cfg(target_os = "macos")]
    fn sub_preview_pinch(&self) -> Option<Subscription<Msg>> {
        self.preview.as_ref().map(|_| {
            cosmic::iced::time::every(std::time::Duration::from_millis(16))
                .map(|_| Msg::Preview(PreviewMsg::PinchPoll))
        })
    }

    /// Pull decoded frames from the playback worker into the view while a recording
    /// is playing inline — polled at ~2× the source fps so frames advance smoothly.
    fn sub_playback_poll(&self) -> Option<Subscription<Msg>> {
        let interval = self.preview.as_ref().and_then(|p| p.playback_poll())?;
        Some(cosmic::iced::time::every(interval).map(|_| Msg::Preview(PreviewMsg::PlayerTick)))
    }

    /// While the recording controls live in the system tray, drain menu clicks
    /// promptly (a click → action within ~80ms, imperceptible).
    fn sub_tray_poll(&self) -> Option<Subscription<Msg>> {
        if self.tray.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(80))
                    .map(|_| Msg::Recording(RecordingMsg::TrayPoll)),
            )
        } else {
            None
        }
    }

    /// Live-status poll for the permission-checker window: while it is open, re-probe
    /// the TCC grants once a second so a card flips green the moment the user grants it
    /// in System Settings (the whole point of the screen). Gated on the window being
    /// open — nothing polls when it's closed. macOS-only (no grants to probe
    /// elsewhere), so absent off macOS (keeping the Linux subscription set identical).
    #[cfg(target_os = "macos")]
    fn sub_permission_poll(&self) -> Option<Subscription<Msg>> {
        if self.permissions.window.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_secs(1))
                    .map(|_| Msg::Permissions(PermissionsMsg::Poll)),
            )
        } else {
            None
        }
    }

    /// Off macOS there is no permission window to poll — a `None` stub so the batch
    /// call site stays platform-uniform.
    #[cfg(not(target_os = "macos"))]
    fn sub_permission_poll(&self) -> Option<Subscription<Msg>> {
        None
    }

    /// Poll for the recording worker finishing (after a stop, or if ffmpeg exits on
    /// its own); also drives the chip's elapsed-time/size readout and the live audio
    /// meters, so keep it brisk.
    fn sub_recording_poll(&self) -> Option<Subscription<Msg>> {
        if self.recording.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(100))
                    .map(|_| Msg::Recording(RecordingMsg::RecordingPoll)),
            )
        } else {
            None
        }
    }

    /// Not recording (mutually exclusive with `sub_recording_poll`, mirroring the
    /// original if/else-if) but a channel is armed (green): keep its meter live.
    fn sub_meter_tick(&self) -> Option<Subscription<Msg>> {
        // macOS/Windows also drive the tick for the armed-idle system-audio metering capture
        // (Bug B / DRAGON-248) — it publishes the sys level on its own thread and this tick is
        // what reads it into `self.sys_level` each frame.
        #[cfg(any(target_os = "macos", windows))]
        let sys_idle = self.sys_idle_meter.is_some();
        #[cfg(not(any(target_os = "macos", windows)))]
        let sys_idle = false;
        // Windows (DRAGON-247): the on-button meters live on the capture overlay; once
        // it is gone and the preview is showing, there is nothing to meter, but the
        // 100ms MeterTick keeps forcing a full re-render of the (heavy) video preview,
        // pegging the single UI thread so it never yields to pump Win32 input — the
        // window goes permanently unresponsive ("froze everything", force-kill). Suppress
        // the tick under the preview. Windows-only so Linux/macOS stay byte-identical.
        #[cfg(windows)]
        let overlay_gone = self.preview.is_some();
        #[cfg(not(windows))]
        let overlay_gone = false;
        if self.recording.is_none()
            && !overlay_gone
            && (self.mic_chain.is_some() || self.sys_meter.is_some() || sys_idle)
        {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(100))
                    .map(|_| Msg::Recording(RecordingMsg::MeterTick)),
            )
        } else {
            None
        }
    }

    /// Watchdog only: the canvas self-drives its animation at vsync and reads the
    /// buffer directly, so this slow tick just checks for a stalled reader (a fast
    /// tick here would rebuild the whole settings view and drop animation frames).
    fn sub_mic_test(&self) -> Option<Subscription<Msg>> {
        if self.mic_test.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(200))
                    .map(|_| Msg::Settings(SettingsMsg::MicTestTick)),
            )
        } else {
            None
        }
    }

    /// Poll the running encoder benchmark for progress.
    fn sub_bench(&self) -> Option<Subscription<Msg>> {
        if self
            .bench
            .as_ref()
            .is_some_and(|b| b.lock().map(|g| !g.finished).unwrap_or(false))
        {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(150))
                    .map(|_| Msg::Settings(SettingsMsg::BenchPoll)),
            )
        } else {
            None
        }
    }

    /// Poll the detection scans while QR/OCR is on in region mode — but only while
    /// there's actually something to poll: a pass in flight (waiting to be drained),
    /// or a settled region an enabled scanner hasn't scanned yet. When idle and
    /// up-to-date the timer vanishes, so the overlay sitting open doesn't rebuild
    /// two fullscreen surfaces 4x a second. A region change always arrives as user
    /// input, which re-evaluates subscriptions and restarts the tick.
    fn sub_scan_poll(&self) -> Option<Subscription<Msg>> {
        let scan_active = self.mode == Mode::Region
            && self.kind == Kind::Scanner
            && (self.scan_codes || (self.scan_text && self.tesseract_available));
        let region_now = self.normalized_region();
        let scan_pending = self.code_busy.load(std::sync::atomic::Ordering::Relaxed)
            || self.ocr_busy.load(std::sync::atomic::Ordering::Relaxed)
            || (self.scan_codes && region_now != self.last_code_region)
            || (self.scan_text && self.tesseract_available && region_now != self.last_ocr_region);
        // `self.scanning()` keeps the tick alive OUTSIDE scan_active too — even with
        // the scanner toggled off mid-pass, the drain is what clears the busy flags
        // (and the spinner); without it a toggle-off mid-scan spun forever.
        if (scan_active && scan_pending) || self.scanning() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(250))
                    .map(|_| Msg::Detect(DetectMsg::MarksPoll)),
            )
        } else {
            None
        }
    }

}

