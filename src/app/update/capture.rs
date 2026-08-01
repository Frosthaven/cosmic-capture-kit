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
            CaptureMsg::ExternalRecordingTick => {
                // DRAGON-322: refresh the cross-process recording flag; if a recording
                // just started elsewhere while we sit in video mode, fall back to image
                // so we can't launch a second recording.
                self.external_recording = crate::instance::any_other_recording();
                if self.external_recording && self.kind == Kind::Video {
                    self.kind = Kind::Image;
                    self.sync_meters();
                }
                Task::none()
            }
            CaptureMsg::SetKind(k) => {
                // DRAGON-322: never enter video mode while another instance is recording.
                if k == Kind::Video && self.external_recording {
                    return Task::none();
                }
                if self.kind == Kind::Scanner && k != Kind::Scanner {
                    // Leaving scanner: the processed marks stay CACHED (their region
                    // keys too), so returning shows them instantly without a rescan;
                    // only transient interaction state drops.
                    self.hovered_mark = None;
                    self.hovered_word = None;
                    self.text_sel.clear();
                    self.text_menu = None;
                }
                // DRAGON-336: the QR/OCR scanners crop their scan source out of the
                // frozen flats, which a freeze-off non-scanner launch no longer grabs.
                // Entering the scanner is therefore the one live toggle that must
                // acquire them lazily (the same shape as the deferred window
                // pre-capture below). Checked BEFORE `self.kind` moves, so it fires
                // once per entry into the scanner, not on every scanner-kind click.
                if k == Kind::Scanner && self.kind != Kind::Scanner {
                    self.kick_frozen_flats();
                }
                // DRAGON-456: the SAME button, pressed again while the scanner is already
                // open, re-reads the screen. There is no kind to switch to, so the press
                // would otherwise be inert — and a stale scan is exactly the state a user
                // reaches for a button in. Everything below still runs (it is all
                // idempotent for a Scanner->Scanner press).
                if scan_press_refreshes(self.kind, k) {
                    self.begin_scan_refresh();
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
            // DRAGON-336: another process handed us its finished capture — open it as a
            // new preview document (and ack it) in the preview domain.
            CaptureMsg::HandoffPoll => self.drain_preview_handoffs(),
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
                    // DRAGON-456: a REFRESH that came back with NOTHING (the re-grab failed —
                    // no compositor connection, no outputs) must not destroy the working
                    // snapshot the scanner is currently reading. Keep what we have and leave
                    // the scan untouched: a failed refresh is a no-op, never a downgrade.
                    // Only the refresh path can decline the delivery — the launch/lazy grabs
                    // still assign unconditionally, including their deliberate EMPTY
                    // placeholder (`acquire_scene`), so their behavior is unchanged.
                    let refreshed = frozen_delivery_accepted(
                        self.scan_refresh,
                        flats.is_empty(),
                        self.frozen.is_empty(),
                    );
                    if refreshed {
                        self.frozen = flats;
                    }
                    self.frozen_pending = false;
                    // If these flats are a user-requested re-read, the scan keys are cleared
                    // HERE and nowhere earlier — see `finish_scan_refresh`.
                    self.finish_scan_refresh(refreshed);
                }
                Task::none()
            }
            // DRAGON-456: the overlay has had a frame to paint nothing; the screen is clean
            // of our own UI, so take the new snapshot now.
            CaptureMsg::ScanRefreshTick => {
                self.run_scan_refresh_grab();
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
                    && let Some(id) = self.capture_preview
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
                // DRAGON-305 (Windows): the fullscreen blocker cover was pre-opened NON-ACTIVATING
                // (so it couldn't steal the target's foreground during the active-appearance grab).
                // The grab is done, so — like the mac windowed arm — DEFER the cover→window swap to
                // `present_capture`, where the COMPOSED dims are known (the window then opens once at
                // its correct size); the cover keeps painting `grab_cover_view` until the window maps.
                // Only WINDOWED picks pre-open (`win_preview_preopen`), so there is no overlay arm here.
                #[cfg(windows)]
                if std::mem::take(&mut self.win_preview_preopen) {
                    self.windowed_swap_pending = true;
                    return Task::none();
                }
                // DRAGON-215: the off-thread window focus-then-grab finished. Raise the
                // preview spinner NOW (after the grab, so its focus steal can't clobber the
                // DRAGON-194 focus the grab depended on) to cover the remaining compose/
                // save/decode; `None` means it was already pre-opened as the defocus sink.
                // DRAGON-428: no editor is coming on a `--no-editor` launch, so raise no
                // spinner to cover its arrival — the capture goes straight to
                // save + clipboard + notify from `present_capture`.
                match dims {
                    Some(d) if !self.no_editor => self.open_preview_spinner(
                        preview::PreviewKind::Image(preview::ImagePreview::loading()),
                        Some(d),
                    ),
                    _ => Task::none(),
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
            CaptureMsg::ShotSaved(path, outcome) => {
                if let Some(failure) = outcome.failure() {
                    // DRAGON-419 (silent-exit path S2) + DRAGON-415. This was ONE boolean
                    // collapsing three genuinely different situations, into a silent exit.
                    // `ShotOutcome` now names which of the three the worker actually hit, so
                    // the note below is the RIGHT code rather than "grab returned nothing, or
                    // the write failed" — and the alert can offer the matching advice. A
                    // worker panic was already noted at its own seam in `capture_flow` (it
                    // fires first and stays the root cause), so it is not re-noted here.
                    log::warn!("async screenshot did not deliver: {outcome:?}");
                    if failure != crate::diag::Failure::WorkerPanic {
                        crate::diag::note_failure(
                            failure,
                            &format!(
                                "async capture reported {outcome:?}; target {}",
                                crate::diag::path_shape(&path),
                            ),
                        );
                    }
                    return self.fail_session();
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
            CaptureMsg::RunImmediate => {
                // Linux: the deferred overlay-less immediate capture, now that
                // the full output list has settled into `self.outputs`. Resolve + drive it;
                // if it can't resolve a target, mint the picker overlays instead so the user
                // can still pick (the deferral suppressed the picker while it was pending).
                #[cfg(target_os = "linux")]
                {
                    if let Some(imm) = self.startup_immediate.take() {
                        if let Some(task) = self.immediate_capture(imm) {
                            return task;
                        }
                        log::warn!(
                            "immediate capture ({imm:?}) could not resolve a target; \
                             falling back to the picker overlay"
                        );
                        return self.mint_startup_pickers();
                    }
                }
                Task::none()
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

    /// DRAGON-336: kick a LAZY frozen-flats grab for a launch that skipped it
    /// (`launch_flats_needed` false — freeze off, non-scanner kind). Mirrors
    /// [`Self::kick_window_precapture`]: spawn the same grab onto its own OS thread,
    /// deposit into `frozen_slot`, and re-arm `frozen_pending` so the existing
    /// `FrozenReady` drain poll lands it exactly like a launch grab.
    ///
    /// LIMITATION (deliberate, and why this is only wired to the SCANNER): a lazy grab
    /// is NOT the launch instant — our capture overlay is already mapped, so the flat it
    /// returns carries the region selector's dim wash everywhere OUTSIDE the currently
    /// drawn selection. The selection's INTERIOR is drawn fully transparent
    /// (`RegionSelection::draw` fills only the four surrounding bands), so the crop the
    /// scanner actually reads is clean whenever a region is already drawn at kick time —
    /// the common case, since the region is restored from the persisted state at launch.
    /// A region drawn AFTER this grab, over screen area that was dimmed, scans a dimmed
    /// source (QR still decodes; OCR degrades). That is strictly better than the
    /// alternative (no flats at all = the scanner silently never produces a mark), but it
    /// is the one place where a lazy flat is not equivalent to a launch flat.
    ///
    /// The FREEZE setting deliberately does NOT call this: a freeze toggled on mid-session
    /// would snapshot the settings window sitting over the desktop and then show/capture
    /// that as the "frozen screen" — corrupting, where the no-flats fallback (`freezing()`
    /// is false on an empty map, so the capture stays live) is merely inert until the next
    /// launch.
    pub(in crate::app) fn kick_frozen_flats(&mut self) {
        if !self.frozen.is_empty() {
            // A freeze / scanner launch already grabbed them; nothing to do.
            return;
        }
        crate::util::timing_mark("kick_frozen_flats: lazy frozen-flats grab (begin)");
        let slot = self.frozen_slot.clone();
        // Clear the EMPTY placeholder `acquire_scene` parked here for the skipped grab:
        // if the drain poll took it after we re-armed `frozen_pending`, it would clear
        // the flag again and stop polling before the real flats land.
        if let Ok(mut g) = slot.lock() {
            *g = None;
        }
        self.frozen_pending = true;
        let want_cursor = self.capture_cursor;
        std::thread::spawn(move || {
            let flats = grab_frozen_flats(want_cursor);
            crate::util::timing_mark("kick_frozen_flats: lazy frozen-flats grab (done)");
            if let Ok(mut g) = slot.lock() {
                *g = Some(flats);
            }
        });
    }

    /// DRAGON-456 step 1 of 3: start a user-requested re-read of the screen.
    ///
    /// Only the BLANK happens here. The re-grab reads the composited screen, and our own
    /// overlay is part of that composite — since this ticket it is painting the frozen
    /// backdrop, so grabbing now would photograph the PREVIOUS still and the refresh would
    /// silently do nothing. So the overlay stops painting, and [`Self::run_scan_refresh_grab`]
    /// takes the picture one tick later (`sub_scan_refresh`), the same hide-then-grab shape
    /// `begin_capture` uses before a live capture.
    ///
    /// A grab already in flight wins, and `frozen_pending` is the test for BOTH of them:
    /// re-pressing during the ~1 frame of blank must not start a second thread racing the
    /// first into the same slot, and neither must a press that lands while the LAUNCH (or
    /// lazy) grab is still running — that one is already delivering pixels at least as
    /// fresh as ours, and racing it could leave the older result as the last writer.
    fn begin_scan_refresh(&mut self) {
        if self.scan_refresh != ScanRefresh::Idle || self.frozen_pending {
            return;
        }
        crate::util::timing_mark("scan refresh: blanking the overlay for the re-grab");
        self.scan_refresh = ScanRefresh::Blanking;
    }

    /// DRAGON-456 step 2 of 3: the overlay has painted nothing for a tick, so take the new
    /// snapshot. Deposits into the SAME `frozen_slot` the launch/lazy grabs use, so it lands
    /// through the existing `FrozenReady` drain with no second delivery path.
    fn run_scan_refresh_grab(&mut self) {
        if self.scan_refresh != ScanRefresh::Blanking {
            return;
        }
        self.scan_refresh = ScanRefresh::Grabbing;
        crate::util::timing_mark("scan refresh: re-grab (begin)");
        let slot = self.frozen_slot.clone();
        // Clear anything parked in the slot (the empty placeholder, or a previous
        // delivery) so the drain can only ever see OUR result.
        if let Ok(mut g) = slot.lock() {
            *g = None;
        }
        self.frozen_pending = true;
        let want_cursor = self.capture_cursor;
        std::thread::spawn(move || {
            let flats = grab_frozen_flats(want_cursor);
            crate::util::timing_mark("scan refresh: re-grab (done)");
            if let Ok(mut g) = slot.lock() {
                *g = Some(flats);
            }
        });
    }

    /// DRAGON-456 step 3 of 3: the new flats have landed — un-blank and re-arm the scan.
    ///
    /// The scan keys (`last_code_region` / `last_ocr_region`) are cleared HERE, not at the
    /// press, and the ORDER is the whole point: clearing them early would let the next
    /// `MarksPoll` (250ms) re-scan the OLD flats, re-set the keys, and leave the fresh
    /// pixels unscanned — a refresh that returned the stale answer it was pressed to fix.
    ///
    /// The cached marks are dropped with them. They describe pixels that no longer exist,
    /// and they carry positions the overlay would draw over the new still until the passes
    /// land a beat later.
    ///
    /// `refreshed` false = the re-grab came back empty and was DECLINED (see `FrozenReady`):
    /// un-blank, but leave the scan alone — the pixels it already holds are still the ones
    /// on screen, so re-running the passes would only spend OCR time to reach the same answer.
    fn finish_scan_refresh(&mut self, refreshed: bool) {
        if self.scan_refresh != ScanRefresh::Grabbing {
            return;
        }
        crate::util::timing_mark("scan refresh: new flats landed, re-arming the scan");
        self.scan_refresh = ScanRefresh::Idle;
        if !refreshed {
            return;
        }
        self.last_code_region = None;
        self.last_ocr_region = None;
        self.code_marks.clear();
        self.text_words.clear();
        self.hovered_mark = None;
        self.hovered_word = None;
        self.text_sel.clear();
        self.text_menu = None;
        self.code_menu = None;
        self.rebuild_marks();
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
