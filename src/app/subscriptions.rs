use super::*;

/// How often a document with a live upload asks its session sidecar what happened
/// (`App::sub_upload_poll`).
///
/// Named in DRAGON-507 because a second duration now depends on it: this tick is what retires
/// a meter whose X has been pressed, so `preview::edit::UPLOAD_ENDING_HOLD` can never be
/// honoured more finely than this, and a compile-time assert beside that constant keeps the
/// two in a relationship somebody can read.
pub(crate) const UPLOAD_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

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
                self.sub_external_recording(),
                self.sub_toast(),
                self.sub_pixel_capture(),
                self.sub_scan_shot(),
                self.sub_menu_hold(),
                self.sub_dim_fade(),
                self.sub_accept_pending(),
                self.sub_scan_spin(),
                self.sub_loading_tick(),
                self.sub_playback_poll(),
                self.sub_preview_toasts(),
                self.sub_upload_poll(),
                self.sub_upload_finalize_anim(),
                self.sub_text_caret_blink(),
                self.sub_tray_poll(),
                self.sub_countdown_tray_poll(),
                self.sub_preview_handoff(),
                self.sub_recording_poll(),
                self.sub_meter_tick(),
                self.sub_mic_test(),
                self.sub_bench(),
                self.sub_scan_poll(),
                self.sub_permission_poll(),
                self.sub_cloud_browser_countdown(),
                self.sub_cloud_folder_spin(),
                self.sub_health_copy_flash(),
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                self.sub_hotkey_suspend_ping(),
                #[cfg(windows)]
                self.sub_settings_liveness(),
                #[cfg(windows)]
                self.sub_preview_finalize(),
                #[cfg(windows)]
                self.sub_overlay_finalize(),
                #[cfg(target_os = "macos")]
                self.sub_preview_pinch(),
                #[cfg(target_os = "macos")]
                self.sub_color_picker_pinch(),
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
        Some(event::listen_with(|e, status, id| match e {
            // The settings toplevel can be closed by its ✕ or the WM.
            Event::Window(window::Event::CloseRequested) => {
                Some(Msg::WindowChrome(WindowChromeMsg::WindowCloseRequested(id)))
            }
            Event::Window(window::Event::Closed) => Some(Msg::WindowChrome(WindowChromeMsg::WindowClosed(id))),
            // macOS: keyboard focus follows the pointer across the per-display capture
            // overlays (guarded to overlay windows, selection phase only, in `update`).
            // Linux (DRAGON-317 regression fix): the SAME event is how we learn the pointer's
            // output — the reliable capture-origin monitor — from the overlay the cursor first
            // enters (cosmic-comp maps our overlay under the cursor, so its enter names it).
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Event::Mouse(cosmic::iced::mouse::Event::CursorEntered) => {
                Some(Msg::WindowChrome(WindowChromeMsg::CursorEnteredWindow(id)))
            }
            Event::Window(window::Event::Resized(size)) => {
                Some(Msg::WindowChrome(WindowChromeMsg::ConfigWindowResized(id, size.width, size.height)))
            }
            // DRAGON-482: a window taking keyboard focus. The preview editor's Upload button
            // is gated on a SNAPSHOT of the connected accounts, and Settings is a separate
            // document (often a separate process), so connecting an account there left the
            // button greyed until the editor was reopened. Coming back to the editor is
            // exactly the moment to re-read, and it is also the only moment the app can
            // observe without polling. Ignored for every window that is not a preview.
            Event::Window(window::Event::Focused) => {
                Some(Msg::WindowChrome(WindowChromeMsg::WindowFocused(id)))
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
            // The delivering surface's id rides along: it is what picks the PREVIEW
            // document a Preview-context binding acts on when several are open
            // (DRAGON-336 phase 2 — see `handle_key`).
            // `location` (DRAGON-364) is the key's PHYSICAL group and is the ONLY thing that
            // separates numpad Enter from main Enter: both the winit path and the Wayland
            // layer-shell path map `KP_Enter` to the same logical `Key::Named(Named::Enter)`
            // and record the keypad only here. Forwarded verbatim; the live text-annotation
            // editor is its one consumer today.
            // `repeat` (DRAGON-601) separates a real press from an auto-repeat the compositor
            // synthesised while the key stayed down. It was destructured away here, so the app
            // treated a hold as a stream of indistinguishable presses; the nudge lane needs the
            // difference to spend exactly one pixel per tap.
            Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                key, modifiers, location, text, repeat, ..
            }) => Some(Msg::WindowChrome(WindowChromeMsg::KeyPressed(
                id,
                modifiers,
                key,
                location,
                text.map(|t| t.to_string()),
                repeat,
            ))),
            // Releases matter only for push-to-talk (release → re-mute the mic).
            Event::Keyboard(cosmic::iced::keyboard::Event::KeyReleased {
                key, modifiers, ..
            }) => Some(Msg::WindowChrome(WindowChromeMsg::KeyReleased(id, modifiers, key))),
            // DRAGON-599: a right press cancels the capture session, on the surfaces where
            // that is what it can mean. The rule itself is `right_click_cancels` in
            // `update::window_chrome`; all this arm does is deliver the press and say which
            // surface took it.
            //
            // `Status::Ignored` is the whole of "no widget claimed it", and it is what keeps
            // this from stealing a right-click that already has a job: the scanner's OCR word
            // menu and its QR/barcode contents menu both `capture_event()` in
            // `widgets::region_selection`, and the preview timeline's context menu does the
            // same, so those presses never reach here at all. Every other arm in this closure
            // ignores the status because a key press means the same thing whoever handled it;
            // a mouse button does not.
            Event::Mouse(cosmic::iced::mouse::Event::ButtonPressed(
                cosmic::iced::mouse::Button::Right,
            )) if status == cosmic::iced::event::Status::Ignored => {
                Some(Msg::WindowChrome(WindowChromeMsg::RightPressed(id)))
            }
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
        if self.settings.capture_hotkey_rebinding.is_some() {
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
            .previews
            .iter()
            .any(|p| self.preview_shown_confirmed != Some(p.window));
        if unconfirmed {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(80))
                    .map(|_| Msg::WindowChrome(WindowChromeMsg::PreviewFinalizeTick)),
            )
        } else {
            None
        }
    }

    /// Windows (DRAGON-437): while any capture overlay has NOT been confirmed placed,
    /// re-drive its native placement every ~80ms. The overlay's twin of
    /// `sub_preview_finalize`, and it exists because the overlay had NO re-driver at all
    /// while the settings window and the preview both did.
    ///
    /// Two independent failures made that gap fatal rather than cosmetic. The overlay is
    /// minted HIDDEN (komorebi opt-out) and shown only by `place_overlay`'s two-phase dance,
    /// kicked by the one-shot `window::open` follow-up — which is NOT delivered while cck is
    /// a BACKGROUND process (a tray-daemon-spawned child). And even when it IS delivered, the
    /// follow-up chain stops after 30 × 40ms, while phase 2 requires a call at least 120ms
    /// after phase 1: a title landing near the end of that budget leaves the window CLOAKED
    /// and parked off-screen, invisible forever, in a process that never exits. Timer
    /// subscriptions DO pump while backgrounded, so this is the reliable driver, and the one
    /// that owns the give-up deadline.
    ///
    /// Off the moment every output reports `placed`, so it cannot keep ticking under a live
    /// capture UI. Also off once the give-up has run, and whenever the overlays are torn down
    /// (`stop_overlay_finalize`), so it can never outlive the windows it drives.
    ///
    /// The condition reads `overlay_finalize_pending`, NOT the deadline clock: the clock is
    /// stamped by the first delivered tick, so letting it gate the subscription would mean
    /// the driver could never start. No macOS/Linux analog, so they stay byte-identical.
    #[cfg(windows)]
    fn sub_overlay_finalize(&self) -> Option<Subscription<Msg>> {
        let waiting = crate::app::update::window_chrome::overlay_finalize_active(
            self.overlay_finalize_pending,
            self.outputs.iter().any(|o| !o.placed.get()),
        );
        if waiting {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(80))
                    .map(|_| Msg::WindowChrome(WindowChromeMsg::OverlayFinalizeTick)),
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

    /// While the cloud-accounts add flow is waiting on the browser (DRAGON-489 follow-up):
    /// tick once a second so `pages::cloud::cloud_browser_step`'s countdown (read against
    /// `CloudAdd::browser_started` and `Instant::now()` at render time) stays live.
    fn sub_cloud_browser_countdown(&self) -> Option<Subscription<Msg>> {
        if self.settings.cloud.waiting_on_browser() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_secs(1))
                    .map(|_| Msg::Settings(SettingsMsg::Cloud(CloudSettingsMsg::CloudBrowserTick))),
            )
        } else {
            None
        }
    }

    /// While a Health-page location copy is still flashing (DRAGON-540): tick once a second so
    /// the copy button's "Copied!" tick reverts on its own.
    ///
    /// `sub_cloud_browser_countdown` in every respect, including the rule that matters: the
    /// flash is a WINDOW read against `Instant::now()` at render time, so the view has to be
    /// rebuilt once more after it closes or the tick stays up until something else redraws.
    /// It gates itself off with the flash, so an idle settings window ticks nothing.
    ///
    /// Covers every page that renders a path through `settings::row::path_line`, not just
    /// Health: the flash is keyed on the copied text and stamped in one place, so Scanner's
    /// language folder and the preview editor's covermark folder need no tick of their own.
    /// DRAGON-583 put the Keyboard page's recording COMMANDS on the same footing: they are
    /// not paths, but they flash through the same `path_line_copied` window, so they ride
    /// this tick too and the name is now the only thing about it that says "Health".
    fn sub_health_copy_flash(&self) -> Option<Subscription<Msg>> {
        let (_, at) = self.settings.health_copied.as_ref()?;
        if crate::widgets::copy_button::copied_recently(Some(*at)) {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_secs(1))
                    .map(|_| Msg::Settings(SettingsMsg::HealthCopyTick)),
            )
        } else {
            None
        }
    }

    /// While a cloud folder LISTING is in flight (DRAGON-514): tick ~30fps so the path row's
    /// refresh glyph spins. That spin is the browser's only loading indication, the
    /// "Reading your folders..." line having been deleted with this ticket.
    ///
    /// The scanner's `sub_scan_spin` in every respect, including the rule that matters: gated on
    /// the listing, so an idle dialog (and a settings window with no dialog open) ticks nothing.
    /// It stops on its own when the listing lands, which is also when the button becomes
    /// pressable again, so the animation and the affordance cannot disagree.
    fn sub_cloud_folder_spin(&self) -> Option<Subscription<Msg>> {
        if self.settings.cloud.listing_folders() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(33))
                    .map(|_| Msg::Settings(SettingsMsg::Cloud(CloudSettingsMsg::FolderSpinTick))),
            )
        } else {
            None
        }
    }

    /// DRAGON-322: while a capture-selection overlay is up (not recording/previewing
    /// ourselves), poll ~1s whether ANOTHER instance is recording, so the video kind
    /// disables/re-enables live if a recording starts or ends elsewhere. Cheap (a
    /// runtime-dir scan); idle the rest of the time (no overlay ⇒ no tick).
    fn sub_external_recording(&self) -> Option<Subscription<Msg>> {
        let selecting = self.recording.is_none()
            && self.previews.is_empty()
            && self.countdown.is_none()
            && self.capturing.is_none()
            && !self.settings.only
            && !self.outputs.is_empty();
        if selecting {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_secs(1))
                    .map(|_| Msg::Capture(CaptureMsg::ExternalRecordingTick)),
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

    /// DRAGON-612: re-ask a HELD accept while the colour picker is still finding its pixel.
    ///
    /// Armed only by a press the gate answered `Wait`, which only a picker launch can produce,
    /// so an ordinary capture launch never schedules it at all. 16ms to match `sub_dim_fade`,
    /// because the pick should land on the first frame after the snapshot arrives rather than
    /// up to a tick later.
    ///
    /// It stops on its own the moment the request is resolved. `accept_pending` has exactly
    /// three writers, and knowing them as a set is the point, because "a 16ms poll that quietly
    /// stays armed" is the failure mode a conditional subscription exists to prevent: every
    /// branch of `request_accept` that is not another `Wait` clears it (fired, refused, or out
    /// of budget), and `destroy_surfaces` clears it too, so a pick or a teardown cannot leave a
    /// tick running under the result window for the rest of the session.
    fn sub_accept_pending(&self) -> Option<Subscription<Msg>> {
        if self.accept_pending.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(16))
                    .map(|_| Msg::Capture(CaptureMsg::AcceptPending)),
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

    /// DRAGON-456: one short tick to let the BLANKED overlay clear off the screen before a
    /// scan refresh re-grabs it. The twin of [`Self::sub_pixel_capture`] for a session that
    /// continues: nothing was torn down, the overlay just stopped painting, so the same
    /// 200ms is what the compositor needs to present the empty surface. Stops immediately
    /// once the grab is away (`Grabbing`), so it is one tick, not a poll.
    /// DRAGON-460: drive the refresh button's spin while the scanner is busy.
    ///
    /// ~30fps, and ONLY while `scanning()` — an idle scanner ticks nothing, so this cannot
    /// become a background redraw the way an always-on animation clock would. It stops on
    /// its own when the passes finish, which is also when the button becomes pressable
    /// again, so the animation and the affordance can never disagree.
    fn sub_scan_spin(&self) -> Option<Subscription<Msg>> {
        if self.scanning() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(33))
                    .map(|_| Msg::Capture(CaptureMsg::ScanSpinTick)),
            )
        } else {
            None
        }
    }

    /// DRAGON-600 (Linux, tray-menu launches): poll the HELD frozen-flats grab while the
    /// tray dropdown is still being retired. One frame's cadence, because the release is a
    /// short settle after our overlay takes keyboard focus and a coarser tick would spend
    /// more of the launch waiting than the settle itself costs. Stops for good the moment
    /// the hold releases, so nothing polls after the grab is running.
    fn sub_menu_hold(&self) -> Option<Subscription<Msg>> {
        if self.menu_hold.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(16))
                    .map(|_| Msg::Capture(CaptureMsg::MenuHoldTick)),
            )
        } else {
            None
        }
    }

    /// DRAGON-606: drive the dim's fade-in, at one frame's cadence, and ONLY while it is
    /// armed or actually ramping. `Waiting` schedules nothing (the frozen-flats grab may
    /// still be reading the screen), and `Done` schedules nothing either, so a 200ms
    /// animation cannot leave a timer running for the rest of the session.
    ///
    /// `Armed` DOES tick, and it has to: the state only advances when a frame is built, so
    /// on a launch where nothing else is asking for redraws this tick is what produces the
    /// frame that starts the clock. Without it an armed fade could sit unstarted.
    fn sub_dim_fade(&self) -> Option<Subscription<Msg>> {
        if matches!(
            self.dim_fade.get(),
            crate::app::overlay::DimFade::Armed | crate::app::overlay::DimFade::Running(_)
        ) {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(16))
                    .map(|_| Msg::Capture(CaptureMsg::DimFadeTick)),
            )
        } else {
            None
        }
    }

    fn sub_scan_shot(&self) -> Option<Subscription<Msg>> {
        if self.scan_shot == ScanShot::Clearing {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(200))
                    .map(|_| Msg::Capture(CaptureMsg::ScanShotTick)),
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
        // One subscription PER open preview, each with a DISTINCT hash id (the window id
        // is folded in via `Subscription::with`, so iced can't collapse two identical
        // 16ms timers into one) and each tick addressed to its own document.
        if self.previews.is_empty() {
            return None;
        }
        Some(Subscription::batch(self.previews.iter().map(|p| {
            let id = p.window;
            cosmic::iced::time::every(std::time::Duration::from_millis(16))
                .with(id)
                .map(|(id, _)| Msg::Preview(id, PreviewMsg::PinchPoll))
        })))
    }

    /// macOS: drive trackpad pinch-to-zoom for the colour picker's magnifier, the same way
    /// [`Self::sub_preview_pinch`] does for the preview editor: drain the gesture recognizer's
    /// accumulated magnification (`ColorPickerMsg::PinchPoll`) while there is a picker overlay
    /// to zoom.
    ///
    /// ONE poll, not one per output. Unlike a preview, which is several independent DOCUMENTS
    /// each with their own zoom, the picker's magnification is a SINGLE piece of state
    /// (`ColorPickerState::zoom`) shared by every per-output overlay a picker launch mints, so
    /// there is only ever one thing here to drive.
    ///
    /// Gated on the OVERLAY being mapped, not on [`Self::color_picking`] alone:
    /// `color_picker.active` marks this whole PROCESS as a picker launch and stays true for its
    /// entire life, including after a pick tears the overlay down and opens the small result
    /// window, where there is no magnifier left to zoom. `self.outputs` is what the overlay
    /// actually maps to and is cleared the moment it closes (`destroy_surfaces`), so pairing the
    /// two is exactly "the picker's dimmed overlay is on screen right now".
    #[cfg(target_os = "macos")]
    fn sub_color_picker_pinch(&self) -> Option<Subscription<Msg>> {
        if self.color_picking() && !self.outputs.is_empty() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(16))
                    .map(|_| Msg::ColorPicker(ColorPickerMsg::PinchPoll)),
            )
        } else {
            None
        }
    }

    /// Expire each document's in-editor toasts (DRAGON-353).
    ///
    /// One tick PER document that actually HAS toasts, each with a DISTINCT hash id (via
    /// `with(window_id)`, without which iced collapses same-interval timers into one) and
    /// each addressed to its own document — the same shape `sub_playback_poll` uses. The
    /// whole subscription disappears once every queue drains, so an idle editor ticks
    /// nothing at all.
    ///
    /// The interval is a fraction of `TOAST_TTL`: a toast may linger up to one tick past
    /// its expiry, which at 250ms against a 4s life is imperceptible and costs far less
    /// than a per-toast timer would. It is also a fraction of the interaction TTL (750ms —
    /// DRAGON-353 follow-up), so a toast the user shortened by getting hands-on still goes
    /// promptly rather than waiting on a coarse sweep.
    fn sub_preview_toasts(&self) -> Option<Subscription<Msg>> {
        let ticks: Vec<_> = self
            .previews
            .iter()
            .filter(|p| !p.toasts.is_empty())
            .map(|p| {
                let id = p.window;
                cosmic::iced::time::every(std::time::Duration::from_millis(250))
                    .with(id)
                    .map(|(id, _)| Msg::Preview(id, PreviewMsg::ToastTick))
            })
            .collect();
        if ticks.is_empty() {
            return None;
        }
        Some(Subscription::batch(ticks))
    }

    /// While a document has an upload it started still in flight, poll its progress/outcome
    /// (DRAGON-490): one tick PER document that actually HAS an entry in `EditState::uploads`,
    /// the same per-document shape `sub_preview_toasts` and `sub_playback_poll` use (a distinct
    /// hash id via `with(window_id)`, so iced does not collapse same-interval timers into one).
    /// Vanishes the instant every upload a document is watching reaches a terminal state, the
    /// same "nothing left to poll, no tick at all" rule every gated subscription here follows.
    ///
    /// [`UPLOAD_POLL_INTERVAL`]: coarse enough that this never competes with the transfer
    /// itself for anything, fine enough that a completion toast or the titlebar indicator
    /// clearing reads as prompt. The child's own tray counter is the fine-grained readout;
    /// this is only for a document that may not even be visible in the same moment the upload
    /// finishes. It is ALSO what retires a meter the user has dismissed (DRAGON-507), which is
    /// why the two durations are pinned against each other where the shorter one is defined.
    fn sub_upload_poll(&self) -> Option<Subscription<Msg>> {
        let ticks: Vec<_> = self
            .previews
            .iter()
            .filter(|p| p.edit.uploads.iter().any(crate::app::preview::edit::upload_needs_poll))
            .map(|p| {
                let id = p.window;
                cosmic::iced::time::every(UPLOAD_POLL_INTERVAL)
                    .with(id)
                    .map(|(id, _)| Msg::Preview(id, PreviewMsg::UploadPoll))
            })
            .collect();
        if ticks.is_empty() {
            return None;
        }
        Some(Subscription::batch(ticks))
    }

    /// Drive the upload meter's finalize stripe sweep (DRAGON-537): while any of a
    /// document's meters sits in its finalize wait (`MeterFace::Finalizing`, the 99 hold
    /// while the provider commits), tick ~30fps to advance the phase, the same cadence and
    /// shape as the folder refresh glyph's spin (`FolderSpinTick`). Per-document like
    /// `sub_upload_poll`, and gone the instant no meter is finalizing, so the fast tick
    /// exists only for the few seconds it is actually animating something.
    fn sub_upload_finalize_anim(&self) -> Option<Subscription<Msg>> {
        const UPLOAD_ANIM_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);
        let ticks: Vec<_> = self
            .previews
            .iter()
            .filter(|p| {
                p.edit.uploads.iter().any(|w| {
                    matches!(
                        crate::app::preview::edit::meter_face(w),
                        crate::app::preview::edit::MeterFace::Finalizing
                    )
                })
            })
            .map(|p| {
                let id = p.window;
                cosmic::iced::time::every(UPLOAD_ANIM_INTERVAL)
                    .with(id)
                    .map(|(id, _)| Msg::Preview(id, PreviewMsg::UploadAnimTick))
            })
            .collect();
        if ticks.is_empty() {
            return None;
        }
        Some(Subscription::batch(ticks))
    }

    // DRAGON-371: `sub_preview_close_hold` lived here — a 50ms per-document tick that drove
    // the 1s hold a close-after-copy/delete used to sit through so its success toast could be
    // read. The hold is gone (the share closes the moment its work succeeds), so the tick,
    // its `PendingClose` condition and its `CloseHoldElapsed` message went with it. Nothing
    // times a close now; if a timed close ever returns it needs a subscription of its own,
    // not a revival of this one.

    /// Blink the text-annotation caret (DRAGON-354): while a preview has a live text edit, tick
    /// every ~530ms so the caret flashes at the familiar rate. Same per-document shape as the
    /// toast tick; it vanishes the moment nothing is being edited.
    fn sub_text_caret_blink(&self) -> Option<Subscription<Msg>> {
        let ticks: Vec<_> = self
            .previews
            .iter()
            .filter(|p| p.edit.text_edit.is_some())
            .map(|p| {
                let id = p.window;
                cosmic::iced::time::every(std::time::Duration::from_millis(530))
                    .with(id)
                    .map(|(id, _)| Msg::Preview(id, PreviewMsg::TextCaretBlink))
            })
            .collect();
        if ticks.is_empty() {
            return None;
        }
        Some(Subscription::batch(ticks))
    }

    /// Pull decoded frames from the playback worker into the view while a recording
    /// is playing inline — polled at ~2× the source fps so frames advance smoothly.
    fn sub_playback_poll(&self) -> Option<Subscription<Msg>> {
        // One poll PER playing preview, at ITS own source fps (DRAGON-336 phase 2). Each
        // gets a DISTINCT hash id via `with(window_id)` — without that iced would treat
        // two same-interval timers as ONE subscription — and each tick is addressed to the
        // preview that owns the stream.
        let polls: Vec<_> = self
            .previews
            .iter()
            .filter_map(|p| p.playback_poll().map(|iv| (p.window, iv)))
            .map(|(id, interval)| {
                cosmic::iced::time::every(interval)
                    .with(id)
                    .map(|(id, _)| Msg::Preview(id, PreviewMsg::PlayerTick))
            })
            .collect();
        if polls.is_empty() {
            return None;
        }
        Some(Subscription::batch(polls))
    }

    /// While the recording controls live in the system tray, drain menu clicks
    /// promptly (a click → action within ~80ms, imperceptible).
    ///
    /// DRAGON-583: the same tick also drains the recording-CONTROL inlet, so a global
    /// hotkey running `--toggle-mic` acts within the same ~80ms. It is a second gate rather
    /// than a second subscription because both feed the one `TrayPoll` handler, and it has
    /// to be a gate of its own: a sandboxed child can fail to register a tray item, and
    /// that is precisely the session where the CLI commands are the only control left.
    fn sub_tray_poll(&self) -> Option<Subscription<Msg>> {
        #[cfg(target_os = "linux")]
        let listening = self.tray.is_some() || self.record_control.is_some();
        #[cfg(not(target_os = "linux"))]
        let listening = self.tray.is_some();
        if listening {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(80))
                    .map(|_| Msg::Recording(RecordingMsg::TrayPoll)),
            )
        } else {
            None
        }
    }

    /// DRAGON-563: while the countdown digits live in the tray (any session), drain the
    /// item's Cancel clicks at the same ~80ms cadence as `sub_tray_poll`. Gated on the
    /// item existing, so a session with no tray host (and every moment outside a
    /// countdown) never ticks.
    fn sub_countdown_tray_poll(&self) -> Option<Subscription<Msg>> {
        if self.countdown_tray.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(80))
                    .map(|_| Msg::Capture(CaptureMsg::CountdownTrayPoll)),
            )
        } else {
            None
        }
    }

    /// DRAGON-336: while this process HOSTS handoffs, drain the inbound channel promptly —
    /// the sibling is blocked waiting for our ack, so this interval is the floor on how long
    /// its handoff takes (and, on a miss, how long before it gives up and does the job
    /// itself). 100ms is imperceptible against a capture's own save, and is well inside
    /// `preview_ipc::ACK_TIMEOUT`. Gated on the listener existing, exactly like
    /// `sub_tray_poll` gates on the tray: no window open ⇒ no host ⇒ no tick.
    ///
    /// DRAGON-613 widened WHAT can be waiting on it (a colour for the picker window as well
    /// as a capture for a preview) without changing the tick or the gate: one listener, one
    /// poll, one drain.
    #[cfg(unix)]
    fn sub_preview_handoff(&self) -> Option<Subscription<Msg>> {
        if self.handoff_host.is_some() {
            Some(
                cosmic::iced::time::every(std::time::Duration::from_millis(100))
                    .map(|_| Msg::Capture(CaptureMsg::HandoffPoll)),
            )
        } else {
            None
        }
    }

    /// Off unix there is no handoff transport, so nothing ever hosts — a `None` stub so the
    /// batch call site stays platform-uniform (Windows' subscription set is unchanged).
    #[cfg(not(unix))]
    fn sub_preview_handoff(&self) -> Option<Subscription<Msg>> {
        None
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
        let overlay_gone = !self.previews.is_empty();
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

