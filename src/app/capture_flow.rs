use super::*;

/// DRAGON-228: whether the capture overlays are in the PICKING phase — the phase
/// where they should hold EXCLUSIVE keyboard so Escape (and the other overlay
/// shortcuts) work without a focusing click first. cosmic-comp only ever
/// auto-focuses Exclusive layer surfaces; under the historical OnDemand a
/// daemon-menu launch had NO keyboard until the user carefully clicked the
/// overlay (a picker's first click IS the capture). Exclusive is safe here: the
/// picking overlay is fullscreen modal UI, and the settings window never
/// coexists with it (`hide_overlays` destroys the overlays when settings opens).
/// It can never outlive picking (DRAGON-109): every commit destroys the surfaces
/// (`begin_capture` → `destroy_surfaces`, before the window flow's
/// focus-then-grab — DRAGON-194), and the countdown / recording phases re-mint
/// their own surfaces with their own interactivity.
// Only `overlay_pick_exclusive` (Linux) consumes it; the pure tests below keep it alive
// off Linux, so the bin build is dead there (Windows has no exclusive-grab step). DRAGON-229.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn picking_phase(countdown: bool, capture_live: bool, recording: bool) -> bool {
    !countdown && !capture_live && !recording
}

impl App {
    /// The keyboard interactivity to MINT a capture overlay with right now:
    /// Exclusive during picking (see [`picking_phase`]), OnDemand otherwise.
    #[cfg(target_os = "linux")]
    pub(super) fn overlay_pick_exclusive(&self) -> bool {
        picking_phase(self.countdown.is_some(), self.capture_live, self.recording.is_some())
    }

    /// The region selection normalized to (x, y, w, h), if non-empty.
    pub(super) fn normalized_region(&self) -> Option<(i32, i32, u32, u32)> {
        self.region.and_then(|gr| {
            let (l, t, r, b) = gr.to_tuple();
            let x = l.min(r);
            let y = t.min(b);
            let w = (l.max(r) - x) as u32;
            let h = (t.max(b) - y) as u32;
            (w >= 1 && h >= 1).then_some((x, y, w, h))
        })
    }

    pub(super) fn capture(&mut self, output: &str) -> Task<cosmic::Action<Msg>> {
        // Build the selection from the active mode.
        let sel = match self.mode {
            Mode::Region => match self.normalized_region() {
                Some((x, y, w, h)) => Selection {
                    x,
                    y,
                    width: w,
                    height: h,
                    output: None,
                    window_id: None,
                },
                // No region drawn yet: ignore the click so the user can drag.
                None => return Task::none(),
            },
            // Monitor (and the Window-mode toolbar button as a fallback): the
            // output whose toolbar was used.
            _ => match self.outputs.iter().find(|o| o.name == output) {
                Some(o) => Selection {
                    x: o.logical_pos.0,
                    y: o.logical_pos.1,
                    width: o.logical_size.0,
                    height: o.logical_size.1,
                    output: Some(o.name.clone()),
                    window_id: None,
                },
                None => return self.teardown(),
            },
        };

        self.run_capture(sel)
    }

    pub(super) fn run_capture(&mut self, sel: Selection) -> Task<cosmic::Action<Msg>> {
        // Delayed shots grab LIVE pixels (the delay exists to change the screen),
        // so a freeze snapshot is bypassed for them. Scanner captures never delay
        // (the chip is hidden in that kind).
        self.capture_live = self.kind != Kind::Scanner && self.configured_delay_secs() > 0;
        // Region video + PipeWire: request the portal (a monitor, to crop) NOW — at
        // commit, before any countdown. Monitor/window launch the portal at
        // mode-select instead, so they don't reach here on the pw path.
        // The xdg-portal ScreenCast path is Linux-only (DRAGON-94); macOS captures
        // via SCK, so `pipewire_available` is always false there and this is gated out.
        #[cfg(target_os = "linux")]
        if self.kind == Kind::Video
            && self.recording_uses_portal()
            && self.pipewire_available
            && self.mode == Mode::Region
            && self.pw_held.is_none()
            && let Some(rt) = self.region_clamped(&inset_region(sel.clone()))
        {
            // Show the clamped ORIGINAL box if we bounce back to draw (wrong monitor).
            if let Some(disp) = self.region_clamped(&sel) {
                let (x, y, w, h) = disp.rect;
                self.region = Some(GlobalRect::new(x, y, x + w as i32, y + h as i32));
            }
            return self.request_pipewire(
                ashpd::desktop::screencast::SourceType::Monitor,
                Some(rt),
                sel,
            );
        }
        // Region screenshot + PipeWire: same portal request; the single-frame grab
        // happens in `do_pixel_capture`. (Freeze is inert under PipeWire.)
        #[cfg(target_os = "linux")]
        if matches!(self.kind, Kind::Image | Kind::Scanner)
            && self.screenshot_uses_portal()
            && self.pipewire_available
            && self.mode == Mode::Region
            && self.pw_held.is_none()
            && let Some(rt) = self.region_clamped(&inset_region(sel.clone()))
        {
            if let Some(disp) = self.region_clamped(&sel) {
                let (x, y, w, h) = disp.rect;
                self.region = Some(GlobalRect::new(x, y, x + w as i32, y + h as i32));
            }
            return self.request_pipewire(
                ashpd::desktop::screencast::SourceType::Monitor,
                Some(rt),
                sel,
            );
        }
        self.proceed_capture(sel)
    }

    /// Run the countdown (if a delay is set) then begin — shared by every non-portal
    /// commit and by the portal path once a stream is granted.
    pub(super) fn proceed_capture(&mut self, sel: Selection) -> Task<cosmic::Action<Msg>> {
        let secs = self.configured_delay_secs();
        if self.kind != Kind::Scanner && secs > 0 {
            // The countdown ticker is a u8; clamp the arbitrary CLI value so a huge
            // `--countdown` can't wrap (255s is already far past any sane delay).
            self.enter_countdown(sel, secs.min(u8::MAX as u64) as u8)
        } else {
            self.begin(sel)
        }
    }

    /// The configured pre-capture delay in seconds: an exact `--countdown` CLI value
    /// when set (may not match any UI preset), otherwise the selected `delay_idx`
    /// preset. This is the setting, not the live countdown remaining.
    pub(super) fn configured_delay_secs(&self) -> u64 {
        self.countdown_override.unwrap_or(DELAYS[self.delay_idx].1)
    }

    /// Commit a capture: start a video recording when the kind is Video (region,
    /// window, or monitor — each records the selection's screen rectangle);
    /// otherwise take a screenshot.
    pub(super) fn begin(&mut self, sel: Selection) -> Task<cosmic::Action<Msg>> {
        let is_video = self.kind == Kind::Video;
        if is_video && !self.ffmpeg_available {
            // No encoder: surface the "install ffmpeg" notice rather than fail. Land on
            // the Capture Modes page's Screen Recordings tab (DRAGON-140). `open_settings`
            // resets the in-page tab to Scanner, so select Recordings AFTER it.
            log::warn!("recording requested but ffmpeg was not found on PATH");
            self.settings.nav.activate(self.settings.capture_modes);
            let task = self.open_settings();
            self.settings.set_capture_tab(super::settings::CaptureTab::Recordings);
            task
        } else if is_video {
            self.start_recording(sel)
        } else {
            self.begin_capture(sel)
        }
    }

    /// Enter the pre-capture countdown. The overlay is recreated click-through
    /// except for the toolbar's region (so the screen stays usable), and
    /// `view_window` switches to the countdown view. The timer subscription drives
    /// the tick down to the capture.
    pub(super) fn enter_countdown(&mut self, sel: Selection, secs: u8) -> Task<cosmic::Action<Msg>> {
        self.countdown = Some(secs);
        // DRAGON-278 follow-up (user spec a): for a WINDOW capture styled ACTIVE, focus the
        // target the MOMENT the countdown starts — so it is already active while the timer
        // runs and its title-bar / Mica activation has the whole countdown to settle (the
        // fire re-focuses in case focus was stolen mid-countdown). No-op for region/monitor
        // picks and for Inactive-styled window picks (the fire-time defocus handles those).
        self.focus_target_at_countdown_start(&sel);
        self.pending = Some(sel);
        self.recreate_active_overlays()
    }

    /// DRAGON-278 follow-up: at COUNTDOWN START, drive the picked window's REAL focus to
    /// ACTIVE so its native chrome (Mica / title bar / accent border) is already the active
    /// appearance while the timer counts down (user spec a). Only fires for a WINDOW capture
    /// whose "Window focus appearance" is ACTIVE — an Inactive-styled capture must NOT be
    /// focused here (the fire-time [`window_focus_grab`] Defocus arm drives that intent). The
    /// platform focus call is bounded but up to ~700ms, so it runs OFF the UI thread (the
    /// countdown overlay keeps ticking). Best-effort — a failure just means the fire-time
    /// re-focus does the work with a fresh (shorter) settle.
    ///
    /// Windows + macOS only: both put the countdown overlay in a click-through
    /// (`passthrough`) state where the user is expected to arrange the shot in other windows,
    /// so focusing the target fits that model. **Linux is deferred** (left byte-identical):
    /// its countdown overlay holds keyboard (Exclusive/OnDemand) for Escape, and activating
    /// a foreign toplevel there while the overlay is mapped fights cosmic-comp's own
    /// post-overlay focus restoration — the exact raciness the fire-time `activate_until`
    /// re-issue loop exists to tame POST-teardown. The Linux fire path (DRAGON-194) already
    /// focuses+grabs correctly; only the countdown-start pre-focus is skipped there.
    fn focus_target_at_countdown_start(&self, sel: &Selection) {
        let Some(id) = sel.window_id.as_deref() else {
            return;
        };
        if window_focus_intent(self.window_single_active) != WindowFocusIntent::Focus {
            return;
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            let id = id.to_string();
            std::thread::spawn(move || {
                #[cfg(windows)]
                let _ = crate::platform::windows::focus_window(&id);
                #[cfg(target_os = "macos")]
                let _ = crate::platform::mac::focus_window(&id);
            });
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            // Linux + other: deferred (see the doc comment) — no pre-focus.
            let _ = id;
        }
    }

