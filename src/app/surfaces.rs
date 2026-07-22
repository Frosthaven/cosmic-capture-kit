use super::*;

impl App {
    /// Destroy every overlay surface (the dummy bottom surface stays alive as the
    /// event-loop anchor so the post-teardown capture tick still fires).
    pub(super) fn destroy_surfaces(&mut self) -> Vec<Task<cosmic::Action<Msg>>> {
        // Release the capture single-instance lock the moment the overlays are
        // PERMANENTLY gone (DRAGON-255+: capture committed -> preview, recording
        // stopped -> preview, or teardown -> exit). That lock exists ONLY to stop a
        // second trigger from opening a DUPLICATE capture overlay while one is live;
        // once no overlay is up, this process must not keep blocking new captures.
        // Without this, a lingering PREVIEW editor (the default post-capture path) held
        // the lock for the whole process life, so the next daemon "Region" / global
        // hotkey found the lock held and silently no-op'd — the "nothing happens" bug —
        // unless "allow multiple instances" was on. `open_settings` already releases it
        // the same way when a capture becomes a settings window; this extends the SAME
        // rule to the preview phase, so an open preview OR settings window never blocks
        // a fresh capture regardless of the "allow multiple instances" setting, on every
        // platform. Idempotent + a no-op when the lock was never taken (allow-multiple
        // on); on the exit paths the process dies right after (the mutex/flock
        // auto-releases anyway), so this only CHANGES behavior for the keep-running
        // preview transition — exactly the intended fix.
        crate::instance::release_capture_lock();
        // macOS: the AeroSpace pause exists ONLY to protect the capture overlays, so
        // resume the tiling WM the moment they're permanently gone (capture committed
        // -> preview, recording stopped, teardown) instead of at process exit — the
        // preview/settings windows are ordinary windows the WM should manage as
        // usual. Every permanent overlay-close routes through here (toolbar rebuilds
        // and portal re-seeds use their own close paths and keep the pause). The
        // finish_session/exit resumes stay as idempotent backstops (the PAUSED flag
        // clears on the first resume, so later calls no-op).
        #[cfg(target_os = "macos")]
        {
            crate::platform::mac::window::resume_tiling_wm();
            self.aerospace_guard = None;
        }
        // Windows (DRAGON-281): the countdown/recording overlays may have been made
        // click-through (`recreate_active_overlays` set `passthrough_active`); this is a
        // PERMANENT overlay close (capture committed → preview, recording stopped → preview,
        // teardown), so the overlays are gone for good and there is nothing left to hover-
        // poll. Clear the flag here so `sub_passthrough`'s 60ms tick stops instead of running
        // forever under the (heavy) preview — the same class of stray-tick-under-preview
        // pegging `sub_meter_tick` guards against (DRAGON-247). The normal restore-to-region
        // path (`restore_interactive_overlays`) already clears it; this covers the commit/
        // teardown paths that never route through there. Windows-gated so mac stays
        // byte-identical (its passthrough poll idles harmlessly on the empty output list).
        #[cfg(windows)]
        {
            self.passthrough_active = false;
            self.passthrough_solid = None;
        }
        let cmds: Vec<_> = self
            .outputs
            .iter()
            .map(|o| super::shell::close_surface(o.id))
            .collect();
        self.outputs.clear();
        cmds
    }

