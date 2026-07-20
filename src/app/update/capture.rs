//! `CaptureMsg` handling — the capture overlay's mode/toggle/commit flow.
//! Split from `application.rs` (DRAGON-115); one file per message domain,
//! mirroring `app/message/`.

use super::super::*;

impl App {
    pub(in crate::app) fn update_capture(&mut self, message: CaptureMsg) -> Task<cosmic::Action<Msg>> {
        match message {
            CaptureMsg::SetMode(m) => {
                self.mode = m;
                // Switching what we're selecting (region/window/monitor) re-homes
                // the toolbar to that mode's anchor, like redrawing a region does.
                self.toolbar_offset.clear();
                // DRAGON-204: the window-picker thumbnails are grabbed by a ~1s
                // SCK-serialized pre-capture that only window mode consumes, so a
                // non-window launch DEFERS it off the launch critical path. Kick it
                // lazily the FIRST time the user switches into window mode (unless the
                // portal picker replaces our native picker — Linux window mode via
                // PipeWire needs no local thumbnails). The existing loading spinner
                // covers the wait; the LoadingTick drain lands the thumbnails as usual.
                if m == Mode::Window && !self.mode_uses_portal() {
                    self.kick_window_precapture();
                }
                // PipeWire source: monitor/window are picked through the portal (not
                // our native overlay picker), so launch it when the icon is chosen.
                if self.mode_uses_portal()
                    && let Some(task) = self.portal_for_mode(m)
                {
                    return task;
                }
                Task::none()
            }
            CaptureMsg::SetKind(k) => {
                if self.kind == Kind::Scanner && k != Kind::Scanner {
                    // Leaving scanner: the processed marks stay CACHED (their region
                    // keys too), so returning shows them instantly without a rescan;
                    // only transient interaction state drops.
                    self.hovered_mark = None;
                    self.hovered_word = None;
                    self.text_sel.clear();
                    self.text_menu = None;
                }
                self.kind = k;
                if k == Kind::Scanner {
                    // Scanning is region work; the mode group is hidden in scanner
                    // kind, so pin the mode here.
                    self.mode = Mode::Region;
                }
                // Meters are only armed in video mode; (de)activate accordingly.
                self.sync_meters();
                // Switching kind while in monitor/window mode arms the portal picker
                // for the new kind's source, mirroring the mode-select behaviour.
                if self.mode_uses_portal()
                    && let Some(task) = self.portal_for_mode(self.mode)
                {
                    return task;
                }
                Task::none()
            }
            CaptureMsg::HoverOutput(name) => {
                if self.hovered_output.as_deref() != Some(name.as_str()) {
                    self.hovered_output = Some(name);
                }
                Task::none()
            }
            CaptureMsg::LoadingTick => {
                // Pick up the pre-capture result the moment the thread posts it.
                if let Some((
                    windows,
                    origin,
                    wallpaper_px,
                    frozen_win_px,
                    frozen_toplevels,
                )) = self.precapture.lock().ok().and_then(|mut g| g.take())
                {
                    self.windows = windows;
                    self.origin_window = origin;
                    // The launch-locked cursor is NOT carried here anymore (DRAGON-213):
                    // it rides its own dedicated launch thread and is drained via
                    // `CursorReady` (which builds `frozen_cursor_handle` once), so it
                    // stays locked at LAUNCH instead of at whenever this pre-capture lands.
                    self.frozen_win_px = frozen_win_px;
                    self.frozen_toplevels = frozen_toplevels;
                    // Wrap each output's pre-resolved pixels in a handle that SHARES
                    // the Arc's allocation (no decode, no ~30 MB byte clone — the
                    // source Arc and this handle are the same buffer). Per output so
                    // each display's picker shows its OWN wallpaper (DRAGON-195).
                    // On macOS the wallpaper is DEFERRED off this path (it lands via
                    // `WallpaperReady`, DRAGON-200), so this map is empty here — don't
                    // clobber an already-drained deferred wallpaper with it. Linux
                    // carries the real (possibly-empty) map here, byte-identical.
                    #[cfg(not(target_os = "macos"))]
                    {
                        self.wallpaper_handles = wallpaper_handles_from_px(wallpaper_px);
                    }
                    #[cfg(target_os = "macos")]
                    if precapture_should_assign_wallpaper(&wallpaper_px) {
                        // Empty in normal operation (deferred path owns it); this only
                        // fires if a future inline mac resolve ever carried real pixels.
                        self.wallpaper_handles = wallpaper_handles_from_px(wallpaper_px);
                    }
                    self.windows_loading = false;
                    // Keep the loading overlay up a few frames so the picker can
                    // render (GPU-upload) behind it before it lifts — no flash.
                    self.window_warmup = 3;
                } else if self.window_warmup > 0 {
                    self.window_warmup -= 1;
                }
                Task::none()
            }
            CaptureMsg::FrozenReady => {
                // macOS (DRAGON-148 option C): the deferred flats grab landed. Drain
                // it into `self.frozen`; a redraw follows automatically (state change),
                // so the overlay switches from the live (dimmed) screen to the still.
                if let Some(flats) = self.frozen_slot.lock().ok().and_then(|mut g| g.take()) {
                    crate::util::timing_mark("FrozenReady: deferred flats drained into self.frozen");
                    self.frozen = flats;
                    self.frozen_pending = false;
                }
                Task::none()
            }
            CaptureMsg::CursorReady => {
                // DRAGON-213: the dedicated launch cursor grab landed. Drain it into
                // `frozen_cursor` (+ its display handle, built once here); the region
                // selector's on-overlay indicator and every capture path that stamps the
                // launch-locked pointer read it. A redraw follows automatically.
                self.drain_cursor_slot();
                Task::none()
            }
            CaptureMsg::WindowGrabbed(dims) => {
                // DRAGON-216: a focus-neutral overlay spinner was pre-opened at pick commit
                // and shown DURING the grab; now the grab is done, resolve it per the preview
                // appearance (Linux only; the flag is never set on macOS). OVERLAY mode
                // promotes the same surface to interactive (Exclusive keyboard) — no flicker.
                // WINDOWED mode swaps it for the real preview window, keeping the overlay's
                // cover up until the window maps (no desktop flash). Takes precedence over the
                // deferred-open path below.
                if std::mem::take(&mut self.window_spinner_neutral) {
                    return self.resolve_neutral_spinner();
                }
                // DRAGON-216/219 (macOS): the FULLSCREEN overlay cover was pre-opened
                // focus-neutral (placed + ordered front non-key, no `gain_focus`) to cover the
                // grab for BOTH appearances; the grab is done, so take focus for real now.
                // WINDOWED swaps the cover for the real preview window (minted now that
                // activation is safe, kept UNDER the cover until its first configure closes it —
                // no flash); OVERLAY just `gain_focus`es the cover, which IS the preview.
                // Returning here skips the deferred open below (no second spinner). A pre-open
                // that produced no surface falls through to that deferred open as a fallback.
                #[cfg(target_os = "macos")]
                if std::mem::take(&mut self.mac_preview_preopen)
                    && let Some(id) = self.preview.as_ref().map(|p| p.window)
                {
                    return if self.preview_windowed {
                        // DEFER the swap to `present_capture` (DRAGON-221 follow-up):
                        // the composed dims land with ShotSaved; the window then opens
                        // once at its correct size (same rule as the Linux arm).
                        self.windowed_swap_pending = true;
                        Task::none()
                    } else {
                        window::gain_focus(id)
                    };
                }
                // DRAGON-215: the off-thread window focus-then-grab finished. Raise the
                // preview spinner NOW (after the grab, so its focus steal can't clobber the
                // DRAGON-194 focus the grab depended on) to cover the remaining compose/
                // save/decode; `None` means it was already pre-opened as the defocus sink.
                match dims {
                    Some(d) => self.open_preview_spinner(
                        preview::PreviewKind::Image(preview::ImagePreview::loading()),
                        Some(d),
                    ),
                    None => Task::none(),
                }
            }
            CaptureMsg::WallpaperReady => {
                // macOS (DRAGON-200): the deferred per-output picker wallpaper landed
                // (grabbed AFTER the frozen flats so it never delayed the region still).
                // Wrap each output's pixels into a share-the-allocation handle and drop
                // them into `wallpaper_handles`; the window picker (if entered) swaps its
                // dark fill for the real wallpaper on the next redraw (state change).
                if let Some(px) = self.wallpaper_slot.lock().ok().and_then(|mut g| g.take()) {
                    crate::util::timing_mark("WallpaperReady: deferred wallpaper drained into handles");
                    self.wallpaper_handles = wallpaper_handles_from_px(px);
                    self.wallpaper_pending = false;
                }
                Task::none()
            }
            // macOS (DRAGON-151): re-solidify the overlay whose toolbar chip is under
            // the pointer; everything else stays click-through. Linux never constructs
            // this message (layer-shell input zones do this natively).
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            CaptureMsg::PassthroughPoll => self.passthrough_poll(),
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            CaptureMsg::PassthroughPoll => Task::none(),
            CaptureMsg::SetHover(h) => {
                self.hover = h;
                Task::none()
            }
            CaptureMsg::ToggleDelayMenu => {
                self.delay_menu_open = !self.delay_menu_open;
                Task::none()
            }
            CaptureMsg::PickDelay(i) => {
                self.delay_idx = i.min(DELAYS.len() - 1);
                // Choosing a preset from the UI takes over from any CLI `--countdown`.
                self.countdown_override = None;
                self.delay_menu_open = false;
                self.save_state();
                Task::none()
            }
            CaptureMsg::Tick => match self.countdown {
                Some(n) if n > 1 => {
                    self.countdown = Some(n - 1);
                    Task::none()
                }
                Some(_) => {
                    self.countdown = None;
                    match self.pending.take() {
                        Some(sel) => self.begin(sel),
                        None => self.teardown(),
                    }
                }
                None => Task::none(),
            },
            CaptureMsg::CancelCapture => {
                // Abort the countdown and return to region select (don't quit),
                // restoring the fully-interactive selection overlay.
                self.countdown = None;
                self.pending = None;
                self.capture_live = false;
                // Drop any granted-but-unused portal stream (closes the fd).
                self.pw_held = None;
                self.pw_pending = None;
                self.mode = Mode::Region;
                self.restore_interactive_overlays()
            }
            CaptureMsg::RegionChange(r) => {
                self.region = Some(r);
                self.region_dragging = true;
                // A fresh region selection re-homes the toolbar to its anchor.
                self.toolbar_offset.clear();
                Task::none()
            }
            CaptureMsg::ToolbarPan(output, dx, dy) => {
                let off = self.toolbar_offset.entry(output).or_insert((0.0, 0.0));
                off.0 += dx;
                off.1 += dy;
                Task::none()
            }
            CaptureMsg::ToolbarDragEnd => {
                // The chip moved; while active the overlay is click-through except
                // the toolbar's region, so rebuild that region at the new spot.
                if self.countdown.is_some() || self.recording.is_some() {
                    self.recreate_active_overlays()
                } else {
                    Task::none()
                }
            }
            CaptureMsg::RegionDone => {
                self.region_dragging = false;
                self.save_state();
                Task::none()
            }
            CaptureMsg::PipewireProbed(ok, types) => {
                self.pipewire_available = ok;
                self.pipewire_source_types = types;
                // Portal reachability feeds the health check → refresh the nav icon,
                // and changes which backends the method dropdowns list.
                self.update_health_nav_icon();
                self.rebuild_capture_methods();
                // First launch only: recordings default to the portal when it's
                // reachable, otherwise the native backend (on macOS `ok` is always
                // false, so this resolves to SCK). Screenshots default to native —
                // UNLESS the compositor doesn't advertise screencopy (GNOME/KDE),
                // where the portal is the only path that can work. Never overrides
                // a saved choice.
                if self.first_launch {
                    self.first_launch = false;
                    self.record_backend = if ok {
                        crate::platform::backend::PORTAL_ID
                    } else {
                        crate::platform::backend::native_backend_id()
                    }
                    .to_string();
                    // The screenshot default only differs from native when the
                    // compositor lacks ext-image-copy-capture — a Wayland-only probe.
                    // On macOS the only backend is SCK, so there's nothing to fall
                    // back FROM.
                    #[cfg(target_os = "linux")]
                    {
                        let p = crate::platform::backend::wayland_protocols();
                        if ok && !(p.image_copy_capture && p.output_source) {
                            self.screenshot_backend =
                                crate::platform::backend::PORTAL_ID.to_string();
                        }
                    }
                    self.save_state();
                }
                Task::none()
            }
            CaptureMsg::PipewireCastReady => self.on_pipewire_cast_ready(),
            CaptureMsg::ShotSaved(path, ok) => {
                if !ok {
                    log::warn!("async screenshot failed to save");
                    return self.finish_session();
                }
                // Restore focus to where we launched, then share (same as the
                // direct screencopy screenshot path).
                if let Some(id) = &self.origin_window {
                    crate::platform::compositor::activate(id);
                }
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                // Cheap header read (no full decode) so the windowed preview opens sized.
                let dims = ::image::image_dimensions(&path).ok();
                self.present_capture(path, size, false, dims)
            }
            CaptureMsg::DismissToast => {
                self.toast = None;
                Task::none()
            }
            CaptureMsg::Capture { output } => self.capture(&output),
            CaptureMsg::CaptureWindow { id, rect } => {
                // Capture the toplevel directly by id — no focus, occlusion-proof.
                let sel = Selection {
                    x: rect.0,
                    y: rect.1,
                    width: rect.2.max(1) as u32,
                    height: rect.3.max(1) as u32,
                    output: None,
                    window_id: Some(id),
                };
                self.run_capture(sel)
            }
            CaptureMsg::DoPixelCapture => self.do_pixel_capture(),
            CaptureMsg::CopySelection => {
                // Region quick-action: capture the CURRENTLY drawn selection, force-copy
                // it, skip the preview, and finish. No-op with nothing drawn (the keymap
                // lane already guards this, but the message could arrive by other means).
                if self.mode != Mode::Region || self.normalized_region().is_none() {
                    return Task::none();
                }
                // Route through the SAME capture plumbing as a normal region commit; the
                // completion share reads this flag and force-copies + finishes instead of
                // opening the preview. `capture` builds the selection from the drawn
                // region in region mode (the passed output name is ignored there).
                self.copy_selection_pending = true;
                self.capture("")
            }
        }
    }