    /// Tear down the overlay and arm the pixel-capture tick. Capturing while the
    /// overlay is still mapped would grab our own UI, so the actual grab happens
    /// one short subscription tick later in [`Self::do_pixel_capture`].
    pub(super) fn begin_capture(&mut self, sel: Selection) -> Task<cosmic::Action<Msg>> {
        // macOS (DRAGON-148 option C): the frozen flats are grabbed on a deferred
        // thread, so a commit could in principle race ahead of them (the user drew +
        // committed before the ~300ms grab landed). By commit the grab is almost
        // always already in `self.frozen` (drained by the poll while the overlay was
        // up); this only covers the rare race. Wait BRIEFLY (bounded) for the pending
        // grab so a freeze capture stays a freeze capture; if it can't land in time
        // we fall through to the existing LIVE grab (freezing() -> false on an empty
        // map). Only worth waiting when freeze is on for a still capture and the live
        // fallback isn't wanted anyway (delayed shots grab live).
        self.await_frozen_flats(&sel);
        // DRAGON-213: guarantee the launch-locked pointer is drained before an IMMEDIATE
        // shot stamps it. The dedicated launch-cursor grab fires at launch and is almost
        // always already drained by the poll while the overlay was up; this only covers a
        // commit that raced the poll. A delayed shot re-grabs the cursor at the capture
        // moment (`use_capture_moment_cursor`), so it never needs the launch lock here.
        self.await_cursor();
        // Overlay-independent captures (a toplevel grab, or a crop of the launch-time
        // freeze snapshot) don't need the overlay surfaces to clear off-screen first,
        // so grab immediately instead of waiting up to 200ms for the teardown tick.
        // The tick still runs as a harmless backstop — do_pixel_capture takes
        // `capturing`, so the second fire is a no-op. Live region/monitor grabs keep
        // waiting for the tick (the overlay must be gone before we read the screen).
        let immediate = sel.window_id.is_some() || (self.freezing() && !self.capture_live);
        // Remember the capture's monitor before the overlay (and `self.outputs`) is torn
        // down, so a post-capture preview can open a fullscreen overlay on that monitor.
        self.preview_output = self.output_for_selection(&sel);
        self.preview_output_scale = self.scale_for_selection(&sel);
        // Immediate captures (a window grab, or a freeze crop) don't read the live
        // composited screen, so we can show the preview overlay (a spinner) the instant
        // the capture is accepted — covering the grab + encode wait instead of flashing
        // a blank desktop. Live region/monitor grabs need a clean screen, so they open
        // the preview only after the grab, in `present_capture`.
        // Window picks defer the spinner to `do_pixel_capture`, AFTER the
        // focus-then-grab (DRAGON-194), on BOTH platforms: the spinner surface
        // contends for the very focus the grab depends on. On Linux it takes
        // keyboard focus on open (Exclusive overlay / focus-on-open window,
        // DRAGON-153), leaving the picked toplevel `Activated` compositor-side
        // while its headerbar renders UNFOCUSED (libcosmic follows keyboard
        // focus) — the fresh grab then races the repaint and captured an inactive
        // titlebar nondeterministically. On macOS a pre-opened spinner window
        // contends with the focus seam's `frontmostApplication == target`
        // verification poll the same way. The grab is synchronous at the top of
        // `do_pixel_capture`, so the spinner still covers compose/save/decode.
        //
        // ONE deliberate exception (Linux): Defocus intent with NO other toplevel
        // to hand focus to — the spinner's focus steal then IS the defocus
        // (`window_defocus_uses_spinner`), so it pre-opens on purpose.
        // DRAGON-216: a Linux OVERLAY window pick shows its spinner DURING the grab in a
        // focus-neutral form (promoted on `WindowGrabbed`) instead of a dead gap. The
        // flag both routes the surface open to `KeyboardInteractivity::None` and marks
        // the pending promotion; it's always false in windowed mode / on macOS (they
        // defer the whole open, below, unchanged).
        let neutral_spinner = self.window_pick_neutral_spinner(&sel, immediate);
        self.window_spinner_neutral = neutral_spinner;
        // Stale-session safety: a deferred windowed swap that never consumed (the
        // prior capture was cancelled between grab and save) must not fire on this
        // session's first ShotSaved.
        self.windowed_swap_pending = false;
        // DRAGON-216 (macOS windowed): a window pick pre-opens its preview WINDOW during the
        // grab too, but ORDER-FRONT ONLY (`visible:false` + `orderFront:`, no activate/makeKey)
        // so it never disturbs the picked window's focus; `WindowGrabbed` takes focus for real.
        // Always false off macOS (and for the overlay appearance, which keeps deferring).
        let mac_preopen = self.window_pick_preopens_window(&sel, immediate);
        #[cfg(target_os = "macos")]
        {
            self.mac_preview_preopen = mac_preopen;
        }
        let preview_pre_open =
            immediate && (sel.window_id.is_none() || self.window_defocus_uses_spinner(&sel));
        let preview_open = if neutral_spinner || preview_pre_open || mac_preopen {
            // The selection sizes the windowed preview at open, before the grab/decode
            // reports exact dims. The whole preview pipeline speaks PHYSICAL capture
            // pixels (the open-fit divides them back to logical by the source scale,
            // DRAGON-221), so scale the LOGICAL selection up by the output's buffer
            // scale — `1.0` on 1× displays, where this is byte-identical.
            let s = self.preview_output_scale;
            let dims = Some((
                (sel.width as f32 * s).round().max(1.0) as u32,
                (sel.height as f32 * s).round().max(1.0) as u32,
            ));
            self.open_preview_spinner(
                preview::PreviewKind::Image(preview::ImagePreview::loading()),
                dims,
            )
        } else {
            Task::none()
        };
        self.capturing = Some(sel);
        let mut cmds = self.destroy_surfaces();
        cmds.push(preview_open);
        if immediate {
            cmds.push(Task::done(cosmic::Action::App(Msg::Capture(CaptureMsg::DoPixelCapture))));
        }
        Task::batch(cmds)
    }

    /// The logical rect (x, y, w, h) of the output a window `sel` sits on, for the
    /// fullscreen check. Uses the FROZEN launch geometry first (it survives the
    /// overlay teardown that clears `self.outputs`, and `do_pixel_capture` runs
    /// post-teardown), then the in-app live output list, then a fresh live query of
    /// the platform's displays. The window's centre picks the output; `None` if no
    /// output contains it. DRAGON-186 Phase 3 / Phase 4.
    ///
    /// DRAGON-186 Phase 4 (bug 4 — a fullscreen mac window still got rounded corners):
    /// a WINDOW capture commits IMMEDIATELY (`begin_capture` dispatches `DoPixelCapture`
    /// without waiting) and `await_frozen_flats` deliberately skips window grabs, so on
    /// macOS the DEFERRED frozen-flats grab (DRAGON-148 option C) may not have landed —
    /// `self.frozen` is empty. `self.outputs` is ALSO empty by then (teardown cleared
    /// it). With both empty, `output_rect_for_window` returned `None`, so the fullscreen
    /// check never fired and the window kept its rounding/shadow/border. The final
    /// `crate::screenshot::output_descs()` fallback (a fresh live display query, portable
    /// and post-teardown-safe on both platforms) closes that gap; verified live that the
    /// display geometry it returns matches a fullscreen window's rect exactly, so
    /// `is_fullscreen` returns true.
    fn output_rect_for_window(&self, sel: &Selection) -> Option<(i32, i32, i32, i32)> {
        let cx = sel.x + sel.width as i32 / 2;
        let cy = sel.y + sel.height as i32 / 2;
        let contains = |px: i32, py: i32, pw: i32, ph: i32| {
            cx >= px && cx < px + pw && cy >= py && cy < py + ph
        };
        // Frozen launch geometry (post-teardown safe).
        if let Some((_, f)) = self.frozen.iter().find(|(_, f)| {
            let (px, py) = f.logical_pos;
            let (pw, ph) = f.logical_size;
            contains(px, py, pw, ph)
        }) {
            let (px, py) = f.logical_pos;
            let (pw, ph) = f.logical_size;
            return Some((px, py, pw, ph));
        }
        // In-app live output list (still mapped, or Linux where the grab is sync).
        if let Some(rect) = self
            .outputs
            .iter()
            .find(|o| {
                let (px, py) = o.logical_pos;
                let (pw, ph) = o.logical_size;
                contains(px, py, pw as i32, ph as i32)
            })
            .map(|o| {
                let (px, py) = o.logical_pos;
                let (pw, ph) = o.logical_size;
                (px, py, pw as i32, ph as i32)
            })
        {
            return Some(rect);
        }
        // Final fallback: a fresh live query of the platform's displays. Post-teardown
        // (both stores empty) this is the only geometry a fast window capture has —
        // without it, a fullscreen mac window is never detected (DRAGON-186 Phase 4).
        crate::screenshot::output_descs()
            .into_iter()
            .find(|o| {
                let (px, py) = o.logical_pos;
                let (pw, ph) = o.logical_size;
                contains(px, py, pw, ph)
            })
            .map(|o| {
                let (px, py) = o.logical_pos;
                let (pw, ph) = o.logical_size;
                (px, py, pw, ph)
            })
    }

    /// The output (and its logical size) the selection sits on — by name for a
    /// whole-monitor grab, else the output containing the selection's centre (falling
    /// back to any output).
    pub(super) fn output_for_selection(&self, sel: &Selection) -> Option<(OutputHandle, (u32, u32))> {
        self.output_for_selection_state(sel)
            .map(|o| (o.output.clone(), o.logical_size))
    }

    /// The backing scale (physical / logical) of the output a `sel` sits on — the
    /// value cached into `preview_output_scale` so the windowed preview opens logical-
    /// sized on scaled COSMIC displays (DRAGON-221). `1.0` on 1× outputs and on macOS
    /// (which reads the backing scale live from `NSScreen` instead).
    pub(super) fn scale_for_selection(&self, sel: &Selection) -> f32 {
        #[cfg(target_os = "linux")]
        {
            self.output_for_selection_state(sel).map(|o| o.scale).unwrap_or(1.0)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = sel;
            1.0
        }
    }

    /// The [`OutputState`] a `sel` sits on: its named output, else the output under the
    /// selection's centre, else the first — the shared lookup behind
    /// [`Self::output_for_selection`] and [`Self::scale_for_selection`].
    fn output_for_selection_state(&self, sel: &Selection) -> Option<&super::OutputState> {
        if let Some(name) = &sel.output
            && let Some(o) = self.outputs.iter().find(|o| &o.name == name)
        {
            return Some(o);
        }
        let cx = sel.x + sel.width as i32 / 2;
        let cy = sel.y + sel.height as i32 / 2;
        self.outputs
            .iter()
            .find(|o| {
                let (lx, ly) = o.logical_pos;
                let (lw, lh) = o.logical_size;
                cx >= lx && cx < lx + lw as i32 && cy >= ly && cy < ly + lh as i32
            })
            .or_else(|| self.outputs.first())
    }

    /// Finish a capture: it's already saved. Copy it to the clipboard when that's
    /// enabled and it's within the size limit, then post a notification — "Copied
    /// to clipboard" when we did, otherwise "Saved" with the location. Clicking the
    /// notification reveals the file. The clipboard + notifier run as detached
    /// helper processes, so we just exit.
    pub(super) fn finish_share(
        &mut self,
        path: &std::path::Path,
        size: u64,
        is_video: bool,
    ) -> Task<cosmic::Action<Msg>> {
        let within_limit = size <= self.clipboard_max_mb.value as u64 * 1_048_576;
        let copied = self.copy_to_clipboard && within_limit;
        if copied {
            crate::platform::services::copy_to_clipboard(path, is_video);
        }
        crate::platform::services::notify(path, copied);
        self.finish_session()
    }

    /// The region "Copy selection" quick-action's completion: like [`Self::finish_share`],
    /// but ALWAYS copies to the clipboard (the shortcut exists to copy, so it ignores the
    /// persisted `copy_to_clipboard` toggle) — still only within the clipboard size limit,
    /// past which copying would fail anyway. The capture is saved to disk exactly as a
    /// normal region shot (the save happens upstream in `do_pixel_capture`); this only
    /// changes the SHARE disposition to a forced copy, then notifies + finishes.
    pub(super) fn finish_share_forced_copy(
        &mut self,
        path: &std::path::Path,
        size: u64,
        is_video: bool,
    ) -> Task<cosmic::Action<Msg>> {
        let within_limit = size <= self.clipboard_max_mb.value as u64 * 1_048_576;
        if within_limit {
            crate::platform::services::copy_to_clipboard(path, is_video);
        } else {
            log::warn!(
                "copy-selection: capture ({size} bytes) exceeds the clipboard size limit; \
                 saved to {} but not copied",
                path.display()
            );
        }
        crate::platform::services::notify(path, within_limit);
        self.finish_session()
    }

