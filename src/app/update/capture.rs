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
                // Pressing Monitor or Window on the floating toolbar IS the user
                // choosing a target, so the picker must show: `TargetSwitch` never
                // replays (DRAGON-580; it rode the replaying in-session lane until
                // then, which silently re-granted the seed's own monitor).
                if self.mode_uses_portal()
                    && let Some(task) =
                        self.portal_for_mode(m, crate::app::portal::RequestOrigin::TargetSwitch)
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
                // Entering the scanner, or pressing its button again while it is already
                // open, both mean the same thing now: READ THE SCREEN.
                //
                // DRAGON-336 had to distinguish them — entering kicked a lazy grab of the
                // whole-screen flats, re-pressing (DRAGON-456) re-grabbed them — because the
                // scan source was a crop of those flats. DRAGON-460 made the source a live
                // shot of the selection, so there is one action with one shape, and
                // `begin_scan_shot`'s own `!= Idle` guard makes a double-press harmless.
                //
                // Checked BEFORE `self.kind` moves, so `scan_press_refreshes` still sees the
                // kind we are coming FROM.
                if (k == Kind::Scanner && self.kind != Kind::Scanner)
                    || scan_press_refreshes(self.kind, k)
                {
                    self.begin_scan_shot();
                }
                self.kind = k;
                if k == Kind::Scanner {
                    // Scanning is region work; the mode group is hidden in scanner
                    // kind, so pin the mode here.
                    self.mode = Mode::Region;
                }
                // Meters are only armed in video mode; (de)activate accordingly.
                self.sync_meters();
                // Switching kind while in monitor/window mode re-arms the portal for
                // the new kind's source. Deliberately NOT the mode-select route
                // (DRAGON-580): image ↔ video ↔ scanner does not re-choose the
                // target, so this continues on the already-granted monitor or window
                // and must not raise a second picker for it.
                if self.mode_uses_portal()
                    && let Some(task) =
                        self.portal_for_mode(self.mode, crate::app::portal::RequestOrigin::InSession)
                {
                    return task;
                }
                // DRAGON-604: the `lab/flatpak` fallback overlay never reaches the
                // branch above (it is pinned to Region, and `portal_for_mode` answers
                // None for Region), and its ONE seed frame is also the pixels the
                // scanner decodes. Entering or leaving the scanner changes what those
                // pixels must contain, so re-grab them on the saved grant. No-op on
                // every other session; see `reseed_fallback_for_kind`.
                #[cfg(target_os = "linux")]
                if let Some(task) = self.reseed_fallback_for_kind() {
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
                // Pick up the pre-capture result the moment the thread posts it. Whether it
                // landed on THIS tick is the one effectful input the loading state machine
                // takes; everything it decides from there is pure (`PickerLoad::advance`).
                let landed = if let Some((
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
                    true
                } else {
                    false
                };
                // DRAGON-645: ONE state machine decides whether the spinner is revealed at
                // all, how long it stays once it has been, and when the picker takes over.
                // It replaces the `windows_loading` + `window_warmup` pair, which drew the
                // spinner unconditionally from the pre-capture's first frame and so flashed
                // it on every fast load. The warmup frames that keep the picker's GPU upload
                // hidden are still here, folded into the `Settling` hold.
                let before = self.picker_load;
                self.picker_load = self.picker_load.advance(landed, self.picker_painted.get());
                // BEHAVIOUR, not content, and worth a line of its own: the reveal only
                // happens on a desktop where enumerating the windows really is slow, so its
                // presence in a log says which of the two paths a session took. Its ABSENCE
                // is the fast path, which is the one this ticket is about and the one that
                // cannot be seen any other way (nothing is drawn, so there is nothing on
                // screen to point at).
                if !before.spinner_up() && self.picker_load.spinner_up() {
                    log::debug!(
                        "window picker: the pre-capture outlasted {}ms, so the loading \
                         spinner is being revealed",
                        crate::app::overlay::PICKER_SPINNER_REVEAL_MS
                    );
                }
                Task::none()
            }
            CaptureMsg::FrozenReady => {
                // macOS (DRAGON-148 option C): the deferred flats grab landed. Drain
                // it into `self.frozen`; a redraw follows automatically (state change),
                // so the overlay switches from the live (dimmed) screen to the still.
                if let Some(flats) = self.frozen_slot.lock().ok().and_then(|mut g| g.take()) {
                    crate::util::timing_mark("FrozenReady: deferred flats drained into self.frozen");
                    // DRAGON-460: these flats are the CAPTURE's scene, and nothing else now.
                    // The scanner reads its own live region shot (`scan_shot_slot`), so a
                    // flats delivery can no longer be a scan refresh — it is always the
                    // launch grab, which assigns unconditionally including its deliberate
                    // EMPTY placeholder from `acquire_scene`.
                    //
                    // The DRAGON-456 decline (`frozen_delivery_accepted`) and the re-arm
                    // (`finish_scan_shot`) both went with that: they existed because a
                    // refresh wrote through this same slot, and calling the re-arm here now
                    // would reset a shot in flight the moment unrelated launch flats landed.
                    //
                    // `lab/flatpak`: on the fallback path the launch flats are a NATIVE
                    // grab this session cannot make (empty at best), while `self.frozen`
                    // holds the PORTAL seed frame the selector is drawing over. The
                    // unconditional assign would clobber that frame, and with a restore
                    // token the seed grant is instant, so the race is real. Drop the
                    // delivery there; everywhere else the assign is unchanged.
                    if !self.overlay_fallback_active() {
                        self.frozen = flats;
                    }
                    self.frozen_pending = false;
                }
                // DRAGON-606: THE ordering seam. The grab thread has posted and been
                // drained, so nothing is reading the screen any more and the dim may
                // finally start fading in. Placed here, in the completion handler itself,
                // rather than behind a timer, because "the grab is over" is an event and
                // not a duration. One-way and idempotent, so the repeated drain ticks a
                // pre-parked empty result produces cannot restart it.
                self.start_dim_fade();
                // DRAGON-601: the COLOUR PICKER has been waiting for exactly this.
                //
                // The flats grab is deferred off the launch critical path, so the picker's
                // first sample almost always runs while `frozen_pending` and correctly
                // declines: there is no snapshot to read yet, and saying "this display has no
                // pixel source" there would be a false alarm once per launch. The pointer
                // position IS already known by then (a layer surface mapping under the cursor
                // gets `wl_pointer.enter`, which iced turns into a `CursorMoved`), so the only
                // thing missing was anybody re-asking once the pixels arrived. Nothing did,
                // and the loupe stayed absent until the user moved the mouse and generated a
                // fresh sample by hand. That is the owner's "the circle appears only after I
                // move", arriving a moment after the pointer-hide half of the same report.
                //
                // Guarded by the pure `picker_wants_frozen_resample` so the condition is one
                // readable statement rather than two terms inlined here, and so it can be
                // tested without a compositor.
                if crate::app::color_picker::picker_wants_frozen_resample(
                    self.color_picking(),
                    self.color_picker.pointer.is_some(),
                ) {
                    return self.color_picker_resample();
                }
                Task::none()
            }
            CaptureMsg::ScanSpinTick => {
                // One step per ~33ms tick; a full turn takes about a second. Wrapped so the
                // float cannot drift upward over a long scan.
                //
                // ADDED, and screen space is y-down, so the glyph turns CLOCKWISE. That is
                // why `scan-refresh-symbolic` maps to Lucide's `refresh-cw`: the arrowheads
                // have to point the way the icon actually moves, or the motion reads as an
                // animation bug rather than as a spinner.
                const STEP: f32 = std::f32::consts::TAU / 30.0;
                self.scan_spin = (self.scan_spin + STEP) % std::f32::consts::TAU;
                Task::none()
            }
            // DRAGON-460: the frame without the marks is on screen, so take the region shot.
            CaptureMsg::ScanShotTick => {
                self.run_scan_shot();
                Task::none()
            }
            // DRAGON-600: the tray dropdown may be gone by now, so maybe run the held grab.
            CaptureMsg::MenuHoldTick => {
                self.tick_menu_hold();
                Task::none()
            }
            // DRAGON-606: one frame of the dim's fade-in. The tick exists only to force the
            // redraw; the alpha itself is derived from the start instant at draw time, so a
            // dropped or late frame cannot leave the ramp behind where it should be.
            // DRAGON-612: the held accept's re-ask, from `sub_accept_pending`. Same resolver
            // as the key press (`request_accept`), so the two arrival routes cannot answer
            // differently, and the same reason for asking the gate again here: a session that
            // moved on since the press must not have a capture committed under it.
            CaptureMsg::AcceptPending => self.request_accept(),
            CaptureMsg::DimFadeTick => {
                if let crate::app::overlay::DimFade::Running { elapsed_ms, last } =
                    self.dim_fade.get()
                {
                    // TWO ways this ramp ends, and since DRAGON-644 they are different
                    // questions. The ramp itself advances only on painted frames
                    // (`dim_now`), so `elapsed_ms` is the honest "the animation is over".
                    let finished = elapsed_ms >= crate::app::overlay::DIM_FADE_MS;
                    // And the bound that used to come for free from a wall clock: a surface
                    // that stops being painted at all would otherwise hold this 16ms tick
                    // open for the rest of the session animating nothing. See
                    // `DIM_FADE_ABANDON_MS`.
                    let abandoned = last.elapsed().as_millis() as u64
                        >= crate::app::overlay::DIM_FADE_ABANDON_MS;
                    if finished || abandoned {
                        if abandoned && !finished {
                            log::debug!(
                                "dim fade abandoned: no painted frame for {}ms with the ramp \
                                 {elapsed_ms}ms/{}ms in",
                                last.elapsed().as_millis(),
                                crate::app::overlay::DIM_FADE_MS
                            );
                        }
                        // Latch the end state so `sub_dim_fade` stops scheduling ticks.
                        self.dim_fade.set(crate::app::overlay::DimFade::Done);
                    }
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
                    // DRAGON-563: the tray digits follow the tick.
                    if let Some(tray) = &self.countdown_tray {
                        tray.set_remaining(n - 1);
                    }
                    Task::none()
                }
                Some(_) => {
                    self.countdown = None;
                    // DRAGON-563: the digits come down the moment the capture fires (a
                    // recording mints its own tray next; a still delivers and exits).
                    self.countdown_tray = None;
                    // The one-shot countdown (owner rule; `countdown_consumed` carries
                    // the full why): firing SPENDS the persisted preset, so the next
                    // launch and every menu title read "Countdown Timer: 00". Cancels
                    // keep it; a CLI --countdown never spends it. Written through the
                    // app's own save path, the same one `PickDelay` uses, so the
                    // in-memory chip and the config file move together.
                    if capture_flow::countdown_consumed(
                        true,
                        self.countdown_override.is_some(),
                        self.delay_idx,
                    ) {
                        self.delay_idx = 0;
                        self.save_state();
                    }
                    match self.pending.take() {
                        Some(sel) => self.begin(sel),
                        None => self.teardown(),
                    }
                }
                None => Task::none(),
            },
            // DRAGON-563: drain the countdown tray's clicks. A Cancel is the ordinary
            // Cancelled ending — on the fallback path the selection window is closed, so
            // there is no picker to return to, and the tray menu's "Cancel countdown"
            // reads as "end this" on every session, so this is `teardown`, not
            // `CancelCapture`'s back-to-selection. The failure is noted FIRST with the
            // precise cause (first-note-wins), so the verdict line says "tray cancel",
            // not the generic Escape wording `teardown` would fall back to.
            CaptureMsg::CountdownTrayPoll => {
                let events = self.countdown_tray.as_ref().map(|t| t.poll()).unwrap_or_default();
                if events.contains(&crate::tray::TrayEvent::Cancel) {
                    self.countdown_tray = None;
                    self.countdown = None;
                    self.pending = None;
                    crate::diag::note_failure(
                        crate::diag::Failure::Cancelled,
                        "the user cancelled the pre-capture countdown from the tray",
                    );
                    return self.teardown();
                }
                Task::none()
            }
            CaptureMsg::CancelCapture => {
                // Abort the countdown and return to region select (don't quit),
                // restoring the fully-interactive selection overlay.
                self.countdown = None;
                // DRAGON-563: a countdown abort takes the digits down too.
                self.countdown_tray = None;
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
            // DRAGON-599: one logical pixel, from an arrow key or its vim letter. The geometry
            // is `GlobalRect::nudged`, which moves and never resizes; all this arm decides is
            // WHAT WALLS the move is fought against, which is `desktop_bounds`.
            //
            // Re-checked here rather than trusted from the gate, like `CopySelection`: the
            // message could arrive by another route, and a nudge with no region drawn has
            // nothing to move.
            CaptureMsg::NudgeRegion(dir) => {
                let (Some(rect), Some(bounds)) = (self.region, self.desktop_bounds()) else {
                    return Task::none();
                };
                let (dx, dy) = dir.delta();
                let moved = rect.nudged(dx, dy, bounds);
                // A refused step (the region is against that wall) writes nothing, so a held
                // key at a wall does not churn state or force redraws.
                if moved != rect.normalize() {
                    self.region = Some(moved);
                }
                Task::none()
            }
            CaptureMsg::PipewireProbed(ok, types) => {
                self.pipewire_available = ok;
                self.pipewire_source_types = types;
                // Portal reachability feeds the health check → refresh the nav icon,
                // and changes which backends the method dropdowns list.
                self.update_health_nav_icon();
                self.rebuild_capture_methods();
                // A first-launch auto-default used to live here (DRAGON-129): it
                // wrote the probe's pick into `record_backend` /
                // `screenshot_backend` and saved, once, keyed on "no config file
                // existed at init". DRAGON-575 removed it, for two reasons. It
                // persisted an AUTO-RESOLUTION as the user's intent, the exact
                // disease DRAGON-571 cured for encoders (a transient portal outage
                // at first launch pinned "screencopy" as the recording choice
                // forever). And its trigger was fragile: ANY config file created
                // before the first GUI probe disarmed it for good. The Flatpak
                // recipe's `resident = true` seed did exactly that, which left the
                // sandbox's screenshot method on the unavailable native default and
                // the settings dropdown with no selection. Nothing replaces the
                // write: the schema defaults plus the dispatch clamp
                // (`screenshot_uses_portal` / `recording_uses_portal`) already land
                // every session on the same method the old heal picked, re-derived
                // each launch instead of frozen at the first one, and the method
                // dropdowns DISPLAY that resolution (`MethodChoices::display_position`).
                Task::none()
            }
            CaptureMsg::PipewireCastReady => self.on_pipewire_cast_ready(),
            #[cfg(target_os = "linux")]
            CaptureMsg::FallbackCastReady => self.on_fallback_cast_ready(),
            #[cfg(target_os = "linux")]
            CaptureMsg::FallbackFrozenReady => self.on_fallback_frozen_ready(),
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
            CaptureMsg::CopySelection => {
                // DRAGON-479: capture the CURRENTLY drawn region and skip the editor. The
                // chord's lane in `keyboard.rs` already applied `region_copy_fires`; re-check
                // the two conditions the capture itself cannot survive without, because a
                // message can arrive by routes the lane does not own.
                if self.mode != Mode::Region || self.normalized_region().is_none() {
                    return Task::none();
                }
                // Route through the SAME capture plumbing as a normal region commit — nothing
                // about the grab, the compose or the save changes. The one-shot flag below is
                // read at the very top of `present_capture`, where it overrides the
                // skip-the-editor decision `no_editor` normally makes alone, so this delivers
                // through `finish_share` (clipboard + notification) and ends the session.
                // `capture` builds the selection from the drawn region in region mode, so the
                // output name it takes is ignored here.
                self.copy_selection_pending = true;
                self.capture("")
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
                // A session with no protocol access at all never reaches the picker:
                // `immediate_capture` ends it through the honest failure path instead
                // (lab/flatpak, `immediate_target_resolvable`).
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
    /// picker's loading state (`picker_load`), which the `sub_loading_tick` poll then
    /// drains via `LoadingTick` exactly like a window-mode launch does. A no-op when the
    /// pre-capture already ran at launch (window-mode launch).
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

    /// DRAGON-460 step 1 of 2: start a read of the screen.
    ///
    /// Only the mark CLEAR happens here. The shot reads the composited screen, and the one
    /// thing of ours inside the crop is the scanner's own marks — QR boxes and text-word
    /// highlights, drawn over the very pixels about to be photographed. Shooting with those
    /// up would scan our own highlights and feed them back in. So they go now, and
    /// [`Self::run_scan_shot`] takes the picture one tick later (`sub_scan_shot`), by which
    /// time the frame without them has reached the screen.
    ///
    /// Clearing rather than hiding is deliberate: a shot is only ever taken because the
    /// region changed or the user asked for a re-read, and in both cases the marks on
    /// screen describe pixels that are about to stop being the answer.
    ///
    /// **Nothing else is hidden, and that is the point of DRAGON-460.** The rest of the
    /// overlay keeps painting. `RegionSelection::draw` fills the dim as four bands AROUND
    /// the selection and never fills the interior, so the crop is clean with the chrome
    /// still up. DRAGON-456 had to blank everything only because it re-read the whole
    /// screen; reading just the selection removes the reason.
    ///
    /// A shot already in flight wins — re-pressing during the clear must not start a second
    /// one racing the first into the same slot.
    pub(in crate::app) fn begin_scan_shot(&mut self) {
        if self.scan_shot != ScanShot::Idle {
            return;
        }
        crate::util::timing_mark("scan shot: clearing marks for the read");
        self.scan_shot = ScanShot::Clearing;
        // The marks (and the keys that say which region they describe) go together, so the
        // poll below cannot decide the region is unchanged and skip the re-read.
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

    /// DRAGON-600: one poll of the tray-dropdown hold on the frozen-flats grab.
    ///
    /// Releases when [`super::super::capture_flow::menu_hold_release`] says so, which is
    /// either a settle after our overlay took keyboard focus (the event that dismisses the
    /// dropdown) or the outer budget, so a compositor that never gives us focus still
    /// captures. Clearing `menu_hold` does three things at once, on purpose: it runs the
    /// grab, it stops this poll, and it lets the overlay start painting again.
    pub(in crate::app) fn tick_menu_hold(&mut self) {
        let Some(hold) = self.menu_hold else {
            return;
        };
        let now = std::time::Instant::now();
        let since_focus = hold.focused.map(|f| now.duration_since(f).as_millis() as u64);
        let since_armed = now.duration_since(hold.armed).as_millis() as u64;
        if !crate::app::capture_flow::menu_hold_release(since_focus, since_armed) {
            return;
        }
        // Behaviour, not content: whether the causal signal arrived, and how long the whole
        // hold cost. That pair is what tells a real dismissal from a lucky wait.
        log::debug!(
            "menu hold: releasing the frozen-flats grab after {since_armed}ms ({})",
            match since_focus {
                Some(ms) => format!("overlay took keyboard focus {ms}ms ago"),
                None => "no overlay focus arrived, the outer budget fired".to_string(),
            }
        );
        self.menu_hold = None;
        crate::app::spawn_frozen_flats_grab(self.frozen_slot.clone(), hold.want_cursor);
    }

    /// DRAGON-460 step 2 of 2: the frame without marks is on screen, so take the shot.
    ///
    /// A LIVE region shot through `crate::screenshot::region` — the portable seam, with an
    /// identical signature on all three platforms (`platform/{linux/native,mac,windows}/
    /// screenshot.rs`), so the scanner reads current pixels everywhere with no `cfg` in this
    /// path at all.
    ///
    /// The cursor is deliberately NOT drawn in: it is not part of what the user is asking
    /// the scanner to read, and a pointer sitting over a word is exactly the kind of thing
    /// that makes OCR return nonsense.
    ///
    /// Deposits into `scan_shot_slot`; `MarksPoll` picks it up and runs the passes.
    fn run_scan_shot(&mut self) {
        if self.scan_shot != ScanShot::Clearing {
            return;
        }
        let Some((x, y, w, h)) = self.normalized_region() else {
            // Nothing selected — nothing to read. Drop straight back to idle rather than
            // sitting in a state the tick would keep firing on.
            self.scan_shot = ScanShot::Idle;
            return;
        };
        self.scan_shot = ScanShot::Shooting;
        let slot = self.scan_shot_slot.clone();
        if let Ok(mut g) = slot.lock() {
            *g = None;
        }
        // `lab/flatpak`: on the fallback overlay the live read below is a NATIVE grab this
        // session cannot make, so it would answer `None` and the scan would find nothing
        // with no way to say why. Read the seed-frozen frame instead. Shown and read stay
        // the same pixels for the same reason they do on the live path: the selector is
        // drawing that very frame. Cropping is a memcpy of the selection, so it runs
        // inline rather than on a thread.
        if self.scan_reads_frozen() {
            crate::util::timing_mark("scan shot: frozen region read (fallback overlay)");
            let shot = self.crop_frozen(x, y, w, h);
            if let Ok(mut g) = slot.lock() {
                *g = Some(shot);
            }
            return;
        }
        crate::util::timing_mark("scan shot: live region read (begin)");
        std::thread::spawn(move || {
            let shot = crate::screenshot::region(x, y, w, h, None);
            crate::util::timing_mark("scan shot: live region read (done)");
            if let Ok(mut g) = slot.lock() {
                *g = Some(shot);
            }
        });
    }

    // DRAGON-460 removed `finish_scan_shot`. It re-armed the scan when refreshed FLATS
    // landed; the scanner reads its own live region shot now, which `MarksPoll` picks up
    // directly, so there is no second landing path to re-arm from.

    fn kick_window_precapture(&mut self) {
        if self.window_precapture_started {
            return;
        }
        crate::util::timing_mark("kick_window_precapture: lazy window pre-capture (begin)");
        self.window_precapture_started = true;
        // QUIET, not spinner-up (DRAGON-645). The picker's loading state begins here, but
        // nothing is drawn until the pre-capture outlasts the reveal threshold, so switching
        // into window mode on a fast desktop shows the picker and never a spinner.
        self.picker_load = crate::app::overlay::PickerLoad::Quiet { ticks: 0 };
        spawn_window_precapture(
            self.precapture.clone(),
            self.freeze,
            wallpaper_path(),
            self.window_radius,
        );
    }
}