    /// DRAGON-204: kick the DEFERRED window pre-capture (gather toplevels + per-window
    /// grabs -> thumbnails) the first time the user enters window mode on a launch that
    /// deferred it (region / monitor / scan). Idempotent: a second switch into window
    /// mode does nothing (the thumbnails are already loaded or in flight). Arms the
    /// picker's loading spinner (`windows_loading`), which the `sub_loading_tick` poll
    /// then drains via `LoadingTick` exactly like a window-mode launch does. A no-op
    /// when the pre-capture already ran at launch (window-mode launch).
    /// Drain the dedicated launch cursor grab (DRAGON-213) into `frozen_cursor`, building
    /// its display handle ONCE here (view() must never mint a handle — a fresh id per
    /// frame forces a GPU re-upload while the indicator shows). Idempotent: once drained,
    /// `cursor_pending` is cleared and the poll subscription stops. Called both by the
    /// `CursorReady` poll AND synchronously at commit (`await_cursor`) so a very fast
    /// commit that raced the poll still stamps the launch-locked pointer. Returns whether
    /// the slot was drained this call.
    pub(in crate::app) fn drain_cursor_slot(&mut self) -> bool {
        let Some(cur) = self.cursor_slot.lock().ok().and_then(|mut g| g.take()) else {
            return false;
        };
        crate::util::timing_mark("CursorReady: dedicated launch cursor drained into frozen_cursor");
        // `..` tolerates the macOS `CursorSprite`'s trailing sprite-scale element
        // (DRAGON-156); Linux's is a 3-tuple.
        self.frozen_cursor_handle = cur.as_ref().map(|(img, ..)| {
            widget::image::Handle::from_rgba(img.width(), img.height(), img.as_raw().clone())
        });
        self.frozen_cursor = cur;
        self.cursor_pending = false;
        true
    }

    fn kick_window_precapture(&mut self) {
        if self.window_precapture_started {
            return;
        }
        crate::util::timing_mark("kick_window_precapture: lazy window pre-capture (begin)");
        self.window_precapture_started = true;
        self.windows_loading = true;
        spawn_window_precapture(
            self.precapture.clone(),
            self.freeze,
            wallpaper_path(),
            self.window_radius,
        );
    }
}