    /// Freeze applies to the COSMIC image workflow in every selection mode (region / monitor /
    /// window): each reconstructs its capture from the launch freeze scene (the flat snapshot, or
    /// the frozen per-window pixels), honouring "Preserve wallpaper" at capture time. Dropped for
    /// video, the PipeWire source (a live portal frame can't be frozen), and delayed shots (which
    /// want the live post-delay screen).
    /// macOS (DRAGON-148 option C) commit-race guard: if the deferred frozen-flats
    /// grab hasn't landed yet but THIS capture would use it (freeze on, a still, not a
    /// delayed live shot), block BRIEFLY on the slot so freeze isn't silently downgraded
    /// to a live grab. Bounded to a short budget (nothing in the capture path waits
    /// unboundedly); on timeout we leave `frozen` empty and the existing live fallback
    /// runs. A no-op on Linux (flats grabbed synchronously; `frozen_pending` never set)
    /// and whenever the flats already landed or this capture wouldn't freeze.
    #[cfg(target_os = "macos")]
    fn await_frozen_flats(&mut self, sel: &Selection) {
        // Only wait when this capture would actually consume the flats: freeze on
        // (and supported by the active backend, DRAGON-186 Phase 2 — the
        // capability already folds in the portal condition, so no separate
        // `!screenshot_uses_portal()` term), Region mode (freeze is region-only,
        // DRAGON-194 follow-up), a still (Image/Scanner), and not a delayed live
        // shot.
        let wants_freeze = self.mode == Mode::Region
            && self.effective_capture_extras().freeze
            && !self.capture_live
            && matches!(self.kind, Kind::Image | Kind::Scanner);
        // A window grab reads per-window pixels / a live handle, not the flats, so it
        // never needs to wait on them.
        if !self.frozen_pending || !wants_freeze || sel.window_id.is_some() {
            return;
        }
        // Bounded poll of the slot (the deferred grab lands within a few hundred ms of
        // launch, and by commit it's almost always already drained). Cap the wait so a
        // stalled grab can never wedge the commit — fall through to the live grab.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(750);
        loop {
            if let Some(flats) = self.frozen_slot.lock().ok().and_then(|mut g| g.take()) {
                crate::util::timing_mark("await_frozen_flats: drained at commit (raced the grab)");
                self.frozen = flats;
                self.frozen_pending = false;
                return;
            }
            if std::time::Instant::now() >= deadline {
                crate::util::timing_mark("await_frozen_flats: timed out, falling back to live grab");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Linux: the flats are grabbed synchronously in `init`, so there's never a
    /// deferred grab to wait on — a no-op (keeps `begin_capture` platform-agnostic).
    #[cfg(not(target_os = "macos"))]
    #[allow(clippy::unused_self)]
    fn await_frozen_flats(&mut self, _sel: &Selection) {}

    /// DRAGON-213 (both platforms): make sure the dedicated launch-cursor grab has been
    /// drained into `frozen_cursor` before an immediate shot reads it. The grab fires at
    /// launch and is tiny, so by commit it is essentially always already drained by the
    /// poll — this only covers a commit that raced ahead of it. Bounded so a stalled grab
    /// can never wedge the commit (it falls through with no launch cursor, which is far
    /// better than blocking or stamping a stale one). A no-op once drained
    /// (`cursor_pending` false) or when the cursor extra is off.
    fn await_cursor(&mut self) {
        if !self.cursor_pending {
            return;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while !self.drain_cursor_slot() {
            if std::time::Instant::now() >= deadline {
                crate::util::timing_mark("await_cursor: timed out, no launch cursor stamped");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Linux (DRAGON-194): whether a WINDOW pick's spinner must PRE-open because it
    /// doubles as the DEFOCUS SINK — Inactive appearance wanted, and there is no
    /// other toplevel to hand focus to (the toplevel-management protocol has no
    /// deactivate request). The spinner stealing focus is then exactly the defocus
    /// the capture needs; every other window pick defers the spinner past the grab.
    /// macOS never needs this: its defocus activates the app itself, windows or not.
    #[cfg(target_os = "linux")]
    fn window_defocus_uses_spinner(&self, sel: &Selection) -> bool {
        sel.window_id.as_deref().is_some_and(|id| {
            window_focus_intent(self.window_single_active) == WindowFocusIntent::Defocus && {
                let candidates: Vec<String> =
                    self.frozen_toplevels.iter().map(|t| t.id.clone()).collect();
                defocus_activation_target(id, self.origin_window.as_deref(), &candidates)
                    .is_none()
            }
        })
    }

    /// Off Linux the spinner is never the defocus sink (macOS defocuses by
    /// activating the app itself); window picks always defer it past the grab.
    #[cfg(not(target_os = "linux"))]
    #[allow(clippy::unused_self)]
    fn window_defocus_uses_spinner(&self, _sel: &Selection) -> bool {
        false
    }

    /// DRAGON-216 (Linux overlay): whether a committed window pick should pre-open its
    /// preview spinner as a FOCUS-NEUTRAL overlay — visible DURING the off-thread
    /// focus-then-grab (`KeyboardInteractivity::None` takes no focus, so the picked
    /// window keeps the activation the grab depends on), promoted to `Exclusive` on
    /// `WindowGrabbed`. The layer surface's `None` interactivity is the ONLY primitive
    /// that maps a surface without stealing focus, so this is Linux-overlay-only:
    /// windowed mode and macOS open a real toplevel (activation-taking on cosmic-comp /
    /// breaks the mac frontmost-verify grab) and keep deferring the whole open past the
    /// grab, exactly as before.
    #[cfg(target_os = "linux")]
    fn window_pick_neutral_spinner(&self, sel: &Selection, immediate: bool) -> bool {
        window_pick_neutral_spinner_decision(
            immediate,
            sel.window_id.is_some(),
            self.preview_after_capture,
            self.window_defocus_uses_spinner(sel),
        )
    }

    /// Off Linux there is no focus-neutral surface primitive, so a window pick never
    /// pre-opens neutral — it defers the whole open past the grab (unchanged).
    #[cfg(not(target_os = "linux"))]
    #[allow(clippy::unused_self)]
    fn window_pick_neutral_spinner(&self, _sel: &Selection, _immediate: bool) -> bool {
        false
    }

    /// DRAGON-216 (macOS): whether a committed window pick should PRE-OPEN its preview
    /// surface during the focus-then-grab. macOS has no focus-neutral layer surface, but a
    /// real window/overlay can still be SHOWN without stealing focus: the WINDOWED preview
    /// opens `visible:false` and is ordered front with `orderFront:` (non-key), and the
    /// OVERLAY preview opens `visible:false` and is placed + ordered front by `place_overlay`
    /// WITHOUT `gain_focus` — so the picked window's frontmost-verify (DRAGON-194) is
    /// undisturbed in either case; `WindowGrabbed` then takes focus for real. Fires for BOTH
    /// appearances now (immediate window pick with the post-capture preview on).
    #[cfg(target_os = "macos")]
    fn window_pick_preopens_window(&self, sel: &Selection, immediate: bool) -> bool {
        window_pick_preopen_decision(immediate, sel.window_id.is_some(), self.preview_after_capture)
    }

    /// Off macOS the window pre-open is Linux's focus-neutral overlay (handled above) or
    /// nothing; this mac-only pre-open never fires.
    #[cfg(not(target_os = "macos"))]
    #[allow(clippy::unused_self)]
    fn window_pick_preopens_window(&self, _sel: &Selection, _immediate: bool) -> bool {
        false
    }

    /// DRAGON-216: resolve the pre-opened focus-neutral overlay spinner once the window
    /// grab has completed (Linux only). The user's preview appearance decides how: OVERLAY
    /// mode keeps the surface and only promotes its keyboard interactivity to `Exclusive`
    /// (no flicker); WINDOWED mode swaps it for the real preview window. No-op when no
    /// neutral overlay spinner is open (e.g. the defocus sink, which opened `Exclusive`).
    #[cfg(target_os = "linux")]
    pub(super) fn resolve_neutral_spinner(&mut self) -> Task<cosmic::Action<Msg>> {
        // Only a live OVERLAY-appearance spinner qualifies; a window surface means the
        // defocus-sink path already opened the real preview (never routed here).
        if self.preview.as_ref().is_none_or(|p| p.surface.is_window()) {
            return Task::none();
        }
        if self.preview_windowed {
            // DEFER the swap to `present_capture` (DRAGON-221 follow-up): the composed
            // image's dims aren't known until ShotSaved (padding/shadow/wallpaper
            // margins grow it past the selection), and a post-open `window::resize` is
            // not honored on COSMIC — so the cover stays up through compose/save and
            // the window opens ONCE at its correct size.
            self.windowed_swap_pending = true;
            Task::none()
        } else {
            super::shell::promote_preview_surface(self.preview.as_ref().map(|p| p.window).unwrap())
        }
    }

    /// Off Linux nothing pre-opens neutral, so there is nothing to resolve.
    #[cfg(not(target_os = "linux"))]
    #[allow(clippy::unused_self)]
    pub(super) fn resolve_neutral_spinner(&mut self) -> Task<cosmic::Action<Msg>> {
        Task::none()
    }

    pub(super) fn freezing(&self) -> bool {
        // The preference gated by the active backend's freeze capability
        // (DRAGON-186 Phase 2). `effective_capture_extras().freeze` already folds
        // in the active backend's freeze capability, which on Linux equals the
        // exact `!screenshot_uses_portal()` condition this used to also test (a
        // portal-active or protocol-less session reports `freeze: false`), so
        // dropping that redundant term is a provable no-op there. On macOS the
        // portal boolean is spuriously true (no Wayland screencopy), so keying on
        // the capability instead is what makes freeze work at all.
        //
        // REGION-only (DRAGON-194 follow-up, both platforms — this seam is
        // portable): freeze is a motion-stopping aid for drawing a region over a
        // busy screen. A WINDOW pick drives the target's REAL focus state and
        // re-grabs it live (frozen pixels would show a stale focus appearance —
        // exactly the bug this fixes), and a MONITOR pick needs no motion-stop
        // to aim. Mode switches mid-overlay recompute this live, so the frozen
        // backdrop appears/disappears with the Region mode selection.
        self.mode == Mode::Region
            && self.effective_capture_extras().freeze
            && matches!(self.kind, Kind::Image | Kind::Scanner)
            && !self.frozen.is_empty()
    }

    /// Whether the FROZEN backdrop is shown DURING SELECTION (the region/monitor
    /// selector's static launch-time background, and the reason the live-cursor
    /// indicator is suppressed). DRAGON-186 Phase 4 (spec §"Freeze pixels during
    /// selection"): "Swapping to recording (or turning on a countdown timer) should
    /// show live changes, but swapping back to picture mode should return to our
    /// frozen capture." Video mode already releases the backdrop because `freezing()`
    /// is false for `Kind::Video`; an ARMED countdown must release it identically —
    /// a delayed shot grabs the LIVE post-delay screen (`capture_live`), so showing a
    /// frozen backdrop while a countdown is armed would misrepresent what the capture
    /// will contain. This releases WHENEVER a countdown is configured
    /// (`configured_delay_secs() > 0`), whether or not it is currently ticking, so
    /// selection always previews live once a timer is set; clearing the delay back to
    /// "No delay" re-freezes. The actual CAPTURE-time freeze decision is unchanged
    /// (`freezing() && !capture_live` already skips the frozen reconstruction for a
    /// delayed shot); this only governs the during-selection preview.
    pub(super) fn freeze_backdrop_active(&self) -> bool {
        freeze_backdrop_active(self.freezing(), self.configured_delay_secs())
    }

    /// The frozen scene's windows that intersect the region (global logical coords), in z-order, as
    /// (pixels, rect, active) — the input to `region_windows_frozen`. Empty until the launch
    /// precapture posts its per-window pixels (callers fall back to the flat snapshot then).
    fn frozen_captured(
        &self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
    ) -> Vec<(image::RgbaImage, crate::platform::compositor::WinRect, bool)> {
        self.frozen_toplevels
            .iter()
            .filter_map(|t| {
                let (wx, wy, ww, wh) = t.rect;
                if wx + ww <= x || wx >= x + w as i32 || wy + wh <= y || wy >= y + h as i32 {
                    return None; // no overlap with the region
                }
                // Prefer the PRE-ACTIVATION pixels for the active window (DRAGON-186
                // Phase 5b): the launch precapture may have grabbed it AFTER our
                // overlay activated (gray inactive look), so a region/monitor freeze
                // that includes the active window uses its colored pre-activation
                // pixels when we have them. Non-active windows fall through to the
                // precapture pixels (their appearance is unchanged by activation).
                let img = self
                    .active_win_px
                    .get(&t.id)
                    .or_else(|| self.frozen_win_px.get(&t.id))?;
                Some((img.clone(), t.rect, t.active))
            })
            .collect()
    }

    /// Each frozen monitor's (logical_pos, logical_size) — for trimming a frozen composite.
    fn frozen_out_rects(&self) -> Vec<((i32, i32), (i32, i32))> {
        self.frozen.values().map(|f| (f.logical_pos, f.logical_size)).collect()
    }

    /// Pixel scale of the frozen monitor under (cx, cy) — the fallback when a frozen region has no
    /// windows, so an empty selection still yields a correctly-sized black rectangle.
    fn frozen_scale_at(&self, cx: i32, cy: i32) -> f32 {
        self.frozen
            .values()
            .find(|f| {
                let (ox, oy) = f.logical_pos;
                let (ow, oh) = f.logical_size;
                cx >= ox && cx < ox + ow && cy >= oy && cy < oy + oh
            })
            .map(|f| f.img.width() as f32 / f.logical_size.0.max(1) as f32)
            .unwrap_or(1.0)
    }

    /// Crop a region (global logical coords) out of the frozen snapshot of the
    /// output it sits on. Uses the snapshot's own stored geometry, so it still
    /// works after teardown clears the live output list. None if no snapshot.
    pub(super) fn crop_frozen(&self, x: i32, y: i32, w: u32, h: u32) -> Option<image::RgbaImage> {
        // Stitch the on-screen parts from every frozen output the selection
        // overlaps (so a region across two monitors composites both), then trim
        // the off-monitor remainder — same model as the live region capture.
        let refs: Vec<crate::screenshot::OutputFrameRef<'_>> = self
            .frozen
            .values()
            .map(|f| (&*f.img, f.logical_pos, f.logical_size))
            .collect();
        crate::screenshot::stitch_region(&refs, x, y, w, h)
    }

    /// Filename stem `<timestamp>[-<descriptor>]` for a capture/recording of `sel`:
    /// a window appends its slugified title (or the literal `window`), a monitor
    /// its slugified name (or `monitor`), a region nothing extra.
    pub(super) fn capture_stem(&self, sel: &Selection) -> String {
        let ts = capture_timestamp();
        let descriptor = if let Some(id) = &sel.window_id {
            let title = self
                .windows
                .values()
                .flatten()
                .find(|w| &w.id == id)
                .map(|w| slugify(&w.title))
                .unwrap_or_default();
            if title.is_empty() {
                "window".to_string()
            } else {
                title
            }
        } else if let Some(name) = &sel.output {
            let name = slugify(name);
            if name.is_empty() {
                "monitor".to_string()
            } else {
                name
            }
        } else {
            String::new()
        };
        if descriptor.is_empty() {
            ts
        } else {
            format!("{ts}-{descriptor}")
        }
    }

    /// One-line metadata for a screenshot, embedded as a PNG text chunk.
    pub(super) fn screenshot_metadata(&self) -> String {
        let cursor = if self.effective_capture_extras().cursor { "on" } else { "off" };
        format!(
            "Cosmic Capture Kit | type=photo | source={} | mode={} | cursor={}",
            self.source_label(),
            self.mode_label(),
            cursor,
        )
    }

    /// Grab pixels natively (cosmic screencopy), save the PNG to the screenshots
    /// folder, then share it (clipboard / notify / reveal) and exit.
    pub(super) fn do_pixel_capture(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(sel) = self.capturing.take() else {
            // A later repeat tick; the capture already ran.
            return Task::none();
        };
        // Capture only what's inside the visible selection line: a region is inset
        // by the line width; window/monitor grab the full target.
        let sel = inset_region(sel);

        // Committing a capture collapses a multi-instance session: tear down any
        // other overlays so only this shot proceeds.
        crate::instance::close_other_instances();

        // Destination path (shared by both the screencopy and PipeWire paths).
        let raw_dir = if self.screenshot_dir.trim().is_empty() {
            "~/Capture"
        } else {
            self.screenshot_dir.trim()
        };
        let dir = crate::util::expand_tilde(raw_dir);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.png", self.capture_stem(&sel)));

        // PipeWire screenshot: a portal stream was granted at commit. Grab a single
        // frame off the UI thread, save it, then share via `PipewireShotSaved`.
        // Linux-only (the portal/PipeWire path); on macOS `pw_held` is always None.
        #[cfg(target_os = "linux")]
        if let Some(held) = self.pw_held.take() {
            let HeldStream { fd, node_id, crop } = held;
            let fallback = path.clone();
            let meta = self.screenshot_metadata();
            // Grab + save on a dedicated OS thread (the PipeWire loop blocks); hand
            // the result back through a oneshot the Task awaits (executor-agnostic).
            let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                let ok = crate::platform::pipewire::grab_frame(fd, node_id, crop)
                    .map(|img| crate::media::png::save_png(&img, &path, &meta))
                    .unwrap_or(false);
                let _ = tx.send((path, ok));
            });
            return Task::perform(
                async move { rx.await.unwrap_or((fallback, false)) },
                |(path, ok)| cosmic::Action::App(Msg::Capture(CaptureMsg::ShotSaved(path, ok))),
            );
        }

        // The extras this capture actually applies: each persisted toggle gated by
        // the active backend's capability (DRAGON-186) — a stale persisted "on"
        // can't make a backend without the capability try to honor it.
        let extras = self.effective_capture_extras();

        // DRAGON-186 Phase 3 (spec §"Preserve mouse cursor"): for a DELAYED/countdown
        // capture the cursor must land where it was WHEN THE TIMER FIRED (this
        // instant, the real capture moment), not where it sat at launch. The frozen
        // scene's launch-locked `frozen_cursor` is right for a non-delayed shot (the
        // cursor is part of the frozen pixels), but a delayed shot grabs the live
        // post-delay screen — so re-grab the cursor NOW and overwrite the launch-locked
        // sprite that every overlay path (`cursor_overlay` below, the window job above)
        // reads. Gated on the cursor capability+pref (`extras.cursor`) and on this
        // being a live/delayed capture; the frozen (non-delayed) path is left
        // byte-identical (`capture_live` false → skipped). Portable: both platforms
        // expose `crate::screenshot::capture_cursor()`. A failed re-grab keeps the
        // launch-locked sprite (better than losing the cursor entirely).
        if use_capture_moment_cursor(extras.cursor, self.capture_live)
            && let Some(c) = crate::screenshot::capture_cursor()
        {
            self.frozen_cursor = Some(c);
        }

        // DRAGON-186 Phase 3: a truly-fullscreen captured window (e.g. a fullscreen
        // game) gets captured AS-IS — no window aesthetics. When the active backend
        // is `fullscreen_aware` and this is a window capture whose rect fills its
        // output, force the "Raw" look (no border / shadow / rounding / padding) and
        // suppress wallpaper-behind (a fullscreen window covers the whole output, so
        // there is no background to show). Region/monitor captures don't carry a
        // single owning window here, so this is a window-mode behavior; identical on
        // COSMIC and macOS (both feed `is_fullscreen` the same global-logical rects).
        // The fullscreen verdict has TWO inputs, ORed:
        //   1. The portable GEOMETRY gate — the window rect fills its output rect (within
        //      tol). Sufficient on COSMIC/Windows, where a fullscreen toplevel's rect equals
        //      its output rect.
        //   2. A per-platform OVERRIDE (`window_is_fullscreen`). On macOS the geometry gate
        //      MISSES native fullscreen: on a notched Mac a fullscreen window sits below the
        //      menu-bar safe area (measured live: origin=0,44 size=2048x1286 on a 2048x1330
        //      display), so its rect never fills the display bounds — AND a maximized/zoomed
        //      window is geometrically identical. The mac arm resolves it via the window's
        //      Space TYPE (a fullscreen window is on a dedicated fullscreen Space, a zoomed
        //      one on the normal desktop Space); Linux/Windows return `false` here, so their
        //      behavior is byte-identical to the geometry-only gate.
        let fullscreen_window = extras.fullscreen_aware
            && sel.window_id.is_some()
            && (self
                .output_rect_for_window(&sel)
                .is_some_and(|out| is_fullscreen((sel.x, sel.y, sel.width as i32, sel.height as i32), out, 2))
                || sel
                    .window_id
                    .as_deref()
                    .is_some_and(crate::screenshot::window_is_fullscreen));

        // Window borders: two explicit user-configured borders (DRAGON-191) drawn via
        // the portable alpha-dilation mechanism — an ACTIVE border for the focused /
        // single-window capture and an INACTIVE border for unfocused windows in a
        // region/monitor composite. No more per-desktop border-config reads
        // (JankyBorders' bordersrc / the COSMIC theme hint are gone). The Active colour
        // follows the system accent when the user hasn't pinned a custom one (resolved
        // in `WindowBorders::resolve`). A fullscreen window forces a RAW capture (no
        // border, no shadow, no rounding — exactly the compositor's output).
        let borders = crate::decoration::WindowBorders::resolve(
            self.active_border_color,
            self.active_border_width,
            self.inactive_border_color,
            self.inactive_border_width,
        );
        // A fullscreen window (e.g. a fullscreen game) captured AS-IS: strip both
        // borders and the shadow so nothing is drawn over its edge-to-edge pixels.
        let borders = if fullscreen_window {
            crate::decoration::WindowBorders {
                active: crate::decoration::BorderSpec { width: 0, ..borders.active },
                inactive: crate::decoration::BorderSpec { width: 0, ..borders.inactive },
            }
        } else {
            borders
        };
        let window_shadow = self.window_drop_shadow && !fullscreen_window;
        // The corner radius to hug for rounding/shadow: the COSMIC theme's window
        // radius on Linux (macOS derives the real radius from the captured alpha
        // corner downstream). A fullscreen window rounds to 0 (raw edge-to-edge).
        let deco_radius = self.window_radius;
        // Freeze active for this capture: reconstruct from the launch scene instead of the live
        // screen (any mode). Delayed shots (capture_live) keep the live post-delay screen.
        let frozen = self.freezing() && !self.capture_live;

        // Window capture (threaded so the picker overlay tears down and clears the screen
        // IMMEDIATELY rather than hanging for the whole job). Freeze feeds the launch-instant window
        // pixels into the SAME decorate/composite pipeline — transparency + wallpaper-behind honoured
        // exactly like live. With no frozen pixels (precapture not posted, or a window opened after
        // launch), `run()` grabs the toplevel live (fine mid-overlay — it's captured by handle).
        if let Some(id) = &sel.window_id {
            // Pixel source, in priority order:
            //   1. `active_win_px` — the target window's PRE-ACTIVATION pixels, grabbed
            //      synchronously before our overlay's `gain_focus` fired (DRAGON-186
            //      Phase 5b). Only the frontmost window is captured there, so this is
            //      populated ONLY when the user is capturing the window that was active
            //      at launch — and it carries the LIVE active appearance (colored traffic
            //      lights) instead of the gray inactive look a post-activation grab gets.
            //      Preferred REGARDLESS of the freeze setting (the fix is about WHEN we
            //      grabbed, not freeze).
            //   2. `frozen_win_px` — the launch precapture's per-window pixels, when
            //      freeze is on (motion-stopped scene reconstruction).
            //   3. None — `run()` grabs the toplevel live (fine mid-overlay; captured by
            //      handle). This is the post-activation path for a NON-active window,
            //      which macOS already renders inactive, so its appearance is unchanged.
            // DRAGON-189 (extended): re-focus-then-grab supersedes the daemon
            // pre-grab for a USER-PICKED window. The pre-activation cache
            // (`active_win_px`) only holds the window that was ALREADY frontmost at
            // capture initiation; a DIFFERENT picked window was never key, so its
            // cached/live pixels render GRAY. On macOS, for a committed window
            // selection, focus the exact window (AX raise + app activate), WAIT until
            // the OS confirms it frontmost (bounded), THEN grab a FRESH live capture
            // of it — its traffic lights are now ACTIVE (colored). This freshly-grabbed
            // active image WINS over the PreActivation/FrozenScene/Live precedence for
            // the committed window; the daemon pre-grab stays only as the fallback
            // below when the fresh active grab is unavailable (owner unresolved / grab
            // failed / freeze crop for a delayed shot). The overlay is already torn
            // down here (`begin_capture` dispatched `DoPixelCapture` post-teardown), so
            // yielding focus to the target is safe — the selection is fully committed.
            // DRAGON-194: the picked window's REAL focus state is driven to match the
            // chosen "Window focus appearance" (`window_single_active`) right before its
            // pixels are grabbed, so its native decorations agree with the border we draw:
            // Active -> focus it (colored mac traffic lights / active CSD titlebar), Inactive
            // -> defocus it (gray / dimmed). The freshly-grabbed pixels win over the
            // PreActivation/FrozenScene/Live precedence for the committed window; every OTHER
            // window stays frozen. (DRAGON-278 follow-up: a WINDOW pick drives focus even under
            // a countdown now — see below; only region/monitor delayed shots skip pre-focus,
            // and they take the region/monitor branch, not this one.)
            // DRAGON-215: the DRAGON-194 focus-then-grab is a seconds-long (Linux) /
            // ~700ms (macOS) / bounded-settle (Windows) wait; run it OFF the UI thread (in the
            // capture worker) so the iced loop keeps pumping and the just-torn-down overlay
            // actually presents instead of freezing full-screen. `None` only on an unsupported
            // platform (`window_focus_grab` returns None there).
            // DRAGON-278 follow-up (user spec b): a WINDOW capture ALWAYS drives + re-grabs the
            // target's focus at fire — EVEN under a countdown. Focus may have been stolen while
            // the timer ran, so the fire re-focuses (the countdown-start pre-focus above just
            // gave the Mica a head start to settle). This intentionally REPLACES the old
            // `!capture_live` skip for window mode: region/monitor delayed shots (which grab the
            // live post-delay screen, no pre-focus) go through the region/monitor branch below,
            // so their frozen-scene/live semantics are unchanged.
            let focus_grab = self.window_focus_grab(id, extras.transparency);
            // The FALLBACK pixel source, chosen + cloned on the UI thread (cheap map
            // lookups + a one-shot memcpy, not a focus-dependent wait): the pre-activation
            // active-window pixels, else the freeze scene's per-window pixels, else `None`
            // (the worker grabs the toplevel live). The off-thread `focus_grab` result,
            // when it lands, WINS over this (`active.or(fallback_px)` in the worker).
            let fallback_px = match window_pixel_source(
                self.active_win_px.contains_key(id),
                frozen && self.frozen_win_px.contains_key(id),
            ) {
                WindowPixelSource::PreActivation => self.active_win_px.get(id).cloned(),
                WindowPixelSource::FrozenScene => self.frozen_win_px.get(id).cloned(),
                WindowPixelSource::Live => None,
            };
            let job = crate::screenshot::WindowCaptureJob {
                id: id.clone(),
                // Don't PaintCursors onto the live grab: it stamps the cursor at its capture-instant
                // position (over the picker), not where it was on the window at launch. We overlay
                // the launch-locked cursor (below) at the correct window-relative spot instead.
                cursor: false,
                sel: sel.clone(),
                capture_transparency: extras.transparency,
                // A fullscreen window covers the whole output: no background shows, so
                // suppress wallpaper-behind regardless of the pref. DRAGON-186 Phase 3.
                capture_wallpaper: extras.wallpaper && !fullscreen_window,
                // A fullscreen window is captured raw (edge-to-edge, no rounding). Any
                // other window rounds to the theme radius (macOS derives its real radius
                // from the captured alpha corner downstream and overrides this).
                window_radius: if fullscreen_window { 0.0 } else { deco_radius },
                // A single-window capture's portrayal is the "Window focus appearance"
                // choice (Active by default): draw the Active or Inactive border. A
                // fullscreen window already had both widths zeroed above.
                border: borders.for_active(self.window_single_active),
                window_shadow,
                // TOTAL margin from the window edge; the active-hint border lives inside it,
                // so the wallpaper/shadow gap is padding - border. A fullscreen capture
                // gets no padding (raw edge-to-edge pixels). DRAGON-186 Phase 3.
                pad_logical: if self.window_padding && !fullscreen_window {
                    self.window_padding_px.value as f32
                } else {
                    0.0
                },
                dark: super::theme_is_dark(),
                // The overlay (and self.outputs) is torn down before this runs, so pass the
                // launch snapshot's per-output geometry for the composite.
                frozen_geom: self
                    .frozen
                    .iter()
                    .map(|(n, f)| (n.clone(), f.logical_pos, f.logical_size))
                    .collect(),
                // Assigned in the worker: the off-thread focus grab (if any) OR the
                // UI-chosen fallback. `None` here so the field exists; never the final value.
                frozen_px: None,
                // DRAGON-278 (Windows only): the off-thread ACTIVATED grab, assigned by the
                // worker below. It wins FIRST in `run()` without touching the frozen/live
                // precedence; the field exists only on the Windows job struct.
                #[cfg(windows)]
                active_grab: None,
                // A window capture stamps the cursor ONLY for a DELAYED shot (user rule,
                // DRAGON-214): with a countdown, `frozen_cursor` was swapped above for the
                // CAPTURE-MOMENT sprite and belongs in the picture (still containment-
                // clipped to the window by `cursor_over_window`). An immediate window pick
                // never includes the cursor — the launch-locked pointer is region/monitor
                // feedback, not part of a picked window. Same shared predicate as the
                // sprite swap, so the two can't drift.
                cursor_overlay: if use_capture_moment_cursor(extras.cursor, self.capture_live) {
                    self.frozen_cursor.clone()
                } else {
                    None
                },
            };
            let meta = self.screenshot_metadata();
            let fallback = path.clone();
            let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
            // Signals the moment the focus-then-grab COMPLETES, so the spinner (whose focus
            // steal is the DRAGON-194 root cause) opens ONLY AFTER the grab, never during it.
            let (grab_tx, grab_rx) = cosmic::iced::futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                // 1. Off-thread focus-then-grab (the former UI-thread freeze). Wins over the
                //    fallback when it lands.
                let active = focus_grab.and_then(|f| f());
                // 2. Grab done — release the spinner (its focus steal is now safe) while
                //    compose/save continue here behind it.
                let _ = grab_tx.send(());
                // 3. Decorate + composite + save (already off-thread pre-215).
                let mut job = job;
                // DRAGON-278: on Windows the activated grab rides its OWN field so it wins in
                // run() WITHOUT reordering the wgc→frozen→PrintWindow precedence; elsewhere it
                // takes over the frozen_px slot (mac/Linux — byte-identical to before).
                #[cfg(windows)]
                {
                    job.active_grab = active;
                    job.frozen_px = fallback_px;
                }
                #[cfg(not(windows))]
                {
                    job.frozen_px = active.or(fallback_px);
                }
                let ok = job
                    .run()
                    .map(|img| crate::media::png::save_png(&img, &path, &meta))
                    .unwrap_or(false);
                let _ = tx.send((path, ok));
            });
            // Open the spinner ON grab-completion — UNLESS begin_capture already pre-opened
            // it as the defocus focus-sink (`window_defocus_uses_spinner`; Linux single-
            // toplevel defocus), in which case `None` suppresses a second one. The grab now
            // runs behind the presented teardown, so the user sees the real desktop + focus
            // flick until the spinner raises, never a frozen overlay (DRAGON-215).
            let open_on_grab: Option<(u32, u32)> = if self.window_defocus_uses_spinner(&sel) {
                None
            } else {
                Some((sel.width, sel.height))
            };
            return Task::batch([
                Task::perform(async move { grab_rx.await.ok() }, move |_| {
                    cosmic::Action::App(Msg::Capture(CaptureMsg::WindowGrabbed(open_on_grab)))
                }),
                Task::perform(
                    async move { rx.await.unwrap_or((fallback, false)) },
                    |(path, ok)| cosmic::Action::App(Msg::Capture(CaptureMsg::ShotSaved(path, ok))),
                ),
            ]);
        }

        // Region / monitor (synchronous — fast). Freeze reconstructs from the scene; otherwise grab
        // live. Each honours "Preserve wallpaper": with it, the flat snapshot / full output; without,
        // windows composited over black.
        // The windows-over-black path (frozen OR live) carries no cursor of its own, so overlay the
        // LAUNCH-LOCKED cursor: captured once when the tool opened and reused for every mode and
        // selection. That way it lands where the pointer actually was (not wherever it drifted while
        // you drew the box, and not an unreliable post-teardown grab), and it's never lost switching
        // between the region/window/monitor selectors.
        let cursor_overlay = self.frozen_cursor.as_ref().filter(|_| extras.cursor);
        let frozen_source = frozen_region_source(extras.wallpaper);
        let img = if frozen && frozen_source == FrozenRegionSource::WindowsOnly {
            // Freeze + no wallpaper: recomposite the frozen windows over the correct
            // background (transparent if transparency-ON, else black) — same compositing
            // as the live path, from the launch instant. DRAGON-186 Phase 3: this branch
            // no longer gates on a NON-EMPTY frozen window set. With an empty set (nothing
            // captured intersected the selection, e.g. an empty desktop region),
            // `region_windows_frozen` builds a correctly-sized transparent/black canvas
            // via `fallback_scale` and composites nothing over it — honoring wallpaper-OFF.
            // The old `!self.frozen_win_px.is_empty()` guard sent that case to
            // `crop_frozen`, which returns the flat launch snapshot WITH the wallpaper
            // baked in — leaking the wallpaper despite wallpaper-OFF (audit gap 3).
            let out_rects = self.frozen_out_rects();
            let captured = self.frozen_captured(sel.x, sel.y, sel.width, sel.height);
            let cx = sel.x + sel.width as i32 / 2;
            let cy = sel.y + sel.height as i32 / 2;
            crate::screenshot::region_windows_frozen(
                crate::screenshot::FrozenWindows {
                    captured,
                    out_rects,
                    fallback_scale: self.frozen_scale_at(cx, cy),
                },
                &sel,
                deco_radius,
                extras.transparency,
                borders,
                cursor_overlay,
            )
        } else if frozen {
            // Freeze + wallpaper (or no-wallpaper before the scene's window pixels post): crop the
            // flat launch snapshot — a clean frozen image (region crop or whole monitor), never the
            // live overlay.
            self.crop_frozen(sel.x, sel.y, sel.width, sel.height)
        } else if !extras.wallpaper {
            // Live region/monitor, windows only (no wallpaper): composite the windows + the
            // launch-locked cursor.
            crate::screenshot::region_windows(
                &sel,
                deco_radius,
                extras.transparency,
                borders,
                cursor_overlay,
            )
        } else if let Some(name) = &sel.output {
            crate::screenshot::output(name, cursor_overlay)
        } else {
            crate::screenshot::region(sel.x, sel.y, sel.width, sel.height, cursor_overlay)
        };

        let Some(img) = img else {
            log::warn!("native pixel capture returned no image");
            return self.finish_session();
        };
        // Save the PNG straight to the screenshots folder (no external tool).
        if !crate::media::png::save_png(&img, &path, &self.screenshot_metadata()) {
            log::warn!("failed to write screenshot to {}", path.display());
            return self.finish_session();
        }
        // Restore focus to where we launched, then share (clipboard/explorer).
        if let Some(id) = &self.origin_window {
            crate::platform::compositor::activate(id);
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        // Pass the capture's pixel size so the windowed preview opens sized to the picture.
        let dims = Some((img.width(), img.height()));
        self.present_capture(path, size, false, dims)
    }

    /// Linux (DRAGON-194): drive the picked toplevel's REAL `activated` state to match the
    /// chosen appearance, THEN re-grab ONLY that window's pixels live (by handle,
    /// occlusion-proof), so its client-side decorations render focused/unfocused to agree
    /// with the border we draw. Every OTHER window stays frozen (only this one is re-grabbed).
    ///
    /// - `Focus`: activate the picked toplevel (cosmic toplevel-manager `activate`).
    /// - `Defocus`: activate a DIFFERENT toplevel so the pick drops its `activated` state
    ///   (the protocol has no deactivate request — [`defocus_activation_target`] chooses
    ///   the pre-launch focused window when possible). No candidate (the pick is the only
    ///   toplevel) -> leave focus as-is and grab best-effort.
    ///
    /// Returns the fresh grab to override the frozen/live precedence for this window, or
    /// `None` if the live grab failed (the caller falls back to its existing precedence).
    /// Verified live (`--test linux-focus-probe`): activating a toplevel flips its
    /// `activated` state and the re-grabbed pixels change ONLY in the titlebar region.
    #[cfg(target_os = "linux")]
    fn capture_window_with_focus(
        id: &str,
        intent: WindowFocusIntent,
        candidates: &[String],
        origin_window: Option<&str>,
    ) -> Option<image::RgbaImage> {
        // DRAGON-215: this runs OFF the UI thread (in the capture worker) — the
        // activate-confirm-settle-grab below is a seconds-long wait, and doing it on the
        // UI thread froze the just-torn-down overlay full-screen for its whole duration
        // (it blocked the iced loop that presents the teardown), which read as a system
        // hang. Its inputs are OWNED (`candidates` = frozen toplevel ids, `origin_window`)
        // so it needs no `&self`.
        // Grace after the STABLE confirmation (activate_until returns only once the state
        // has held ~400ms, during which the client has repainted) so the freshly committed
        // buffer is what the capture pipeline picks up. Shared cross-platform settle.
        const REDRAW_SETTLE: std::time::Duration = crate::platform::WINDOW_ACTIVATION_SETTLE;
        let (target, want_active) = match intent {
            WindowFocusIntent::Focus => (Some(id.to_string()), true),
            WindowFocusIntent::Defocus => {
                (defocus_activation_target(id, origin_window, candidates), false)
            }
        };
        if let Some(t) = target {
            // VERIFIED activation (DRAGON-194 follow-up): a fire-and-forget activate
            // races cosmic-comp's own post-overlay focus restoration (the overlay just
            // closed; the compositor returns focus to the pre-overlay toplevel on its
            // own schedule and can clobber ours mid-settle — the picked window then
            // captured unfocused). Poll the pick's `activated` state and re-issue
            // until it sticks, bounded, like the mac seam's frontmost confirmation.
            let confirmed = crate::platform::compositor::activate_until(&t, id, want_active);
            if !confirmed {
                log::warn!(
                    "DRAGON-194 capture_window_with_focus: {id} never reached \
                     activated={want_active} within the budget; grabbing best-effort"
                );
            }
            std::thread::sleep(REDRAW_SETTLE);
        } else {
            // Defocus with NO other toplevel: the pre-opened SPINNER is the focus
            // sink (`window_defocus_uses_spinner` made begin_capture raise it before
            // this grab — its focus steal is exactly the interference the Focus
            // intent defers it to avoid). The pick's headerbar follows keyboard
            // focus, which the spinner now holds; give the repaint one settle.
            log::debug!(
                "DRAGON-194 capture_window_with_focus: no defocus target for {id} \
                 (single toplevel); the pre-opened spinner is the focus sink"
            );
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        let img = crate::screenshot::window(id, false);
        log::debug!(
            "DRAGON-194 capture_window_with_focus: id={id} intent={intent:?} grabbed={}",
            img.is_some()
        );
        img
    }

    /// Build the OFF-THREAD window focus-then-grab for a committed window pick
    /// (DRAGON-215). The DRAGON-194 focus-then-grab (Linux `activate_until` + settle;
    /// macOS the bounded frontmost poll) used to run SYNCHRONOUSLY in `do_pixel_capture`,
    /// blocking the iced loop — which is also what presents the just-torn-down overlay —
    /// so the dead overlay lingered frozen full-screen for the whole (seconds- / ~700ms-
    /// long) wait, reading as a system hang. Returning it as a `Send` closure the capture
    /// worker runs keeps the loop pumping: the teardown presents and the user watches the
    /// real desktop + the focus flick while the grab happens behind the spinner. The fresh
    /// grab still WINS the frozen/live precedence for the picked window. `None` on a
    /// platform without the seam (the worker falls back to the frozen/live source).
    #[allow(clippy::type_complexity)]
    fn window_focus_grab(
        &self,
        id: &str,
        transparency: bool,
    ) -> Option<Box<dyn FnOnce() -> Option<image::RgbaImage> + Send>> {
        let intent = window_focus_intent(self.window_single_active);
        #[cfg(target_os = "linux")]
        {
            // Linux drives focus via the Wayland toplevel activate; transparency is chosen
            // downstream in the worker, not at the focus grab.
            let _ = transparency;
            let id = id.to_string();
            // Owned so the closure needs no `&self`: the frozen toplevel ids (the defocus
            // target search space) and the pre-launch focused window.
            let candidates: Vec<String> =
                self.frozen_toplevels.iter().map(|t| t.id.clone()).collect();
            let origin = self.origin_window.clone();
            Some(Box::new(move || {
                Self::capture_window_with_focus(&id, intent, &candidates, origin.as_deref())
            }))
        }
        #[cfg(target_os = "macos")]
        {
            // macOS grabs via SCK, which carries per-window alpha unconditionally; the
            // transparency choice is applied downstream, not at the focus grab.
            let _ = transparency;
            let id = id.to_string();
            Some(Box::new(move || match intent {
                WindowFocusIntent::Focus => crate::platform::mac::capture_window_active(&id),
                WindowFocusIntent::Defocus => crate::platform::mac::capture_window_inactive(&id),
            }))
        }
        #[cfg(windows)]
        {
            // DRAGON-278: Windows grabs the window ITSELF right after driving its focus, so
            // it needs the transparency choice at grab time (WGC vs PrintWindow) — threaded in
            // from the caller's already-computed extras, never re-derived in the closure.
            let id = id.to_string();
            Some(Box::new(move || match intent {
                WindowFocusIntent::Focus => {
                    crate::platform::windows::capture_window_active(&id, transparency)
                }
                WindowFocusIntent::Defocus => {
                    crate::platform::windows::capture_window_inactive(&id, transparency)
                }
            }))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = (id, intent, transparency);
            None
        }
    }
}

/// Whether a window rectangle covers its output rectangle within `tol` logical
/// pixels on every edge — i.e. the window is TRULY fullscreen (fills the whole
/// output, no visible decoration gap). Both rects are (x, y, w, h) in the same
/// global logical space. A small tolerance absorbs sub-pixel rounding and a
/// hairline the compositor may leave; anything larger (a maximized-but-decorated
/// window, a titlebar) is NOT fullscreen. DRAGON-186 Phase 3 — the pure,
/// platform-agnostic predicate (COSMIC + macOS feed it the same geometry).
pub(super) fn is_fullscreen(win: (i32, i32, i32, i32), out: (i32, i32, i32, i32), tol: i32) -> bool {
    let (wx, wy, ww, wh) = win;
    let (ox, oy, ow, oh) = out;
    // Reject degenerate rects — a zero-size output can't be "filled".
    if ow <= 0 || oh <= 0 || ww <= 0 || wh <= 0 {
        return false;
    }
    // The window must start at (or before, within tol) the output's top-left and
    // extend to (or past, within tol) its bottom-right — every edge flush.
    (wx - ox).abs() <= tol
        && (wy - oy).abs() <= tol
        && ((wx + ww) - (ox + ow)).abs() <= tol
        && ((wy + wh) - (oy + oh)).abs() <= tol
}

/// Whether the frozen backdrop is shown DURING SELECTION, factored pure so the
/// countdown-release rule is testable on any OS. `freezing` is the freeze gate
/// (`App::freezing`); `delay_secs` is the configured pre-capture delay
/// (`configured_delay_secs`). DRAGON-186 Phase 4: an armed countdown (`delay_secs >
/// 0`) releases the backdrop so selection previews the LIVE screen the delayed shot
/// will actually grab — mirroring how video mode releases it (there `freezing` is
/// already false). No delay re-freezes.
pub(super) fn freeze_backdrop_active(freezing: bool, delay_secs: u64) -> bool {
    freezing && delay_secs == 0
}

/// Whether to re-grab the cursor AT THE CAPTURE MOMENT (vs reuse the launch-locked
/// `frozen_cursor`). DRAGON-186 Phase 3 (spec §"Preserve mouse cursor"): a delayed/
/// countdown capture (`capture_live`) must place the cursor where it ended up when
/// the timer fired, so we re-grab live at the capture instant; a non-delayed shot
/// keeps the launch-locked cursor (it is part of the frozen scene pixels). Only
/// meaningful when the cursor extra is actually active (`cursor_active` folds the
/// capability AND the pref). Pure so the source selection is testable on any OS.
pub(super) fn use_capture_moment_cursor(cursor_active: bool, capture_live: bool) -> bool {
    cursor_active && capture_live
}

/// Whether the region/monitor selector should draw the LAUNCH-LOCKED cursor indicator
/// (DRAGON-214). The overlay preview and the stamped capture MUST agree, so this shares
/// the launch-vs-capture-moment decision with the capture path
/// ([`use_capture_moment_cursor`]): the indicator shows the launch-locked sprite EXACTLY
/// when an immediate (non-countdown) capture would stamp it. Per the behaviour spec:
///
/// - Region OR Monitor mode (what-you-see-is-what-you-get for both) — Window mode hides
///   it (a window pick is not a composed crop; the cursor is stamped only via the
///   containment rule at capture).
/// - "Preserve mouse cursor" (`cursor_active`) on — the effective extra, so a backend
///   that can't bake a cursor never previews one.
/// - No armed countdown — a countdown capture uses the CAPTURE-MOMENT pointer
///   (`use_capture_moment_cursor(cursor_active, true)`), so the launch-locked indicator
///   must hide (and reappear when the countdown is cleared).
/// - No freeze backdrop — the frozen still already bakes the pointer in, so drawing the
///   sprite over it would double it.
///
/// Pure so the visibility is unit-testable on any OS, holding on both platforms and with
/// freeze on or off.
pub(super) fn show_launch_cursor_indicator(
    mode: Mode,
    cursor_active: bool,
    freeze_backdrop: bool,
    countdown_armed: bool,
) -> bool {
    matches!(mode, Mode::Region | Mode::Monitor)
        && cursor_active
        && !freeze_backdrop
        && !use_capture_moment_cursor(cursor_active, countdown_armed)
}

/// Which reconstruction a frozen (freeze-mode) region/monitor capture uses — the
/// decision at the heart of DRAGON-186 Phase 3's audit-gap-3 fix, factored pure so
/// it can be tested on any OS. `wallpaper` OFF ALWAYS means the windows-only
/// composite (windows over transparent/black per the 3-way rule), INDEPENDENT of
/// whether any window was actually captured — an empty frozen set with wallpaper-OFF
/// must still composite over transparent/black (`region_windows_frozen` sizes an
/// empty canvas from `fallback_scale`), NEVER fall through to the flat launch
/// snapshot (`crop_frozen`), which carries the wallpaper baked in. Only wallpaper-ON
/// uses that flat snapshot (the wallpaper IS the desired background).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FrozenRegionSource {
    /// Composite the frozen windows over transparent/black (wallpaper OFF).
    WindowsOnly,
    /// Crop the flat launch snapshot, wallpaper included (wallpaper ON).
    FlatSnapshot,
}

/// The frozen region/monitor reconstruction source for a given "Preserve wallpaper"
/// state. See [`FrozenRegionSource`]. Deliberately does NOT take a "has any frozen
/// window" flag: the whole point of the Phase-3 fix is that emptiness must not
/// change the background choice.
pub(super) fn frozen_region_source(keep_wallpaper: bool) -> FrozenRegionSource {
    if keep_wallpaper {
        FrozenRegionSource::FlatSnapshot
    } else {
        FrozenRegionSource::WindowsOnly
    }
}

/// Which pixel source a WINDOW-mode capture decorates. DRAGON-186 Phase 5b: on
/// macOS, activating our accessory overlay (`gain_focus`) deactivates whatever app
/// was frontmost, re-rendering its window in the gray INACTIVE appearance. So the
/// commit prefers, in order: (1) the target window's PRE-ACTIVATION pixels
/// (`active_win_px`, grabbed synchronously before activation — carries the live
/// active appearance), then (2) the freeze scene's per-window pixels
/// (`frozen_win_px`, only when the capture is a freeze capture), then (3) a LIVE
/// grab in `WindowCaptureJob::run` (a non-active window, whose appearance activation
/// doesn't change). Factored pure so the priority is testable on any OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WindowPixelSource {
    /// The target window's pre-activation pixels (`active_win_px`).
    PreActivation,
    /// The freeze scene's per-window pixels (`frozen_win_px`).
    FrozenScene,
    /// No stored pixels: `run()` grabs the toplevel live.
    Live,
}

/// The window-mode pixel source given whether we HAVE pre-activation pixels for this
/// window and whether a freeze-scene grab of it is available (already ANDed with the
/// freeze gate by the caller). See [`WindowPixelSource`].
pub(super) fn window_pixel_source(
    have_pre_activation: bool,
    have_frozen_scene: bool,
) -> WindowPixelSource {
    if have_pre_activation {
        WindowPixelSource::PreActivation
    } else if have_frozen_scene {
        WindowPixelSource::FrozenScene
    } else {
        WindowPixelSource::Live
    }
}

/// What to do to the picked window's REAL focus state immediately before grabbing its
/// pixels (DRAGON-194), so its native window decorations agree with the "Window focus
/// appearance" the user chose. Derived purely from `window_single_active`; the old
/// `window_border_style` "Raw" (style 3) — a no-focus-touch mode — retired in DRAGON-191
/// (the appearance dropdown is now Active/Inactive only; "no border" is just width 0), so
/// there is no leave-untouched variant to produce here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WindowFocusIntent {
    /// Portray as Active: make the picked window frontmost/activated before the grab
    /// (colored mac traffic lights / focused CSD titlebar).
    Focus,
    /// Portray as Inactive: ensure the picked window is NOT frontmost/activated before
    /// the grab (gray mac traffic lights / dimmed CSD titlebar).
    Defocus,
}

/// DRAGON-216: the pure decision behind [`App::window_pick_neutral_spinner`]. A window
/// pick pre-opens its preview spinner as a FOCUS-NEUTRAL overlay (visible during the
/// focus-then-grab) when EVERY condition holds: the commit is immediate (a window grab,
/// not a delayed shot), it's actually a window pick, the preview is enabled, and it's NOT
/// the single-toplevel defocus sink (which deliberately pre-opens `Exclusive` to BE the
/// focus sink). It fires for BOTH preview appearances — the layer-shell neutral overlay is
/// the only focus-safe primitive, so WINDOWED mode uses it too and swaps it for the real
/// window at `WindowGrabbed` (the appearance decides the resolution, not the pre-open). Any
/// miss defers the whole open past the grab.
#[cfg(target_os = "linux")]
fn window_pick_neutral_spinner_decision(
    immediate: bool,
    is_window_pick: bool,
    preview_enabled: bool,
    is_defocus_sink: bool,
) -> bool {
    immediate && is_window_pick && preview_enabled && !is_defocus_sink
}

/// DRAGON-216: the pure decision behind [`App::window_pick_preopens_window`] (macOS). A
/// window pick pre-opens its preview surface to cover the off-thread focus-then-grab, for
/// BOTH appearances now: the WINDOWED preview opens order-front, and the fullscreen OVERLAY
/// preview is placed WITHOUT taking focus (`gain_focus` deferred to `WindowGrabbed`) — so
/// neither disturbs the picked window's key/frontmost state the grab depends on. Fires when
/// the commit is immediate, it's a window pick, and the preview is enabled. There is no
/// defocus-sink term — macOS has no single-toplevel spinner focus-sink. Portable pure logic
/// (tested on every platform); only the macOS caller consults it, so it is dead code
/// elsewhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn window_pick_preopen_decision(
    immediate: bool,
    is_window_pick: bool,
    preview_enabled: bool,
) -> bool {
    immediate && is_window_pick && preview_enabled
}

/// The [`WindowFocusIntent`] for a single-window capture given the persisted
/// "Window focus appearance" (`window_single_active`: true = Active, false = Inactive).
pub(super) fn window_focus_intent(single_active: bool) -> WindowFocusIntent {
    if single_active {
        WindowFocusIntent::Focus
    } else {
        WindowFocusIntent::Defocus
    }
}

/// Linux (DRAGON-194): which OTHER toplevel to activate so the picked window becomes
/// DEactivated (there is no deactivate request in the cosmic toplevel-manager protocol —
/// activating a different toplevel is the only way to drop a window's `activated` state).
/// Prefers the pre-launch focused window (`origin`) when it's a DIFFERENT, still-present
/// toplevel; otherwise any other candidate. `None` when the picked window is the only
/// toplevel (nothing to hand focus to — the caller grabs it best-effort as-is).
#[cfg(target_os = "linux")]
pub(super) fn defocus_activation_target(
    picked: &str,
    origin: Option<&str>,
    candidates: &[String],
) -> Option<String> {
    if let Some(o) = origin
        && o != picked
        && candidates.iter().any(|c| c == o)
    {
        return Some(o.to_string());
    }
    candidates.iter().find(|c| c.as_str() != picked).cloned()
}

#[cfg(test)]
mod window_focus_intent_tests {
    #[cfg(target_os = "linux")]
    use super::defocus_activation_target;
    use super::{window_focus_intent, WindowFocusIntent};