    /// The one-shot session is finished — capture shared, preview closed, or an
    /// unrecoverable error — so end the process. This is THE lifecycle seam for the
    /// one-shot app model: every "we're done" path routes through here.
    ///
    /// macOS residency now lives in a SEPARATE menu-bar daemon (`crate::daemon`,
    /// DRAGON-130) that spawns each capture as a one-shot child, so a capture process
    /// ALWAYS exits at finish — even with `resident` on in config — exactly like
    /// Linux. (Before, this branched to an in-app idle; that idle was retired when the
    /// daemon took over.) The macOS pre-exit teardown (resume the tiling WM + release
    /// the AeroSpace babysitter) still runs so the WM is restored before we go.
    pub(super) fn finish_session(&mut self) -> Task<cosmic::Action<Msg>> {
        // DRAGON-174: the per-session status icon lives for the WHOLE session and exits
        // with it — tear it down here (removes an own NSStatusItem / ksni item; reverts a
        // resident relay to idle). Explicit even though the process exits right after, so
        // an own item never lingers a frame and the resident sees the clean disconnect.
        self.drop_session_icon();
        #[cfg(target_os = "macos")]
        {
            crate::platform::mac::window::resume_tiling_wm();
            // Release the AeroSpace death-pipe babysitter (DRAGON-130): dropping the
            // guard closes the child's stdin → it fires `aerospace enable on` on EOF —
            // a harmless double of the resume above (idempotent by design).
            self.aerospace_guard = None;
        }
        // Windows (DRAGON-246): GUARANTEE the process actually dies. `cosmic::iced::exit()`
        // only queues an `Action::Exit` the winit runtime must still process to unwind the
        // event loop; if a settings child's window was destroyed out-of-band (see
        // `sub_settings_liveness`) or the runtime otherwise fails to tear down, the process
        // would linger as a ZOMBIE holding the settings named mutex + pid file — and then
        // every later daemon "Settings" click finds the lock held and self-exits, so nothing
        // appears. A short-delay hard-exit thread is the backstop: on the normal path
        // `iced::exit()` unwinds and `main` returns (terminating this detached thread
        // mid-sleep) WELL before the grace elapses, so `process::exit` never runs and there
        // is no added latency; only a wedged runtime reaches it, converting a forever-zombie
        // into a clean ~1.5s exit. Every `finish_session` caller has already completed its
        // file/clipboard/notify work (finalize/bake results are awaited before this seam), so
        // the delayed exit can cut nothing off. Linux/macOS never arm this — their
        // `iced::exit()` reliably terminates — keeping their behavior byte-identical.
        #[cfg(windows)]
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            log::warn!(
                "DRAGON-246: iced::exit() did not terminate the process within the grace \
                 window — forcing process exit (the settings mutex/pid would otherwise leak)"
            );
            std::process::exit(0);
        });
        cosmic::iced::exit()
    }

    pub(super) fn teardown(&mut self) -> Task<cosmic::Action<Msg>> {
        let mut cmds = self.destroy_surfaces();
        cmds.push(self.finish_session());
        Task::batch(cmds)
    }

    /// An EXPLICIT quit (the toolbar ✕ / `WindowChromeMsg::Quit`): tear the overlays
    /// down and exit. Identical to `teardown()` — `finish_session` already resumes the
    /// tiling WM and releases the AeroSpace babysitter on macOS before exiting, so no
    /// separate quit path is needed now that residency lives in the daemon.
    pub(super) fn quit_now(&mut self) -> Task<cosmic::Action<Msg>> {
        self.teardown()
    }

    /// Build a per-output `Layer::Overlay` surface. `input_zone`: None = full
    /// input, Some(rects) = only those rects are interactive (rest click-through).
    /// `keyboard` enables ON-DEMAND keyboard input (focus on click); `false` = none.
    #[cfg(target_os = "linux")]
    pub(super) fn overlay_surface(
        &self,
        output: OutputHandle,
        id: window::Id,
        input_zone: Option<Vec<cosmic::iced::Rectangle>>,
        keyboard: bool,
    ) -> Task<cosmic::Action<Msg>> {
        super::shell::overlay_surface(output, id, input_zone, keyboard)
    }

    // macOS/Windows: there is no per-output `overlay_surface` method — the capture
    // overlays are minted directly through `shell::overlay_window` (which MINTS the
    // window id, unlike the layer-shell path that takes a pre-chosen one) by
    // `seed_outputs_mac` below, and countdown/recording keep the windows in place
    // (the recreate/restore methods are no-ops off Wayland).

    /// macOS/Windows: seed `self.outputs` + mint one capture overlay window per
    /// display, once at startup. This replaces the Wayland `OutputEvent` → `on_output`
    /// path (there is no output-event subscription off Wayland). Mirrors `on_output`'s
    /// guards: `--settings` gets no overlays, and `--preview <file>` opens the file's
    /// preview on the active output instead of any capture overlay.
    #[cfg(not(target_os = "linux"))]
    // The final overlay-seeding branch is a cfg'd macOS `{ return … }` block followed by
    // a cfg'd-out `not(macos)` tail; dropping `return` there is a compile error, so allow
    // the lint on macOS only (Linux compiles the tail branch and never sees it).
    #[cfg_attr(target_os = "macos", allow(clippy::needless_return))]
    pub(super) fn seed_outputs_mac(&mut self) -> Task<cosmic::Action<Msg>> {
        // `--settings` is a standalone window with no capture overlays.
        if self.settings.only {
            return Task::none();
        }
        // The permission-checker window (`--permissions` / missing-grant routing) is
        // likewise a standalone window — no capture overlays, and no tiling-WM pause
        // (nothing is being captured). Same guard shape as settings.
        if self.permissions.window.is_some() {
            return Task::none();
        }
        // `--preview <file>`: no capture overlays — open the preview on the active
        // output, mirroring `on_output`'s preview branch.
        if self.preview_mode {
            if let Some((path, is_video)) = self.startup_preview.take() {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                return self.open_external_preview(path, size, is_video);
            }
            return Task::none();
        }
        // DRAGON-295: an IMMEDIATE picker-free capture (`--active-window` / `--active-monitor`,
        // typically from a daemon global hotkey). Resolve the target NOW and drive it straight
        // through the capture pipeline, minting NO overlay windows. On a failure to resolve
        // (no frontmost window, no display under the cursor) we fall through to the normal
        // overlay so the user can still pick. macOS/Windows only; Linux never sets `immediate`.
        #[cfg(not(target_os = "linux"))]
        if let Some(imm) = self.startup_immediate.take() {
            if let Some(task) = self.immediate_capture(imm) {
                return task;
            }
            log::warn!(
                "immediate capture ({imm:?}) could not resolve a target; \
                 falling back to the picker overlay"
            );
        }
        // Tiling-WM handling (DRAGON-154): by default AeroSpace never manages the
        // overlays at all (the pre-order-front chrome strip opts them out of its
        // detection), so there is nothing to pause. The legacy whole-session pause
        // survives behind CCK_AEROSPACE_PAUSE=1; when engaged it is slow (1–3s), so
        // it runs off the UI thread and the overlays are minted from the follow-up
        // `SeedOverlays` message once it's done.
        #[cfg(target_os = "macos")]
        {
            // A pause (if opted into) was already KICKED at `App::init` entry
            // (`early_pause_tiling_wm`), so it has been running in parallel with the
            // whole scene grab. Here we only WAIT for it to finish before minting
            // overlays — usually already done (and instant in the default no-pause
            // mode), so this returns near-instantly instead of paying 1-3s serially.
            crate::util::timing_mark("seed_outputs_mac: awaiting early tiling-WM pause (spawn_blocking)");
            return Task::perform(
                async {
                    crate::util::timing_mark("seed_outputs_mac: early-pause wait started");
                    let _ = tokio::task::spawn_blocking(
                        crate::platform::mac::window::wait_for_early_pause,
                    )
                    .await;
                    crate::util::timing_mark("seed_outputs_mac: early-pause wait done");
                },
                |()| cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::SeedOverlays)),
            );
        }
        // No tiling-WM integration off macOS yet — seed immediately.
        #[cfg(not(target_os = "macos"))]
        self.seed_overlays_mac()
    }

    /// macOS/Windows: mint one capture overlay window per display. Split out of
    /// `seed_outputs_mac` so a slow tiling-WM pause (the CCK_AEROSPACE_PAUSE escape
    /// hatch; a no-op in the default no-pause mode) can fully complete first — when
    /// engaged, the overlays are created into an already-paused WM (no burst).
    #[cfg(not(target_os = "linux"))]
    pub(super) fn seed_overlays_mac(&mut self) -> Task<cosmic::Action<Msg>> {
        crate::util::timing_mark("seed_overlays_mac: minting overlay windows (begin)");
        // macOS (DRAGON-130): this runs AFTER the off-thread tiling-WM pause completed
        // (`seed_outputs_mac` awaits it before emitting `SeedOverlays`). Arm the
        // death-pipe babysitter now so a crash / force-quit mid-session still
        // re-enables AeroSpace (the child restores on pipe EOF). `engage` no-ops
        // (returns None) unless WE actually paused a tiling WM.
        #[cfg(target_os = "macos")]
        {
            self.aerospace_guard = crate::platform::mac::window::AerospaceGuard::engage();
        }
        let mut cmds = Vec::new();
        // Focus the overlay on the display UNDER THE POINTER so Escape / shortcuts
        // work without a click first, right where the user is working; fall back to
        // the primary display (origin 0,0) when the pointer maps to no display. A
        // borderless winit window can become key on mac.
        let mut focus_id: Option<window::Id> = None;
        let mut primary_id: Option<window::Id> = None;
        #[cfg(target_os = "macos")]
        let pointer = Some(crate::platform::mac::global_pointer_position());
        #[cfg(not(target_os = "macos"))]
        let pointer: Option<(i32, i32)> = None;
        for desc in crate::screenshot::output_descs() {
            if self.outputs.iter().any(|o| o.name == desc.name) {
                continue;
            }
            let logical_size = (
                desc.logical_size.0.max(0) as u32,
                desc.logical_size.1.max(0) as u32,
            );
            let (id, open) = super::shell::overlay_window(desc.logical_pos, logical_size);
            if desc.logical_pos == (0, 0) {
                primary_id = Some(id);
            }
            if let Some((px, py)) = pointer {
                let (ox, oy) = desc.logical_pos;
                let (w, h) = (logical_size.0 as i32, logical_size.1 as i32);
                if px >= ox && px < ox + w && py >= oy && py < oy + h {
                    focus_id = Some(id);
                }
            }
            cmds.push(open);
            // Title the window with the display name (hidden in the UI, but AppKit
            // keeps it) so `place_overlay` can match this exact NSWindow to its
            // display when the `OverlayOpened` handler configures it natively.
            cmds.push(self.set_window_title(desc.name.clone(), id));
            self.outputs.push(OutputState {
                output: desc.name.clone(),
                id,
                name: desc.name,
                logical_pos: desc.logical_pos,
                logical_size,
                #[cfg(target_os = "macos")]
                placed: std::cell::Cell::new(false),
            });
        }
        if self.outputs.is_empty() {
            log::warn!(
                "no displays returned for the capture overlay — Screen Recording \
                 permission may be denied (grant it in System Settings and restart)."
            );
        }
        if let Some(id) = focus_id
            .or(primary_id)
            .or_else(|| self.outputs.first().map(|o| o.id))
        {
            // macOS (DRAGON-186 Phase 5b): grab the previously-active window's pixels
            // NOW, while its owning app is still frontmost, BEFORE `gain_focus` fires.
            // `gain_focus` activates our accessory process, which deactivates that app
            // and re-renders its window in the INACTIVE appearance (grayed traffic
            // lights, dimmed title bar). All our window-pixel grabs otherwise happen
            // AT/AFTER this activation, so they capture the gray look. This synchronous
            // pre-activation grab is the one deterministic point where the active
            // window's LIVE active pixels can still be read; window-mode commit prefers
            // them. Only the frontmost window changes appearance on activation (macOS
            // renders every other window inactive already), so a single grab suffices.
            // DRAGON-204/212: this pre-activation colored grab is consumed ONLY by a
            // DELAYED window capture. A NON-delayed pick re-focuses and re-grabs the picked
            // window FRESH at commit (DRAGON-194's `capture_window_active`, the same reason
            // DRAGON-196 dropped the daemon pre-grab), so the launch grab is redundant there
            // and only cost ~SCK-serialized latency on the critical path — it blocks
            // seed_overlays AND starves the Cocoa event loop that sets the title / places the
            // overlay, which was the bulk of the ~2s window-launch time. A delayed capture
            // (`capture_live`) instead grabs the LIVE post-delay screen and never re-focuses,
            // so for it these pre-activation pixels are the only colored source. Run it ONLY
            // for a delayed window launch; region / monitor / scan never consume it, and a
            // lazy switch INTO window mode can't reinstate a colored grab anyway (our overlay
            // is already frontmost). (Window kind is never Scanner, so the delay test alone
            // captures `capture_live` here.)
            #[cfg(target_os = "macos")]
            if self.mode == Mode::Window && self.configured_delay_secs() > 0 {
                self.grab_active_window_pixels();
            }
            cmds.push(window::gain_focus(id));
        }
        // Arm the button meters for the overlay we just seeded: on a `--video` launch the
        // kind is set at init WITHOUT going through `SetKind`, so `sync_meters` never ran
        // and both channels' armed-idle meters sat idle. This is the macOS overlay-up seam
        // — the parity point for the Linux output-hotplug meter arming. Idempotent (a
        // no-op if nothing changed).
        self.sync_meters();
        crate::util::timing_mark("seed_overlays_mac: overlay open tasks batched (done)");
        Task::batch(cmds)
    }

    /// macOS (DRAGON-186 Phase 5b, fixed DRAGON-189): synchronously capture the pixels
    /// of the window(s) that carry the ACTIVE (colored-traffic-light) appearance RIGHT
    /// NOW, before our overlay's `gain_focus` activation deactivates the owning app
    /// (which re-renders its front window in the gray inactive appearance). Deposits into
    /// `self.active_win_px` keyed by toplevel id; the window-mode commit path prefers
    /// these active pixels over any post-activation grab, independent of the freeze
    /// setting.
    ///
    /// DRAGON-189: the target is the FRONTMOST APP's front window
    /// (`NSWorkspace.frontmostApplication`), NOT SCK's per-window `isActive` flag. That
    /// flag can point at a window the OS is *not* coloring (e.g. a background app's
    /// window while Finder — which often has no standard window — is truly frontmost),
    /// so the old grab cached the wrong window and the window the user actually captured
    /// missed the cache and fell through to a gray post-activation grab. We also keep
    /// the SCK-`active` window as a secondary grab (cheap, keyed by its own id) so a
    /// capture of it is covered too. Only the FRONT window of the front app changes
    /// appearance on our activation, so grabbing it colored is the load-bearing fix.
    #[cfg(target_os = "macos")]
    pub(super) fn grab_active_window_pixels(&mut self) {
        crate::util::timing_mark("grab_active_window_pixels: pre-activation active-window grab (begin)");
        // DRAGON-189: prefer the DAEMON's pre-activation grab if it handed us one. The
        // resident daemon grabs the frontmost window's COLORED pixels at hotkey/menu time,
        // BEFORE spawning this child, while the target is still frontmost; by the time this
        // child boots and reaches here the target has lost frontmost (a self-grab now would
        // be GRAY). The handoff arrives via CCK_ACTIVE_WIN_PNG / CCK_ACTIVE_WIN_ID (keyed by
        // the exact windowID, so a user with several windows of one app gets the right one).
        // Absent for a direct `--window` CLI launch (no daemon) — that path falls through to
        // the live grab below, which for a CLI launch is still pre-`gain_focus` and colored.
        if let Some((id, img)) = crate::platform::mac::active_window::load_from_env() {
            let score = crate::platform::mac::traffic_light_colorfulness(&img, 160, 60);
            log::debug!(
                "DRAGON-189 grab_active_window_pixels: seeded from daemon handoff id={id} \
                 traffic_light_colorfulness={score} ({})",
                if score > 60 { "COLORED" } else if score < 10 { "GRAY" } else { "ambiguous" }
            );
            self.active_win_px.insert(id, img);
            crate::util::timing_mark("grab_active_window_pixels: seeded from daemon handoff (colored)");
            // Still run the live grab below as a supplement: it covers the SCK-`active`
            // window too, and is a no-op net cost if it returns the same/other windows.
        }
        // Collect the candidate windows to grab, de-duped by id: the frontmost app's
        // FRONT window (the one the OS colors and the user is looking at), plus SCK's
        // `active` window as a secondary. Only these two matter for the colored-vs-gray
        // fix, so we don't grab the front app's whole window list (keeps the synchronous
        // pre-mint grab cheap — every extra SCK grab is ~40-70ms on the critical path).
        let mut targets: Vec<crate::platform::compositor::Toplevel> = Vec::new();
        if let Some(pid) = crate::platform::mac::frontmost_app_pid()
            && let Some(front_win) = crate::platform::mac::list_windows_owned_by(pid).into_iter().next()
        {
            targets.push(front_win);
        }
        for t in crate::platform::mac::list_windows() {
            if t.active && !targets.iter().any(|x| x.id == t.id) {
                targets.push(t);
            }
        }
        if targets.is_empty() {
            crate::util::timing_mark("grab_active_window_pixels: no front-app / active window (skip)");
            return;
        }
        let front = {
            let ws = objc2_app_kit::NSWorkspace::sharedWorkspace();
            ws.frontmostApplication().and_then(|a| a.localizedName().map(|n| n.to_string()))
        };
        let mut grabbed = 0;
        for t in &targets {
            // Never clobber a colored daemon handoff for this id with a (now-gray) live
            // re-grab: the daemon's pre-activation pixels are authoritative (DRAGON-189).
            if self.active_win_px.contains_key(&t.id) {
                continue;
            }
            if let Some(img) = crate::platform::mac::capture_window(&t.id) {
                // DRAGON-189 empirical probe (debug-gated): the grabbed window's
                // traffic-light colorfulness, so `RUST_LOG=debug` on a real launch shows
                // these pixels are COLORED (>60) at grab time, not GRAY (<10, the bug).
                let score = crate::platform::mac::traffic_light_colorfulness(&img, 160, 60);
                log::debug!(
                    "DRAGON-189 grab_active_window_pixels: front_app={front:?} window={:?} \
                     traffic_light_colorfulness={score} ({})",
                    t.title,
                    if score > 60 { "COLORED" } else if score < 10 { "GRAY (bug)" } else { "ambiguous" }
                );
                self.active_win_px.insert(t.id.clone(), img);
                grabbed += 1;
            }
        }
        if grabbed > 0 {
            crate::util::timing_mark("grab_active_window_pixels: active-window pixels grabbed (colored, pre-activation)");
        } else {
            crate::util::timing_mark("grab_active_window_pixels: capture_window returned None (fallback to live)");
        }
        // Debug dump hook (DRAGON-189 before/after proof only): with CCK_DUMP_ACTIVE_PX=<dir>
        // set, write every active_win_px entry to <dir>/active-<id>.png and log its
        // traffic-light score, so a real WINDOW-flow run can be eyeballed. Never set in
        // production; a no-op otherwise.
        if let Some(dir) = std::env::var_os("CCK_DUMP_ACTIVE_PX") {
            let dir = std::path::PathBuf::from(dir);
            let _ = std::fs::create_dir_all(&dir);
            for (id, img) in &self.active_win_px {
                let score = crate::platform::mac::traffic_light_colorfulness(img, 160, 60);
                let p = dir.join(format!("active-{id}.png"));
                let _ = img.save(&p);
                log::debug!(
                    "DRAGON-189 dump active_win_px id={id} score={score} ({}) -> {}",
                    if score > 60 { "COLORED" } else if score < 10 { "GRAY" } else { "ambiguous" },
                    p.display()
                );
            }
        }
    }

    /// macOS/Windows: apply the native NSWindow tweaks to a capture overlay once it has
    /// opened (view installed). On macOS this hides the titlebar chrome, raises it above
    /// the menu bar, and sets the exact full-display frame (the tiling WM is globally
    /// paused for the session, so the overlay stays put — no per-frame fighting needed).
    /// Retries briefly if the window's async-set title hasn't landed yet. Ignores ids
    /// that aren't ours.
    #[cfg(not(target_os = "linux"))]
    // The macOS retry branch is a cfg'd `{ return … }` block followed by a cfg'd-out
    // `not(macos)` tail; dropping `return` there is a compile error, so allow the lint on
    // macOS only.
    #[cfg_attr(target_os = "macos", allow(clippy::needless_return))]
    pub(super) fn configure_overlay(&self, id: window::Id, attempt: u8) -> Task<cosmic::Action<Msg>> {
        let Some(o) = self.outputs.iter().find(|o| o.id == id) else {
            return Task::none();
        };
        #[cfg(target_os = "macos")]
        {
            // How long to keep polling for the NSWindow title to land (set by an async
            // change-title task, so it can lag the open-task completion): 30 × 40ms ≈
            // 1.2s, far longer than the one-or-two frames it normally takes.
            const MAX_ATTEMPTS: u8 = 30;
            const RETRY_MS: u64 = 40;
            if crate::platform::mac::window::place_overlay(&o.name, o.logical_pos, o.logical_size) {
                // Opt the overlay out of the user's tiling WM via the portable seam. The
                // pre-order-front chrome strip already classified it as an AeroSpace popup;
                // this backstops that title-scoped, through the shared entry point.
                crate::platform::opt_out_of_tiling(&o.name);
                // Placed. A tiling WM never manages the overlay (the DRAGON-154
                // chrome-strip opt-out, or the escape-hatch pause), so nothing
                // re-homes it — no burst needed. DRAGON-204: mark placed so the view
                // stops rendering transparent and draws the UI now that the window sits
                // at its final full-display frame (no visible clamp-then-reframe shift).
                o.placed.set(true);
                crate::util::timing_mark("configure_overlay: place_overlay MATCHED (overlay visible + framed) *** USER SEES UI ***");
                return Task::none();
            }
            crate::util::timing_mark("configure_overlay: place_overlay not matched yet (title lag, will retry)");
            if attempt >= MAX_ATTEMPTS {
                log::warn!(
                    "overlay for '{}' never matched its NSWindow after {MAX_ATTEMPTS} attempts \
                     — it may be mispositioned",
                    o.name
                );
                // DRAGON-204: give up gating — draw the overlay even if it never matched,
                // so a window we couldn't place isn't left permanently invisible.
                o.placed.set(true);
                return Task::none();
            }
            // Title not applied yet — re-emit to try again shortly (place_overlay is
            // idempotent; a stable unique key means this converges once it lands).
            return Task::perform(
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(RETRY_MS)).await;
                },
                move |()| {
                    cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::OverlayOpened(
                        id,
                        attempt + 1,
                    )))
                },
            );
        }
        // DRAGON-229 stage 7: Windows places the overlay natively via SetWindowPos
        // (winit treats Position::Specific as LOGICAL and over-scales physical coords on
        // HiDPI), matching THIS process's overlay window by the title = display name.
        // Retries briefly if the async-set title hasn't landed yet (like the mac path).
        #[cfg(target_os = "windows")]
        {
            const MAX_ATTEMPTS: u8 = 30;
            const RETRY_MS: u64 = 40;
            let placed = crate::platform::windows::window::place_overlay(
                &o.name,
                o.logical_pos,
                o.logical_size,
                // DRAGON-298: a transient capture SELECTOR must show NO taskbar button (clears
                // the winit-set WS_EX_APPWINDOW that overrides its WS_EX_TOOLWINDOW).
                false,
                // The selector needs keyboard focus for Escape / shortcuts.
                true,
            );
            if placed || attempt >= MAX_ATTEMPTS {
                if attempt >= MAX_ATTEMPTS {
                    log::warn!(
                        "overlay for '{}' never matched its window after {MAX_ATTEMPTS} attempts \
                         — it may be mispositioned",
                        o.name
                    );
                }
                // DRAGON-280: now the overlay is on-screen (phase 2 done), re-assert HWND_TOPMOST
                // once. place_overlay's two-phase off-screen->on-screen show can race a fullscreen
                // app's own topmost / DWM independent-flip state; a cheap z-order-only SetWindowPos
                // after the present grace pins us above it. No-op if the window is already gone.
                if placed {
                    // Opt the overlay out of the user's tiling WM via the portable seam.
                    // `place_overlay` already set the komorebi bit pre-show; this is the
                    // idempotent, title-scoped confirmation through the shared entry point.
                    crate::platform::opt_out_of_tiling(&o.name);
                    crate::platform::windows::window::reassert_topmost(&o.name);
                }
                // DRAGON-276: during a COUNTDOWN or RECORDING the overlay must be click-through
                // so the user can use the screen being captured (the SELECTION overlay stays
                // interactive — you drag a region). The overlay is recreated per phase, so this
                // only ever tags the countdown/recording overlays, never the selection one.
                if self.countdown.is_some() || self.recording.is_some() {
                    crate::platform::windows::window::set_click_through(&o.name, true);
                }
                Task::none()
            } else {
                Task::perform(
                    async move {
                        tokio::time::sleep(std::time::Duration::from_millis(RETRY_MS)).await;
                    },
                    move |()| {
                        cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::OverlayOpened(
                            id,
                            attempt + 1,
                        )))
                    },
                )
            }
        }
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        {
            let _ = (o, attempt);
            Task::none()
        }
    }

    /// macOS (DRAGON-130 crash-dodge): strip the native titlebar buttons from the
    /// windowed preview once it has opened (view installed), so a window we had to open
    /// `decorations: true` (to keep `Titled | Resizable` in the mask and dodge the
    /// borderless `is_zoomed` crash — see `shell::preview_window`) still reads as a clean
    /// CSD window. Matches the NSWindow by its title; retries briefly if the async-set
    /// title hasn't landed yet, then gives up. Only acts on the current preview window.
    #[cfg(target_os = "macos")]
    pub(super) fn finalize_preview_window(&mut self, id: window::Id, attempt: u8) -> Task<cosmic::Action<Msg>> {
        // Only our current windowed preview qualifies.
        if self.preview.as_ref().is_none_or(|p| p.window != id) {
            return Task::none();
        }
        const MAX_ATTEMPTS: u8 = 30;
        const RETRY_MS: u64 = 40;
        // The windowed preview is only ever minted AFTER the grab (a window pick covers the
        // grab with the fullscreen overlay, then swaps to this window, DRAGON-219), so it
        // always takes focus for real here — there is no order-front-only pre-open phase.
        let matched =
            crate::platform::mac::window::finalize_preview_window(super::shell::PREVIEW_WINDOW_TITLE);
        if matched {
            // Window vibrancy (DRAGON-268): the windowed preview (a CSD toplevel) is the mac
            // analog of Linux's frosted window / Windows' Mica — reveal the winit-inserted
            // vibrancy by clearing its Metal layer. NEVER the fullscreen OVERLAY preview
            // (that path never reaches here). Gated on the SAME frosted-windows signal
            // (`self.glass`, `Some` unless `CCK_NO_GLASS`) AND that this is actually a windowed
            // preview, exactly like the Windows `frosted_windowed` gate. The chrome already
            // paints translucent from `self.glass`.
            let frosted_windowed = self.glass.is_some_and(|g| g.frosted_windows)
                && self.preview.as_ref().is_some_and(|p| p.surface.is_window());
            if frosted_windowed {
                crate::platform::mac::window::enable_window_vibrancy(
                    super::shell::PREVIEW_WINDOW_TITLE,
                );
            }
            // DRAGON-219: a match means this swapped-in window is now placed + focused, so the
            // fullscreen overlay cover that covered the grab (kept until now so the window
            // mapped under it with no flash) has served its purpose — close it. On macOS this
            // is the reliable "window is up" signal (the window's first `Resized` can fire
            // before native placement). No-op for a non-swap open (no cover pending).
            if let Some(overlay) = self.grab_overlay_closing.take() {
                return super::shell::close_surface(overlay);
            }
            return Task::none();
        }
        if attempt >= MAX_ATTEMPTS {
            log::warn!(
                "preview window never matched its NSWindow after {MAX_ATTEMPTS} attempts \
                 — the titlebar buttons may still show"
            );
            return Task::none();
        }
        Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(RETRY_MS)).await;
            },
            move |()| {
                cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::PreviewOpened(
                    id,
                    attempt + 1,
                )))
            },
        )
    }

    /// Windows (DRAGON-233 fix 6): the windowed preview was opened `visible:false`; once its
    /// async-set title lands, CENTER it on its target monitor's work area and show it — the
    /// Windows sibling of the mac finalize above. DRAGON-302: no komorebi opt-out, so a tiling
    /// WM tiles it like a normal window (the tiler then owns placement). Matched by
    /// [`super::shell::PREVIEW_WINDOW_TITLE`]. winit opens a new toplevel at the OS
    /// default cascade (not centered — the user's off-center report), so
    /// [`crate::platform::windows::window::show_centered`] positions it natively while
    /// still hidden (no flash), keeping the restyle-before-show opt-out ordering intact.
    /// The anchor is the capture output when known (`preview_output`), else the display
    /// under the pointer (`--preview`). Retries briefly if the title hasn't landed yet
    /// (same 30 x 40ms budget as `configure_overlay`), then gives up loudly. Only acts
    /// on the current preview window. No `WS_EX_TOOLWINDOW`: it keeps its taskbar button.
    #[cfg(target_os = "windows")]
    pub(super) fn finalize_preview_window(&mut self, id: window::Id, attempt: u8) -> Task<cosmic::Action<Msg>> {
        // Only our current windowed preview qualifies.
        if self.preview.as_ref().is_none_or(|p| p.window != id) {
            return Task::none();
        }
        const MAX_ATTEMPTS: u8 = 30;
        const RETRY_MS: u64 = 40;
        let name = self.preview_output.as_ref().map(|(n, _)| n.clone());
        let (pos, size) = crate::platform::windows::window::preview_overlay_rect(name.as_deref());
        let monitor = (pos.0, pos.1, size.0 as i32, size.1 as i32);
        // Mica applies only to the WINDOWED preview (a CSD toplevel) — never the fullscreen
        // OVERLAY preview, exactly like Linux excludes its layer-shell overlay from frosting.
        // Gated on the SAME frosted-windows signal Linux uses (`self.glass`, Some only on
        // Win11 22H2+ and not `CCK_NO_GLASS`-disabled), so the unified toggle turns it off too.
        let frosted_windowed = self.glass.is_some_and(|g| g.frosted_windows)
            && self.preview.as_ref().is_some_and(|p| p.surface.is_window());
        let shown =
            crate::platform::windows::window::show_centered(super::shell::PREVIEW_WINDOW_TITLE, monitor);
        if shown {
            // DRAGON-281: the native show is confirmed — record it so `sub_preview_finalize`
            // stops re-driving this finalize (it was the safety net for the one-shot open
            // follow-up not being delivered while cck was a background process).
            self.preview_shown_confirmed = Some(id);
            // Native DWM caption buttons (DRAGON-284): the windowed preview is a CSD toplevel
            // like settings, so install the native min/max/close over its owned header.
            // Idempotent (once per HWND); safe under this arm's retry / DRAGON-281 re-drive.
            // Unconditional (not gated on Mica) — the buttons are chrome, not glass.
            crate::platform::windows::caption::install_native_caption_buttons(
                super::shell::PREVIEW_WINDOW_TITLE,
            );
            if frosted_windowed {
                // Mica backdrop (DRAGON-267): the windowed preview is now shown — apply the
                // DWM Mica material. The chrome already paints translucent from `self.glass`.
                crate::platform::windows::window::apply_mica(super::shell::PREVIEW_WINDOW_TITLE);
            }
            // DRAGON-305: a WINDOWED single-window capture pre-opened the fullscreen BLOCKER cover
            // to hide the grab/compose, then swapped it for THIS window (`swap_neutral_spinner_to_window`,
            // which set `grab_overlay_closing`). The window mapped UNDER the cover (still painting
            // `grab_cover_view`) with no desktop flash; now it is shown + placed, so the cover has
            // served its purpose — close it. The mac sibling closes it here too (surfaces.rs); no-op
            // for a normal (non-swap) preview open, where nothing is pending.
            if let Some(cover) = self.grab_overlay_closing.take() {
                return super::shell::close_surface(cover);
            }
        }
        if shown || attempt >= MAX_ATTEMPTS {
            if attempt >= MAX_ATTEMPTS {
                log::warn!(
                    "preview window never matched its title after {MAX_ATTEMPTS} attempts \
                     — komorebi may tile it and it may stay hidden"
                );
            }
            Task::none()
        } else {
            Task::perform(
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(RETRY_MS)).await;
                },
                move |()| {
                    cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::PreviewOpened(
                        id,
                        attempt + 1,
                    )))
                },
            )
        }
    }

    /// Windows (DRAGON-233 fix 5): natively place the fullscreen OVERLAY preview once it
    /// has opened — the sibling of mac's `finalize_preview_overlay` below. Reuses the
    /// capture overlay's [`crate::platform::windows::window::place_overlay`]: OR-in
    /// `WS_EX_DLGMODALFRAME` before the first show (komorebi opt-out, race-free), set the
    /// exact physical full-display rect topmost, and activate it so the preview hotkeys
    /// (Save / Copy / Escape) work immediately. Matched by the async-set
    /// [`super::shell::PREVIEW_OVERLAY_TITLE`], so it polls briefly like the others, then
    /// gives up loudly. Only acts on the current preview window.
    #[cfg(target_os = "windows")]
    pub(super) fn finalize_preview_overlay(
        &mut self,
        id: window::Id,
        pos: (i32, i32),
        size: (u32, u32),
        attempt: u8,
    ) -> Task<cosmic::Action<Msg>> {
        if self.preview.as_ref().is_none_or(|p| p.window != id) {
            return Task::none();
        }
        const MAX_ATTEMPTS: u8 = 30;
        const RETRY_MS: u64 = 40;
        let placed = crate::platform::windows::window::place_overlay(
            super::shell::PREVIEW_OVERLAY_TITLE,
            pos,
            size,
            // DRAGON-298: the preview editor KEEPS its taskbar button (leave WS_EX_APPWINDOW set).
            true,
            // DRAGON-305: a normal overlay preview activates for its hotkeys; the pre-open BLOCKER
            // cover (`win_preview_preopen`) shows NON-ACTIVATING so it can't steal the target's
            // foreground during the concurrent grab. It is swapped for the real window afterward.
            !self.win_preview_preopen,
        );
        if placed {
            // DRAGON-281: the overlay is placed + shown — record it so `sub_preview_finalize`
            // stops re-driving this finalize (the safety net for the one-shot open follow-up
            // not being delivered while cck was a background process).
            self.preview_shown_confirmed = Some(id);
        }
        if placed || attempt >= MAX_ATTEMPTS {
            if attempt >= MAX_ATTEMPTS {
                log::warn!(
                    "overlay preview never matched its window after {MAX_ATTEMPTS} attempts \
                     — it may be mispositioned"
                );
            }
            Task::none()
        } else {
            Task::perform(
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(RETRY_MS)).await;
                },
                move |()| {
                    cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::PreviewOverlayOpened(
                        id,
                        pos,
                        size,
                        attempt + 1,
                    )))
                },
            )
        }
    }

    /// macOS: natively place the fullscreen OVERLAY preview once it has opened (view
    /// installed): raise it to the shielding level, cover the display's full logical
    /// rect (menu bar included), erase the crash-dodge titlebar — the exact
    /// [`crate::platform::mac::window::place_overlay`] recipe the capture overlays
    /// use, matched by the distinct [`super::shell::PREVIEW_OVERLAY_TITLE`]. Then take
    /// keyboard focus so the preview hotkeys (Save / Copy / Escape) work immediately —
    /// the PlainWindows stand-in for the Linux overlay's exclusive keyboard grab (a
    /// borderless window can become key on mac). Matches the NSWindow by its
    /// async-set title, so it polls briefly like `finalize_preview_window`. Only acts
    /// on the current preview window.
    #[cfg(target_os = "macos")]
    pub(super) fn finalize_preview_overlay(
        &self,
        id: window::Id,
        pos: (i32, i32),
        size: (u32, u32),
        attempt: u8,
    ) -> Task<cosmic::Action<Msg>> {
        if self.preview.as_ref().is_none_or(|p| p.window != id) {
            return Task::none();
        }
        const MAX_ATTEMPTS: u8 = 30;
        const RETRY_MS: u64 = 40;
        if crate::platform::mac::window::place_overlay(
            super::shell::PREVIEW_OVERLAY_TITLE,
            pos,
            size,
        ) {
            // DRAGON-216: while pre-opening to cover the grab, `place_overlay` has placed +
            // order-fronted it (non-key) — but DON'T take focus yet, which would flip the
            // picked window off frontmost mid-grab. `WindowGrabbed` clears the flag and
            // re-takes focus for real. A normal (deferred) open takes focus immediately.
            return if self.mac_preview_preopen {
                Task::none()
            } else {
                window::gain_focus(id)
            };
        }
        if attempt >= MAX_ATTEMPTS {
            log::warn!(
                "overlay preview never matched its NSWindow after {MAX_ATTEMPTS} attempts \
                 — it may be mispositioned"
            );
            return Task::none();
        }
        Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(RETRY_MS)).await;
            },
            move |()| {
                cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::PreviewOverlayOpened(
                    id,
                    pos,
                    size,
                    attempt + 1,
                )))
            },
        )
    }

    /// A fullscreen `Layer::Overlay` surface for the post-capture preview, on the
    /// capture's monitor. Exclusive keyboard so its hotkeys (Save / Copy / Cancel) work
    /// immediately without a click first; full input so the action buttons are clickable.
    #[cfg(target_os = "linux")]
    pub(super) fn preview_surface(
        &self,
        output: OutputHandle,
        id: window::Id,
    ) -> Task<cosmic::Action<Msg>> {
        self.preview_surface_on(IcedOutput::Output(output), id)
    }

    /// Like [`Self::preview_surface`] but on the compositor's *active* output (the one the
    /// user is on) — used by `--preview`, which has no capture selection to anchor to. The
    /// real monitor size arrives later via the surface's resize event.
    #[cfg(target_os = "linux")]
    pub(super) fn preview_surface_active(&self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        self.preview_surface_on(IcedOutput::Active, id)
    }

    #[cfg(target_os = "linux")]
    fn preview_surface_on(&self, output: IcedOutput, id: window::Id) -> Task<cosmic::Action<Msg>> {
        // DRAGON-216: a window pick pre-opens its spinner FOCUS-NEUTRAL so it can't steal
        // the picked toplevel's focus during the grab; `WindowGrabbed` promotes it.
        if self.window_spinner_neutral {
            super::shell::preview_surface_neutral(output, id)
        } else {
            super::shell::preview_surface(output, id)
        }
    }

    /// Exotic platforms (not Linux / macOS / Windows): no fullscreen overlay preview
    /// implementation, so these inert stubs keep the seam total for the
    /// compiled-but-unreachable overlay branch (`preview_surface_for` forces the WINDOW
    /// there via the cfg! fallback). macOS and Windows both grew a real overlay preview
    /// (`finalize_preview_overlay`), so they no longer route through here — hence the
    /// `not(windows)` narrowing (DRAGON-233 fix 5) that keeps them dead-code-free.
    #[cfg(all(not(target_os = "linux"), not(target_os = "macos"), not(target_os = "windows")))]
    pub(super) fn preview_surface(
        &self,
        output: OutputHandle,
        id: window::Id,
    ) -> Task<cosmic::Action<Msg>> {
        let _ = (output, id);
        Task::none()
    }

    #[cfg(all(not(target_os = "linux"), not(target_os = "macos"), not(target_os = "windows")))]
    pub(super) fn preview_surface_active(&self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let _ = id;
        Task::none()
    }

    /// Recreate every overlay surface click-through except the toolbar's rect (the
    /// active chip — countdown timer or recording stop); everything else is
    /// click-through so the screen can be used/rearranged. Keyboard stays on
    /// (OnDemand) so Escape still works. Shared by the countdown and recording.
    /// `toolbar_layout` reflects the now-active state, so it returns the chip rect.
    #[cfg(target_os = "linux")]
    pub(super) fn recreate_active_overlays(&mut self) -> Task<cosmic::Action<Msg>> {
        let plans: Vec<(OutputHandle, window::Id, window::Id, Option<cosmic::iced::Rectangle>)> = self
            .outputs
            .iter()
            .map(|o| {
                let zone = self.toolbar_layout(o).map(|(r, _)| r);
                (o.output.clone(), o.id, window::Id::unique(), zone)
            })
            .collect();
        let mut cmds = Vec::new();
        for (output, old_id, new_id, zone) in plans {
            cmds.push(super::shell::close_surface(old_id));
            if let Some(o) = self.outputs.iter_mut().find(|o| o.output == output) {
                o.id = new_id;
            }
            // With the controls in the system tray there's no in-frame toolbar, so the
            // overlay is fully click-through (empty input zone — no invisible clickable
            // rect stealing input) and takes NO keyboard interactivity at all — the
            // recorded desktop keeps completely normal focus. Recording control is
            // the tray menu (plus portal GlobalShortcuts where the desktop ships
            // them, delivered focus-free regardless of our surfaces). This mode
            // HISTORICALLY grabbed the keyboard (Exclusive) for the hotkeys, which
            // held keyboard focus hostage from every window for the whole recording
            // (DRAGON-109) — never again. In-frame recording keeps OnDemand (the
            // toolbar takes focus on click) so its hotkeys work after a click while
            // typing in the recorded app isn't captured.
            // DRAGON-172: gate on `tray_hides_toolbar`, NOT `tray.is_some()` — a macOS
            // daemon relay in toolbar-placement mode keeps the in-frame toolbar, so the
            // overlay must keep the toolbar's input zone (not go fully click-through).
            let tray = self.tray_hides_toolbar;
            let input_zone = if tray {
                Some(Vec::new())
            } else {
                Some(zone.map(|r| vec![r]).unwrap_or_default())
            };
            cmds.push(self.overlay_surface(output, new_id, input_zone, !tray));
        }
        Task::batch(cmds)
    }

    /// macOS: the overlay windows stay up (`view_window` switches to the countdown /
    /// recording chip off state), but the WINDOWS must stop eating input so the desktop
    /// underneath is usable (DRAGON-151): make every overlay click-through (iced mouse
    /// passthrough → winit cursor-hittest → `NSWindow.ignoresMouseEvents`). AppKit has
    /// no per-rect input region, so the toolbar chip stays reachable via a poll
    /// (`sub_passthrough`) that re-solidifies just the overlay whose chip rect the
    /// pointer is over.
    ///
    /// Windows (DRAGON-276 root cause): the overlays likewise stay up, and the SAME
    /// poll shape applies — every overlay window goes click-through here
    /// (`WS_EX_TRANSPARENT|WS_EX_LAYERED` via [`set_click_through`], matched by title
    /// like `place_overlay`), and `passthrough_poll` re-solidifies the hovered chip.
    /// Win32 has no per-rect input region either, so the mac poll is the exact analog.
    #[cfg(not(target_os = "linux"))]
    #[cfg_attr(
        any(target_os = "macos", target_os = "windows"),
        allow(clippy::needless_return)
    )]
    pub(super) fn recreate_active_overlays(&mut self) -> Task<cosmic::Action<Msg>> {
        #[cfg(target_os = "macos")]
        {
            self.passthrough_active = true;
            self.passthrough_solid = None;
            return Task::batch(
                self.outputs
                    .iter()
                    .map(|o| window::enable_mouse_passthrough(o.id)),
            );
        }
        #[cfg(target_os = "windows")]
        {
            self.passthrough_active = true;
            self.passthrough_solid = None;
            for o in &self.outputs {
                crate::platform::windows::window::set_click_through(&o.name, true);
            }
            return Task::none();
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        Task::none()
    }

    /// macOS (DRAGON-151): one `sub_passthrough` tick — find the overlay whose
    /// toolbar chip is under the pointer and make just that window solid (mouse
    /// passthrough OFF); the previously solid one (if different) goes back to
    /// click-through. No hover change = no tasks.
    #[cfg(target_os = "macos")]
    pub(super) fn passthrough_poll(&mut self) -> Task<cosmic::Action<Msg>> {
        if !self.passthrough_active {
            return Task::none();
        }
        let (px, py) = crate::platform::mac::global_pointer_position();
        let hovered = self.outputs.iter().find_map(|o| {
            let (r, _) = self.toolbar_layout(o)?;
            let local = cosmic::iced::Point::new(
                px as f32 - o.logical_pos.0 as f32,
                py as f32 - o.logical_pos.1 as f32,
            );
            r.contains(local).then_some(o.id)
        });
        if hovered == self.passthrough_solid {
            return Task::none();
        }
        let mut cmds = Vec::new();
        if let Some(prev) = self.passthrough_solid.take() {
            cmds.push(window::enable_mouse_passthrough(prev));
        }
        if let Some(id) = hovered {
            cmds.push(window::disable_mouse_passthrough(id));
            self.passthrough_solid = Some(id);
        }
        Task::batch(cmds)
    }

    /// Windows (DRAGON-276): the mac hover poll, Win32 form — one `sub_passthrough`
    /// tick while the countdown/recording overlays are click-through. Find the overlay
    /// whose toolbar chip is under the pointer (cursor mapped into the overlay's
    /// LOGICAL space by the platform helper, compared against the same
    /// `toolbar_layout` rect the view draws) and make just that window solid again;
    /// the previously solid one (if different) goes back to click-through. The Win32
    /// ex-style flip is synchronous, so no tasks are minted.
    #[cfg(target_os = "windows")]
    pub(super) fn passthrough_poll(&mut self) -> Task<cosmic::Action<Msg>> {
        if !self.passthrough_active {
            return Task::none();
        }
        let hovered = self.outputs.iter().find_map(|o| {
            let (r, _) = self.toolbar_layout(o)?;
            let (lx, ly) =
                crate::platform::windows::window::cursor_in_window_logical(&o.name)?;
            r.contains(cosmic::iced::Point::new(lx, ly)).then_some(o.id)
        });
        if hovered == self.passthrough_solid {
            return Task::none();
        }
        if let Some(prev) = self.passthrough_solid.take()
            && let Some(o) = self.outputs.iter().find(|o| o.id == prev)
        {
            crate::platform::windows::window::set_click_through(&o.name, true);
        }
        if let Some(id) = hovered {
            if let Some(o) = self.outputs.iter().find(|o| o.id == id) {
                crate::platform::windows::window::set_click_through(&o.name, false);
            }
            self.passthrough_solid = Some(id);
        }
        Task::none()
    }


    /// Destroy the capture overlay surfaces while keeping the tracked outputs.
    /// The overlay is a `Layer::Overlay` surface, so it stacks *above* the
    /// settings toplevel — hide it while settings is open. Closing settings ends
    /// the instance, so the overlay is never recreated afterwards.
    #[cfg(target_os = "linux")] // macOS hands settings off to a fresh process (DRAGON-153)
    pub(super) fn hide_overlays(&mut self) -> Task<cosmic::Action<Msg>> {
        let cmds: Vec<_> = self
            .outputs
            .iter()
            .map(|o| super::shell::close_surface(o.id))
            .collect();
        Task::batch(cmds)
    }

    /// Windows: destroy the capture overlay surfaces when settings opens — the Linux
    /// `hide_overlays` behaviour (DRAGON-233 fix 2). The capture overlays are
    /// always-on-top `HWND_TOPMOST` windows, so a settings window opened from the
    /// in-overlay gear would be minted BEHIND them and never seen — the user's "gear
    /// flashes, settings never appears, process seems stuck" report. Closing them hands
    /// settings the screen. Closing settings ends the instance, so the overlays are
    /// never recreated; the hidden bootstrap window keeps the event loop alive and
    /// `WindowClosed` on an overlay id is a no-op, so the process does NOT exit here.
    /// `self.outputs` is kept (like Linux) — the ids are simply never addressed again.
    #[cfg(target_os = "windows")]
    pub(super) fn hide_overlays(&mut self) -> Task<cosmic::Action<Msg>> {
        let cmds: Vec<_> = self
            .outputs
            .iter()
            .map(|o| super::shell::close_surface(o.id))
            .collect();
        Task::batch(cmds)
    }

    /// Recreate every overlay surface fully interactive (full input zone +
    /// picking-phase keyboard). Used to return to region select after a countdown,
    /// which had recreated the surfaces click-through. The countdown is already
    /// cleared by the caller, so this mints the picking phase's EXCLUSIVE keyboard
    /// (DRAGON-228) — Escape works again without a focusing click.
    #[cfg(target_os = "linux")]
    pub(super) fn restore_interactive_overlays(&mut self) -> Task<cosmic::Action<Msg>> {
        use cosmic_client_toolkit::sctk::shell::wlr_layer::KeyboardInteractivity;
        let kb = if self.overlay_pick_exclusive() {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::OnDemand
        };
        let plans: Vec<(OutputHandle, window::Id, window::Id)> = self
            .outputs
            .iter()
            .map(|o| (o.output.clone(), o.id, window::Id::unique()))
            .collect();
        let mut cmds = Vec::new();
        for (output, old_id, new_id) in plans {
            cmds.push(super::shell::close_surface(old_id));
            if let Some(o) = self.outputs.iter_mut().find(|o| o.output == output) {
                o.id = new_id;
            }
            cmds.push(super::shell::overlay_surface_with(output, new_id, None, kb));
        }
        Task::batch(cmds)
    }

    /// macOS/Windows: undo `recreate_active_overlays`' click-through — every overlay
    /// solid and interactive again for region select (a no-op on windows that were
    /// never made passthrough).
    #[cfg(not(target_os = "linux"))]
    #[cfg_attr(
        any(target_os = "macos", target_os = "windows"),
        allow(clippy::needless_return)
    )]
    pub(super) fn restore_interactive_overlays(&mut self) -> Task<cosmic::Action<Msg>> {
        #[cfg(target_os = "macos")]
        {
            self.passthrough_active = false;
            self.passthrough_solid = None;
            return Task::batch(
                self.outputs
                    .iter()
                    .map(|o| window::disable_mouse_passthrough(o.id)),
            );
        }
        #[cfg(target_os = "windows")]
        {
            self.passthrough_active = false;
            self.passthrough_solid = None;
            for o in &self.outputs {
                crate::platform::windows::window::set_click_through(&o.name, false);
            }
            return Task::none();
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        Task::none()
    }

    /// Destroy the overlay surfaces but KEEP `self.outputs`, so the overlay can be
    /// recreated after a portal dialog (unlike `destroy_surfaces`, which clears them).
    /// Linux-only: the sole caller is `request_pipewire` (the xdg-portal cast path).
    #[cfg(target_os = "linux")]
    pub(super) fn yield_overlays(&mut self) -> Vec<Task<cosmic::Action<Msg>>> {
        self.outputs
            .iter()
            .map(|o| super::shell::close_surface(o.id))
            .collect()
    }

    #[cfg(target_os = "linux")]
    pub(super) fn on_output(&mut self, ev: OutputEvent, output: OutputHandle) -> Task<cosmic::Action<Msg>> {
        // `--settings` is a standalone window with no capture overlays.
        if self.settings.only {
            return Task::none();
        }
        match ev {
            OutputEvent::Created(Some(info))
                if info.name.is_some()
                    && info.logical_size.is_some()
                    && info.logical_position.is_some() =>
            {
                let (lw, lh) = info.logical_size.unwrap();
                let (lx, ly) = info.logical_position.unwrap();
                // The output's buffer scale (physical / logical) — the current mode's
                // physical width over the logical width, so it captures COSMIC's
                // FRACTIONAL scaling (1.5×) too, not just the integer `scale_factor`.
                // `1.0` when the mode/logical isn't advertised (the 1×, byte-identical
                // fallback). Cached with the capture output so the windowed preview opens
                // at the grab's true on-screen size on scaled displays (DRAGON-221).
                let scale = info
                    .modes
                    .iter()
                    .find(|m| m.current)
                    .map(|m| m.dimensions.0)
                    .filter(|&pw| pw > 0 && lw > 0)
                    .map(|pw| pw as f32 / lw as f32)
                    .filter(|s| s.is_finite() && *s >= 1.0)
                    .unwrap_or(1.0);
                // `--preview <file>`: no capture overlays — open the preview on the active
                // output once outputs exist, then ignore the rest. (The surface targets
                // the active monitor itself; we don't need this event's specific output.)
                if self.preview_mode {
                    if let Some((path, is_video)) = self.startup_preview.take() {
                        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        return self.open_external_preview(path, size, is_video);
                    }
                    return Task::none();
                }
                if self.outputs.iter().any(|o| o.output == output) {
                    return Task::none();
                }
                let id = window::Id::unique();
                self.outputs.push(OutputState {
                    output: output.clone(),
                    id,
                    name: info.name.unwrap(),
                    logical_pos: (lx, ly),
                    logical_size: (lw as u32, lh as u32),
                    #[cfg(target_os = "linux")]
                    scale,
                    #[cfg(target_os = "macos")]
                    placed: std::cell::Cell::new(false),
                });
                // No auto-seeded region: monitors without a region show a "begin
                // drawing" hint instead, and the user draws where they want.
                // Full-input overlay for the interactive UI. DRAGON-228: the picking
                // phase mints EXCLUSIVE keyboard (Escape and the overlay shortcuts
                // work without a focusing click — cosmic-comp never auto-focuses
                // OnDemand layers); non-picking phases mint OnDemand as before.
                use cosmic_client_toolkit::sctk::shell::wlr_layer::KeyboardInteractivity;
                let kb = if self.overlay_pick_exclusive() {
                    KeyboardInteractivity::Exclusive
                } else {
                    KeyboardInteractivity::OnDemand
                };
                super::shell::overlay_surface_with(output, id, None, kb)
            }
            OutputEvent::Removed => {
                if let Some(pos) = self.outputs.iter().position(|o| o.output == output) {
                    let st = self.outputs.remove(pos);
                    super::shell::close_surface(st.id)
                } else {
                    Task::none()
                }
            }
            _ => Task::none(),
        }
    }
}