    // Active appearance -> focus the picked window before grabbing.
    #[test]
    fn active_maps_to_focus() {
        assert_eq!(window_focus_intent(true), WindowFocusIntent::Focus);
    }

    // Inactive appearance -> defocus the picked window before grabbing.
    #[test]
    fn inactive_maps_to_defocus() {
        assert_eq!(window_focus_intent(false), WindowFocusIntent::Defocus);
    }

    // Prefer the pre-launch focused window when it differs from the pick and is present.
    #[cfg(target_os = "linux")]
    #[test]
    fn defocus_prefers_origin_when_different_and_present() {
        let cands = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(
            defocus_activation_target("a", Some("b"), &cands),
            Some("b".to_string())
        );
    }

    // Origin == the pick: fall through to any OTHER candidate (never the pick itself).
    #[cfg(target_os = "linux")]
    #[test]
    fn defocus_skips_origin_equal_to_pick() {
        let cands = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            defocus_activation_target("a", Some("a"), &cands),
            Some("b".to_string())
        );
    }

    // Origin not among the known toplevels: ignore it, use any other candidate.
    #[cfg(target_os = "linux")]
    #[test]
    fn defocus_ignores_unknown_origin() {
        let cands = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            defocus_activation_target("a", Some("gone"), &cands),
            Some("b".to_string())
        );
    }

    // No origin hint: any other candidate.
    #[cfg(target_os = "linux")]
    #[test]
    fn defocus_without_origin_picks_other() {
        let cands = vec!["a".to_string(), "b".to_string()];
        assert_eq!(defocus_activation_target("a", None, &cands), Some("b".to_string()));
    }

    // The pick is the ONLY toplevel: nothing to hand focus to.
    #[cfg(target_os = "linux")]
    #[test]
    fn defocus_single_window_has_no_target() {
        let cands = vec!["a".to_string()];
        assert_eq!(defocus_activation_target("a", Some("a"), &cands), None);
        assert_eq!(defocus_activation_target("a", None, &cands), None);
    }
}

// DRAGON-216: the focus-neutral-spinner pre-open decision (Linux overlay only).
#[cfg(all(test, target_os = "linux"))]
mod neutral_spinner_tests {
    use super::window_pick_neutral_spinner_decision as decide;

    // The happy path: immediate window pick, preview on, not the defocus sink.
    #[test]
    fn window_pick_pre_opens_neutral() {
        assert!(decide(true, true, true, false));
    }

    // WINDOWED mode also pre-opens the neutral overlay now (DRAGON-216 follow-up): the
    // layer-shell None-interactivity surface is the only focus-safe primitive, so windowed
    // mode covers the grab with it too and swaps it for the real window on completion.
    // (The appearance is decided at `WindowGrabbed`, not here — the decision is mode-blind.)
    #[test]
    fn both_appearances_pre_open_neutral() {
        // Same inputs regardless of preview_windowed — the decision no longer takes it.
        assert!(decide(true, true, true, false));
    }

    // The single-toplevel defocus sink deliberately pre-opens Exclusive to BE the focus
    // sink, so it must NOT be routed through the neutral path.
    #[test]
    fn defocus_sink_is_not_neutral() {
        assert!(!decide(true, true, true, true));
    }

    // A delayed shot (not immediate) never pre-opens; nor does a non-window pick
    // (region/monitor), nor a run with the post-capture preview disabled.
    #[test]
    fn other_misses_defer() {
        assert!(!decide(false, true, true, false)); // delayed
        assert!(!decide(true, false, true, false)); // region/monitor
        assert!(!decide(true, true, false, false)); // preview off
    }
}

// DRAGON-216: the macOS windowed preview-window pre-open decision (portable pure logic).
#[cfg(test)]
mod mac_preopen_tests {
    use super::window_pick_preopen_decision as decide;

    // An immediate window pick with preview on pre-opens for BOTH appearances now
    // (DRAGON-216: the fullscreen overlay preview covers the grab too, not just windowed).
    #[test]
    fn window_pick_pre_opens_both_appearances() {
        assert!(decide(true, true, true));
    }

    // Delayed / non-window / preview-off all defer.
    #[test]
    fn other_misses_defer() {
        assert!(!decide(false, true, true)); // delayed
        assert!(!decide(true, false, true)); // region/monitor
        assert!(!decide(true, true, false)); // preview off
    }
}

#[cfg(test)]
mod cursor_source_tests {
    use super::use_capture_moment_cursor;

    // Countdown/delayed capture with the cursor extra on: re-grab at the capture
    // moment (cursor should be where the timer left it, not where it launched).
    #[test]
    fn countdown_regrabs_at_capture_moment() {
        assert!(use_capture_moment_cursor(true, true));
    }

    // No countdown (frozen scene): keep the launch-locked cursor — it is part of
    // the frozen pixels.
    #[test]
    fn no_countdown_uses_launch_locked_cursor() {
        assert!(!use_capture_moment_cursor(true, false));
    }

    // Cursor extra off: never re-grab (there is no cursor to place either way).
    #[test]
    fn cursor_off_never_regrabs() {
        assert!(!use_capture_moment_cursor(false, true));
        assert!(!use_capture_moment_cursor(false, false));
    }
}

#[cfg(test)]
mod launch_cursor_indicator_tests {
    use super::show_launch_cursor_indicator;
    use crate::app::Mode;

    // Region OR Monitor, cursor on, no freeze backdrop, no countdown: the launch-locked
    // indicator shows (what-you-see-is-what-you-get — both modes stamp the cursor).
    #[test]
    fn region_and_monitor_show_the_indicator() {
        assert!(show_launch_cursor_indicator(Mode::Region, true, false, false));
        assert!(show_launch_cursor_indicator(Mode::Monitor, true, false, false));
    }

    // Window mode never shows it (a window pick is not a composed crop; the cursor is
    // stamped only via the containment rule at capture).
    #[test]
    fn window_mode_hides_the_indicator() {
        assert!(!show_launch_cursor_indicator(Mode::Window, true, false, false));
    }

    // An armed countdown hides it: the delayed capture uses the CAPTURE-MOMENT pointer,
    // so previewing the launch lock would mislead. Clearing the countdown restores it.
    #[test]
    fn armed_countdown_hides_then_restores() {
        assert!(!show_launch_cursor_indicator(Mode::Region, true, false, true));
        assert!(!show_launch_cursor_indicator(Mode::Monitor, true, false, true));
        assert!(show_launch_cursor_indicator(Mode::Region, true, false, false));
    }

    // The frozen backdrop already bakes the pointer into the still; drawing the sprite
    // over it would double it.
    #[test]
    fn freeze_backdrop_hides_the_indicator() {
        assert!(!show_launch_cursor_indicator(Mode::Region, true, true, false));
    }

    // Cursor extra off: nothing to preview in any mode.
    #[test]
    fn cursor_off_hides_the_indicator() {
        assert!(!show_launch_cursor_indicator(Mode::Region, false, false, false));
        assert!(!show_launch_cursor_indicator(Mode::Monitor, false, false, false));
    }
}

#[cfg(test)]
mod freeze_backdrop_tests {
    use super::freeze_backdrop_active;

    // No delay + freeze on: the frozen backdrop is shown (picture mode, no countdown).
    #[test]
    fn freeze_no_delay_shows_backdrop() {
        assert!(freeze_backdrop_active(true, 0));
    }

    // Freeze on but a countdown armed: release the backdrop so selection previews the
    // LIVE screen the delayed shot will grab (the DRAGON-186 Phase 4 fix). Any nonzero
    // delay releases it.
    #[test]
    fn freeze_with_armed_countdown_releases_backdrop() {
        assert!(!freeze_backdrop_active(true, 3));
        assert!(!freeze_backdrop_active(true, 5));
        assert!(!freeze_backdrop_active(true, 10));
        assert!(!freeze_backdrop_active(true, 1));
    }

    // Freeze off (e.g. video mode, or freeze disabled): never a backdrop regardless of
    // the delay — matches how video mode already releases it.
    #[test]
    fn freeze_off_never_shows_backdrop() {
        assert!(!freeze_backdrop_active(false, 0));
        assert!(!freeze_backdrop_active(false, 5));
    }
}

#[cfg(test)]
mod frozen_source_tests {
    use super::{frozen_region_source, FrozenRegionSource};

    // Wallpaper OFF always composites windows-only, so wallpaper can never leak —
    // and this holds regardless of how many windows were frozen (there is no
    // window-count input by design).
    #[test]
    fn wallpaper_off_is_windows_only() {
        assert_eq!(frozen_region_source(false), FrozenRegionSource::WindowsOnly);
    }

    // Wallpaper ON uses the flat snapshot (the wallpaper is the wanted background).
    #[test]
    fn wallpaper_on_is_flat_snapshot() {
        assert_eq!(frozen_region_source(true), FrozenRegionSource::FlatSnapshot);
    }
}

#[cfg(test)]
mod window_pixel_source_tests {
    use super::{window_pixel_source, WindowPixelSource};

    // DRAGON-186 Phase 5b: pre-activation pixels always win — they carry the active
    // (colored) appearance grabbed before our overlay deactivated the target app.
    // They beat a freeze-scene grab (which may be post-activation / gray).
    #[test]
    fn pre_activation_wins_over_frozen() {
        assert_eq!(window_pixel_source(true, true), WindowPixelSource::PreActivation);
    }

    // Pre-activation pixels win over a live grab too (the whole point of the fix).
    #[test]
    fn pre_activation_wins_over_live() {
        assert_eq!(window_pixel_source(true, false), WindowPixelSource::PreActivation);
    }

    // No pre-activation pixels (a NON-active window, or the grab failed) but a freeze
    // scene grab is available: use the freeze scene (motion-stopped reconstruction).
    #[test]
    fn frozen_scene_when_no_pre_activation() {
        assert_eq!(window_pixel_source(false, true), WindowPixelSource::FrozenScene);
    }

    // Nothing stored: fall through to a LIVE grab in `WindowCaptureJob::run`. This is
    // the path for a non-active window with freeze off — its appearance is unchanged
    // by activation, so a post-activation live grab is correct.
    #[test]
    fn live_when_nothing_stored() {
        assert_eq!(window_pixel_source(false, false), WindowPixelSource::Live);
    }
}

#[cfg(test)]
mod fullscreen_tests {
    use super::is_fullscreen;

    // A 1080p window exactly filling a 1080p output at the origin is fullscreen.
    #[test]
    fn exact_fill_is_fullscreen() {
        assert!(is_fullscreen((0, 0, 1920, 1080), (0, 0, 1920, 1080), 2));
    }

    // The same on a second monitor at an offset (global coords).
    #[test]
    fn offset_output_exact_fill_is_fullscreen() {
        assert!(is_fullscreen((1920, 0, 2560, 1440), (1920, 0, 2560, 1440), 2));
    }

    // Sub-pixel / hairline slop within tolerance still counts.
    #[test]
    fn within_tolerance_is_fullscreen() {
        assert!(is_fullscreen((1, 0, 1919, 1080), (0, 0, 1920, 1080), 2));
        assert!(is_fullscreen((0, 0, 1921, 1081), (0, 0, 1920, 1080), 2));
    }

    // A maximized-but-decorated window (a titlebar's worth short) is NOT fullscreen.
    #[test]
    fn maximized_with_titlebar_is_not_fullscreen() {
        assert!(!is_fullscreen((0, 32, 1920, 1048), (0, 0, 1920, 1080), 2));
    }

    // A small floating window is not fullscreen.
    #[test]
    fn floating_window_is_not_fullscreen() {
        assert!(!is_fullscreen((100, 100, 800, 600), (0, 0, 1920, 1080), 2));
    }

    // A window offset onto the wrong output (does not reach this output's edges)
    // is not fullscreen against it.
    #[test]
    fn window_not_reaching_edges_is_not_fullscreen() {
        assert!(!is_fullscreen((0, 0, 1920, 1080), (0, 0, 2560, 1440), 2));
    }

    // Degenerate output/window rects can't be fullscreen.
    #[test]
    fn degenerate_rects_are_not_fullscreen() {
        assert!(!is_fullscreen((0, 0, 1920, 1080), (0, 0, 0, 0), 2));
        assert!(!is_fullscreen((0, 0, 0, 0), (0, 0, 1920, 1080), 2));
    }

    // DRAGON-186 Phase 4: the EXACT values logged live on this Mac for a real
    // fullscreen TextEdit window on a secondary display (both rects come from the SAME
    // global-logical top-left space — `list_windows`'s `SCWindow.frame` and
    // `output_descs`'s `CGDisplayBounds`), proving the predicate itself was always
    // correct. Bug 4 was that `output_rect_for_window` returned `None` post-teardown
    // (no geometry to compare against), NOT a coordinate-space mismatch — the live
    // `output_descs()` fallback now supplies exactly this output rect.
    #[test]
    fn real_mac_fullscreen_window_is_detected() {
        // 'Untitled' TextEdit fullscreen on Display-2.
        let win = (-2521, -1492, 1920, 1080);
        let out = (-2521, -1492, 1920, 1080);
        assert!(is_fullscreen(win, out, 2));
    }

    // The same machine's NON-fullscreen editor windows (logged live) must NOT trip it.
    #[test]
    fn real_mac_floating_windows_are_not_fullscreen() {
        // Zen Browser / terminal on Display-3 (pos -601,-1800 size 3200x1800).
        let out = (-601, -1800, 3200, 1800);
        assert!(!is_fullscreen((1009, -1750, 1570, 1729), out, 2));
        assert!(!is_fullscreen((-581, -1750, 1570, 1729), out, 2));
    }

    // DRAGON-186 follow-up — the NOTCHED built-in display case the geometry gate CANNOT
    // catch. Measured live on a 2048x1330 notch Mac: a native-fullscreen TextEdit reported
    // origin=0,44 size=2048x1286 (the fullscreen window sits BELOW the menu-bar safe area,
    // so it never fills the whole display bounds). Unlike the DRAGON-186 external-display
    // case above (where win == out exactly), here the geometry gate returns FALSE — which
    // is exactly why the mac path needs the Space-TYPE override (`window_is_fullscreen`).
    #[test]
    fn notched_mac_fullscreen_window_misses_the_geometry_gate() {
        let win = (0, 44, 2048, 1286);
        let out = (0, 0, 2048, 1330);
        // 44px inset on the top edge, 44px short on height — far beyond tol=2.
        assert!(!is_fullscreen(win, out, 2));
    }

    // ...and a MAXIMIZED (zoomed) window on the same notch Mac is GEOMETRICALLY IDENTICAL to
    // the fullscreen one (measured live: both origin=0,44 size=2048x1286), so no tolerance
    // bump could separate them without also catching a zoomed window. The Space TYPE is the
    // only discriminator — hence the override rather than a mac-specific tolerance.
    #[test]
    fn notched_mac_zoomed_window_is_geometrically_identical_to_fullscreen() {
        let out = (0, 0, 2048, 1330);
        let zoomed = (0, 44, 2048, 1286);
        let fullscreen = (0, 44, 2048, 1286);
        assert_eq!(zoomed, fullscreen);
        // Both fail the geometry gate the same way; only the Space-type override tells them
        // apart (proven live via `spaces::fullscreen_space_ids`).
        assert!(!is_fullscreen(zoomed, out, 2));
        assert!(!is_fullscreen(fullscreen, out, 2));
    }
}

#[cfg(test)]
mod picker_keyboard_tests {
    use super::picking_phase;

    #[test]
    fn only_the_idle_picking_phase_takes_exclusive() {
        // Idle picking (any mode) -> Exclusive: Escape must work sans click.
        assert!(picking_phase(false, false, false));
        // Any countdown / live-capture / recording phase forbids it (DRAGON-109).
        assert!(!picking_phase(true, false, false));
        assert!(!picking_phase(false, true, false));
        assert!(!picking_phase(false, false, true));
    }
}
