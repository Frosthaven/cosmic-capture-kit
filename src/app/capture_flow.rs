use super::*;

/// The extension every still capture is auto-saved with.
pub(in crate::app) const STILL_EXT: &str = "png";
/// The extension every recording is auto-saved with.
pub(in crate::app) const RECORDING_EXT: &str = "mp4";

/// The auto-save file NAME for a still capture whose stem is `stem`.
///
/// One line, and it is the line DRAGON-429 is about. The extension is appended HERE and
/// nowhere else on the still auto-save path, so "can a capture be saved without one?" is a
/// question about this function alone rather than about every branch that reaches it.
/// `App::capture_stem` returns a stem for EVERY kind (window, monitor, region), and all
/// three still branches join it through here.
pub(in crate::app) fn still_save_name(stem: &str) -> String {
    format!("{stem}.{STILL_EXT}")
}

/// The auto-save file NAME for a recording whose stem is `stem`. The `.mp4` twin of
/// [`still_save_name`]; see it for why this exists as a function at all.
pub(in crate::app) fn recording_save_name(stem: &str) -> String {
    format!("{stem}.{RECORDING_EXT}")
}

/// The documented fallback when a save-folder setting is blank. Named because two readers
/// need the same answer and a typo between them would send captures to two places.
pub(in crate::app) const DEFAULT_CAPTURE_DIR: &str = "~/Capture";

/// **Pure**, unit-tested: the bounding box, in CAPTURE coordinates, of a set of output rects
/// given as `(pos, size)` — the whole desktop this session can reach (DRAGON-599).
///
/// The walls a keyboard nudge fights, and the reason they are the DESKTOP's rather than one
/// output's: a drawn region may already span two displays (the drag wall lets you push through
/// a monitor border on purpose, see `widgets::region_selection::wall`), so confining a nudge to
/// the surface the key arrived on would refuse to move such a region at all.
///
/// A bounding BOX is deliberately coarser than the desktop's true shape. On an L-shaped layout
/// it includes corners no output covers, so a nudge can walk a region into dead space, exactly
/// as a DRAG already can. Matching the drag is worth more here than a tighter wall, and the
/// capture itself already handles a region that overhangs an output.
///
/// `None` for an empty set, and zero-sized outputs are skipped: an output with no extent
/// contributes no place a region could go, and folding its origin in would drag the box to it.
pub(in crate::app) fn desktop_bounds_of(
    rects: impl IntoIterator<Item = ((i32, i32), (u32, u32))>,
) -> Option<(i32, i32, i32, i32)> {
    rects
        .into_iter()
        .filter(|(_, (w, h))| *w > 0 && *h > 0)
        .fold(None, |acc: Option<(i32, i32, i32, i32)>, ((x, y), (w, h))| {
            let (r, b) = (x + w as i32, y + h as i32);
            Some(match acc {
                None => (x, y, r, b),
                Some((l0, t0, r0, b0)) => (l0.min(x), t0.min(y), r0.max(r), b0.max(b)),
            })
        })
}

/// WHERE a fresh capture's file is written (DRAGON-467).
///
/// `configured` is the user's save folder for this media kind; `transient` is wherever
/// unsaved captures of that kind live. "Automatically save originals" picks between them, and
/// that is the whole rule:
///
/// * ON (the default, and what every earlier version did unconditionally) writes into the
///   user's folder, so a capture is a file the moment it is taken.
/// * OFF writes into the transient location instead. The editor still opens on it and the
///   clipboard still gets it, but nothing reaches the user's folder until they choose Save.
///   The Windows 11 Snipping Tool's "Automatically save original screenshots" is the same
///   toggle over the same two outcomes.
///
/// **The transient location differs by MEDIUM**, and the split is not cosmetic (DRAGON-467
/// review, major 3). Stills go to the session runtime directory, which is a tmpfs and
/// therefore RAM; a few MB written once is exactly what that is for. Recordings go to a
/// disk-backed cache folder ([`crate::util::transient_recording_dir`]) because a take buffers
/// its live `.recording` temp AND its finished file, and filling a RAM-backed
/// `$XDG_RUNTIME_DIR` (10% of memory by default) would ENOSPC in the middle of the take.
/// [`transient_dir`] picks the right one; this function only chooses between the two columns.
///
/// The transient file is deliberately NOT cleaned up when the editor closes: on Linux the
/// clipboard worker is a detached process holding a `file://` URI for a recording, so removing
/// the file would break a paste the user can still perform. Accumulation is bounded by AGE
/// instead ([`crate::util::sweep_transient_recordings`] at
/// [`crate::util::TRANSIENT_MAX_AGE`]), and the stills' runtime dir is the OS's to clear at
/// logout.
///
/// Pure; unit-tested in `capture_dir_tests`.
pub(in crate::app) fn capture_write_dir(
    save_originals: bool,
    configured: &std::path::Path,
    transient: &std::path::Path,
) -> std::path::PathBuf {
    if save_originals { configured.to_path_buf() } else { transient.to_path_buf() }
}

/// The transient location for a capture of this MEDIUM (DRAGON-467 review, major 3): the
/// disk-backed cache folder for a recording, the session runtime directory for a still.
///
/// Falls back to the runtime directory when the OS offers no cache dir at all. That is the
/// tmpfs risk the split exists to avoid, but it only arises where there is nowhere better,
/// and a capture that lands somewhere beats one that refuses to start.
pub(in crate::app) fn transient_dir(is_video: bool) -> std::path::PathBuf {
    if is_video
        && let Some(dir) = crate::util::transient_recording_dir()
    {
        return dir;
    }
    std::path::PathBuf::from(crate::util::runtime_dir())
}

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

// ── The tray-menu dropdown hold (Linux, DRAGON-600) ──────────────────────────
//
// A child launched from the tray menu finds the host's dropdown STILL ON SCREEN, and it
// stays there until something takes keyboard focus away from it. That something is us:
// `picking_phase` above mints the capture overlays EXCLUSIVE, cosmic-comp auto-focuses an
// Exclusive Overlay-layer surface on its first commit, the panel's popup gets
// `wl_keyboard.leave`, and cosmic-panel closes it. Confirmed in cosmic-comp
// (`shell/focus/mod.rs`, the `KeyboardInteractivity::Exclusive` arm that calls
// `keyboard.unset_grab` so the following `set_focus` actually lands) and in cosmic-panel
// (`wrapper_space.rs::keyboard_leave` -> `close_popups`).
//
// So the dismissal is CAUSED BY THIS PROCESS, which is why no pre-spawn delay in the
// daemon could ever work: it delayed the very thing it was waiting for. The launch-time
// flats grab just runs too early, finishing before the overlay is up.
//
// The fix is ordering, not duration: hold the grab until our overlay reports keyboard
// focus, then let the panel finish tearing the popup down, then grab. While the hold is
// up the overlay paints NOTHING, which is the same trick DRAGON-456 used for the scan
// re-read: a mapped Linux layer surface that draws nothing composites to nothing, so it
// cannot photograph itself.

/// After our overlay takes keyboard focus, how long the panel is given to actually retire
/// its popup and the compositor to composite a frame without it. This is a settle, not a
/// guess about whether the dismissal will happen: the causal event has already been
/// observed by the time it starts. cosmic-panel closes the popup synchronously on the
/// keyboard leave, so this only has to cover one client round trip plus a frame or two.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // Linux is the only host-owned menu
pub(super) const MENU_DISMISS_SETTLE_MS: u64 = 150;

/// The OUTER bound on the whole hold (DRAGON-118: nothing waits unboundedly). If keyboard
/// focus never arrives, because the compositor refused the Exclusive grab or there are no
/// outputs, the grab runs anyway and the launch proceeds with whatever is on screen. A
/// stale dropdown in a snapshot is a blemish; a capture that never happens is a loss.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) const MENU_HOLD_BUDGET_MS: u64 = 1200;

const _: () = assert!(
    MENU_DISMISS_SETTLE_MS < MENU_HOLD_BUDGET_MS,
    "DRAGON-600: the post-focus settle must fit inside the outer hold budget, or the \
     budget would fire first and the settle could never be observed"
);

/// **Pure**, unit-tested: whether this launch holds its frozen-flats grab for the tray
/// dropdown. Only a menu-launched child has a dropdown on screen, and only a launch that
/// actually grabs flats has anything to protect, so a PrintScreen or hotkey launch pays
/// nothing at all. `menu_launch` is [`crate::recording_ui::MENU_LAUNCH_ENV`]'s presence,
/// `want_flats` is [`super::launch_flats_needed`]'s answer.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn menu_flats_hold_needed(menu_launch: bool, want_flats: bool) -> bool {
    menu_launch && want_flats
}

/// **Pure**, unit-tested: whether a held grab should run NOW.
///
/// `since_focus_ms` is `None` until our overlay has taken keyboard focus, which is the
/// event that dismisses the dropdown; once it is `Some`, the settle is counted from there.
/// `since_launch_ms` is measured from the hold being armed and drives the outer bound, so
/// the two clocks answer different questions: one "has the dismissal had time to land",
/// the other "have we waited long enough that waiting is now the bigger risk".
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn menu_hold_release(since_focus_ms: Option<u64>, since_launch_ms: u64) -> bool {
    since_focus_ms.is_some_and(|ms| ms >= MENU_DISMISS_SETTLE_MS)
        || since_launch_ms >= MENU_HOLD_BUDGET_MS
}

impl App {
    /// The keyboard interactivity to MINT a capture overlay with right now:
    /// Exclusive during picking (see [`picking_phase`]), OnDemand otherwise.
    #[cfg(target_os = "linux")]
    pub(super) fn overlay_pick_exclusive(&self) -> bool {
        picking_phase(self.countdown.is_some(), self.capture_live, self.recording.is_some())
    }

    /// DRAGON-599: this session's whole desktop, in CAPTURE coordinates, as the walls a
    /// keyboard region nudge stops at. The effectful half of [`desktop_bounds_of`]: it reads
    /// `self.outputs` and decides nothing.
    pub(super) fn desktop_bounds(&self) -> Option<(i32, i32, i32, i32)> {
        desktop_bounds_of(self.outputs.iter().map(|o| (o.logical_pos, o.logical_size)))
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

    /// DRAGON-295 (macOS/Windows): run an IMMEDIATE, picker-free capture — the frontmost
    /// window (`ActiveWindow`) or the monitor under the cursor (`ActiveMonitor`) — by
    /// resolving the target's screen rect NOW and driving it straight through the normal
    /// capture pipeline ([`Self::run_capture`]), minting NO overlay. Returns `None` when the
    /// target can't be resolved (no frontmost window, no display under the cursor), so the
    /// caller can fall back to the picker overlay. The pipeline mode was already pinned to
    /// Window / Monitor in `init` so the right worker (window-focus grab vs monitor grab)
    /// runs. Never called on Linux (its capture keys are COSMIC custom shortcuts).
    ///
    /// Linux exception (`lab/flatpak`): when the compositor exposes NONE of the protocols
    /// an immediate capture needs (a sandboxed client, or a compositor that never
    /// implemented them), this returns the honest failure ending (`diag::note_failure` +
    /// [`App::fail_session`]) instead of `None`: the picker overlay is not a fallback in
    /// those sessions (layer shell is hidden by the same filter), and a portal dialog
    /// would defeat the point of a zero-interaction capture. See
    /// [`immediate_target_resolvable`].
    pub(super) fn immediate_capture(
        &mut self,
        imm: ImmediateCapture,
    ) -> Option<Task<cosmic::Action<Msg>>> {
        // lab/flatpak: an immediate capture needs the COMPOSITOR to say which target is
        // active and to hand over its pixels. When this session has no protocol access
        // for that, end through the honest failure path NOW: the native grab below
        // cannot work, and the picker fallback cannot draw either (the same sessions
        // hide layer shell). Keyed on the protocol probe, never on sandbox detection,
        // so a normal COSMIC session (protocols present) is byte-identical.
        #[cfg(target_os = "linux")]
        {
            let (native_capture, window_list) = immediate_protocol_terms();
            if !immediate_target_resolvable(imm, native_capture, window_list) {
                log::warn!(
                    "immediate capture ({imm:?}) is not available in this session: the \
                     compositor exposes no protocol to resolve or grab the active target"
                );
                crate::diag::note_failure(
                    crate::diag::Failure::NoOutputs,
                    &format!(
                        "immediate capture ({imm:?}) has no usable capture target: \
                         native_capture={native_capture} toplevel_list={window_list}"
                    ),
                );
                return Some(self.fail_session());
            }
        }
        // Linux: an overlay-less immediate capture mints NO picker overlay, so — unlike the
        // interactive path — nothing fills `capture_pointer_output` from an overlay's
        // pointer-enter. Learn the cursor's output DIRECTLY from the momentary transparent
        // per-output probe (`platform::compositor::pointer_output`, same wl_pointer-enter
        // signal) and store it in that SAME field, so every shared downstream path (the
        // active-monitor selection, the preview's trigger display, the windowed 80% cap, the
        // re-home) follows the cursor exactly as mac/Windows do with their real global cursor
        // position. Resolved once per session. `None` (probe unavailable / no enter) degrades
        // to the primary-output / single-window fallbacks below.
        #[cfg(target_os = "linux")]
        if self.capture_pointer_output.is_none() {
            self.capture_pointer_output = crate::platform::compositor::pointer_output();
            if self.capture_pointer_output.is_none() {
                log::debug!(
                    "immediate capture: cursor-output probe did not resolve; monitor/window \
                     selection falls back to the primary output / single window"
                );
            }
        }
        let sel = match imm {
            ImmediateCapture::ActiveWindow => {
                // The active window's global rect + stable id feed a window `Selection` (the
                // same shape a picker commit produces). DRAGON-295 fix: prefer the window the
                // DAEMON resolved at hotkey-PRESS time (target still frontmost) and handed off
                // via env, re-reading its current rect; a freshly-booted child resolving
                // frontmost ITSELF runs ~hundreds of ms too late (target already deactivated),
                // which resolved nothing and fell back to the window PICKER. A direct
                // `--active-window` CLI launch with no handoff resolves live.
                #[cfg(target_os = "macos")]
                let win = crate::platform::mac::active_window::immediate_active_window();
                // Windows: the foreground window (GetForegroundWindow + its DWM frame + a
                // stable HWND id) as a `Toplevel`, the analogue of mac's `active_window`, with
                // the same daemon-handoff preference.
                #[cfg(windows)]
                let win = crate::platform::windows::immediate_active_window();
                // Linux (COSMIC): normally the cctk toplevel carrying the compositor's
                // `Activated` state (no daemon handoff needed — a COSMIC custom shortcut
                // launches the capture app directly). But when NOTHING is `Activated` — the
                // user's keyboard focus sits on an empty desktop / a different monitor, so no
                // window holds activation (the confirmed "shows the picker instead of
                // capturing" cause) — `pick_immediate_window` falls back through the SHARED
                // cursor-output signal (a window on the cursor's monitor) then the single
                // existing window, yielding `None` (→ picker) only for a genuinely ambiguous
                // multi-window idle desktop.
                #[cfg(target_os = "linux")]
                let win = {
                    let per_output = crate::platform::compositor::list_toplevels();
                    let flat: Vec<(String, crate::platform::compositor::Toplevel)> = per_output
                        .iter()
                        .flat_map(|(out, tops)| {
                            let out = out.clone();
                            tops.iter().map(move |t| (out.clone(), t.clone()))
                        })
                        .collect();
                    pick_immediate_window(&flat, self.capture_pointer_output.as_deref())
                };
                let win = win?;
                let (x, y, w, h) = win.rect;
                Selection {
                    x,
                    y,
                    width: w.max(0) as u32,
                    height: h.max(0) as u32,
                    output: None,
                    window_id: Some(win.id),
                }
            }
            ImmediateCapture::ActiveMonitor => {
                // The display under the cursor, resolved through ONE shared path on every
                // platform: [`Self::immediate_cursor_monitor`] wraps the per-platform "where
                // is the cursor" seam and hands the SAME `OutputDesc` to the SAME `Selection`
                // construction below. `?` on an empty display list falls the caller back to
                // the picker overlay.
                let descs = crate::screenshot::output_descs();
                let desc = self.immediate_cursor_monitor(&descs)?;
                Selection {
                    x: desc.logical_pos.0,
                    y: desc.logical_pos.1,
                    width: desc.logical_size.0.max(0) as u32,
                    height: desc.logical_size.1.max(0) as u32,
                    output: Some(desc.name),
                    window_id: None,
                }
            }
        };
        Some(self.run_capture(sel))
    }

    /// The [`OutputDesc`](crate::platform::backend::OutputDesc) the CURSOR sits
    /// on, for the picker-free "Capture Active Monitor" — the ONE shared resolver every
    /// platform's [`ImmediateCapture::ActiveMonitor`] arm now flows through. The only
    /// per-platform part is "how do I know where the cursor is":
    /// - **macOS / Windows** feed a REAL global cursor position into the shared
    ///   [`monitor_for_pointer`] (byte-identical to the pre-cursor-probe arms).
    /// - **Linux (COSMIC)** has no global pointer position, so it uses the cursor's OUTPUT
    ///   learned by the momentary transparent probe (cached in `capture_pointer_output`,
    ///   resolved at the top of [`Self::immediate_capture`]), matched by name into the live
    ///   display list; when the probe couldn't resolve it, it defers to the same
    ///   primary-output fallback `monitor_for_pointer(None, …)` yields. This REPLACES the
    ///   old focused-window heuristic (the active toplevel's centre) that captured the
    ///   monitor holding the focused window instead of the one the user's cursor was on.
    fn immediate_cursor_monitor(
        &self,
        descs: &[crate::platform::backend::OutputDesc],
    ) -> Option<crate::platform::backend::OutputDesc> {
        #[cfg(target_os = "macos")]
        {
            monitor_for_pointer(Some(crate::platform::mac::global_pointer_position()), descs)
        }
        #[cfg(windows)]
        {
            monitor_for_pointer(crate::platform::windows::cursor_position(), descs)
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(name) = self.capture_pointer_output.as_deref()
                && let Some(d) = descs.iter().find(|d| d.name == name)
            {
                return Some(d.clone());
            }
            monitor_for_pointer(None, descs)
        }
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
                // A region COMMIT: the session's target was picked at the seed,
                // so replaying the session token avoids a second prompt.
                super::portal::RequestOrigin::InSession,
            );
        }
        // Region screenshot + PipeWire: same portal request; the single-frame grab
        // happens in `do_pixel_capture`. (Freeze is inert under PipeWire.)
        //
        // `lab/flatpak` (`fallback_region_still_from_frozen`): a NON-DELAYED region
        // still on the fallback path skips this request: the seed grant already froze
        // the granted monitor, the user drew over exactly that still, and a fresh live
        // frame could differ from what they selected (WYSIWYG). It falls through to
        // `proceed_capture`, whose `do_pixel_capture` crops the frozen frame. A DELAYED
        // shot keeps this request: the delay exists to change the screen, so the fresh
        // portal frame at fire time is the honest source (the restore token means no
        // second dialog).
        #[cfg(target_os = "linux")]
        if matches!(self.kind, Kind::Image | Kind::Scanner)
            && self.screenshot_uses_portal()
            && self.pipewire_available
            && self.mode == Mode::Region
            && self.pw_held.is_none()
            && !self.fallback_region_still_from_frozen()
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
                // Same commit-time shape as the video arm above: in-session.
                super::portal::RequestOrigin::InSession,
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
        // DRAGON-563: the remaining seconds also render IN the tray icon, on EVERY
        // session (the owner ungated it: "we can have that across the board"). Normal
        // sessions keep their on-screen countdown and get the digits in addition. On the
        // `lab/flatpak` fallback path the digits are the ONLY countdown surface: the
        // plain toplevel counted down over a gray sheet, and window/monitor countdowns
        // had no surface at all, so `recreate_active_overlays` (below) tears the
        // fallback window down for the countdown's whole run (its arm keys on
        // `self.countdown`, set above). There the editor anchor is snapshotted NOW,
        // while the outputs are still known, the same rule as the fallback record-start
        // snapshot; `begin_capture` keeps it when its fresh resolution comes back empty
        // (`keep_countdown_anchor`).
        //
        // No tray host answering: NORMAL sessions keep their historical on-screen
        // countdown (nothing changes for them). The FALLBACK path proceeds with NO
        // visual countdown at all — the reopened DRAGON-563: sandboxed child trays can
        // fail to register where the resident's succeeds, and the "keep the window so
        // the timer is visible" failure-safe put the gray sheet right back. The capture
        // still fires on schedule and Cancel stays possible from any surface that does
        // exist (a resident tray, Escape on a still-open selector); the warn names the
        // condition so the log shows WHY nothing was on screen.
        self.countdown_tray = crate::tray::CountdownTraySession::start(secs);
        if self.overlay_fallback_active() {
            if self.countdown_tray.is_none() {
                log::warn!(
                    "portal-fallback countdown: no tray host answered, proceeding with no \
                     visual countdown ({secs}s still fires on schedule)"
                );
            }
            self.snapshot_preview_anchor(&sel);
        }
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
    ///
    /// **A capture excludes our overlay by TIMING, not by any filter** (DRAGON-608), and the
    /// distinction matters more than it looks. Nothing marks our surfaces as un-capturable;
    /// what keeps them out of the shot is only that we have already left the screen by the
    /// time it is read. That exclusion therefore reaches exactly ONE process, our own.
    ///
    /// Two consequences, and both are relied on:
    ///
    /// * A capture started over a live COLOUR PICKER photographs the picker, correctly and
    ///   with no feature of its own, because the picker is a separate process whose overlay
    ///   nothing here tore down. That is the whole of DRAGON-608's delivered behaviour.
    /// * The teardown DISTURBS whatever is underneath. The compositor hands the pointer to the
    ///   newly topmost surface as a `wl_pointer.enter`, and a live picker there used to read
    ///   that as a real move and jump its loupe into the shot (DRAGON-611). The rule that
    ///   separates a revealed pointer from a moved one is
    ///   [`crate::widgets::color_pick`]'s `pointer_report_moves_sample`.
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
        // DRAGON-309: the post-capture preview opens on the TRIGGER display (the monitor
        // active when the capture was initiated), NOT the selection's monitor — so picking a
        // target on another display still lands the preview back where the user started. Fall
        // back to the selection's output when the trigger can't be resolved (keeps the
        // DRAGON-304 immediate-capture behavior). Captured before the overlay (and
        // `self.outputs`) tears down, so the fullscreen preview overlay can open there.
        //
        // `lab/flatpak` (DRAGON-563): on the fallback path a TRAY countdown tore the
        // selection window and `self.outputs` down at COUNTDOWN start, so the fresh
        // resolutions here come back empty at fire time; the countdown-start snapshot
        // (`snapshot_preview_anchor`, holding both the anchor and its scale) stands in,
        // the same precedence `stop_recording` applies at record stop. Every other path
        // has no snapshot to keep, so the historical overwrite below is untouched.
        let fresh = self.active_trigger_display().or_else(|| self.output_for_selection(&sel));
        let keep_snapshot = keep_countdown_anchor(
            self.overlay_fallback_active(),
            fresh.is_some(),
            self.preview_output.is_some(),
        );
        if !keep_snapshot {
            self.preview_output = fresh;
        }
        // DRAGON-317 regression fix: the windowed-preview re-home target is the RELIABLE
        // capture-origin monitor ONLY — the pointer's output learned from the capture
        // overlay's first pointer-enter (`capture_pointer_output`), NOT the launch
        // focused-toplevel guess. When that reliable signal is absent (no overlay was
        // entered), we leave `preview_output_name` None so NO move fires and cosmic-comp's
        // native placement (the fresh window maps on the pointer's own output — already where
        // the user is) stands, instead of dragging the preview onto the focused window's
        // monitor. Cached NOW, before `destroy_surfaces` (below) clears `self.outputs`.
        #[cfg(target_os = "linux")]
        {
            self.preview_output_name = self.capture_pointer_output.clone();
        }
        if !keep_snapshot {
            self.preview_output_scale = self.scale_for_selection(&sel);
        }
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
        // DRAGON-305 (Windows): mirror the mac pre-open — a WINDOWED window pick covers the grab
        // with the fullscreen blocker (forced overlay via `win_preview_preopen`), placed
        // non-activating so the target's active grab is undisturbed.
        #[cfg(windows)]
        {
            self.win_preview_preopen = mac_preopen;
        }
        // DRAGON-428: `--no-editor` suppresses this pre-open too. `neutral_spinner` and
        // `mac_preopen` already carry the term inside their own decisions; this third one is
        // computed inline, so it takes the term here. All three exist only to cover the gap
        // until the editor appears, and this launch opens none.
        let preview_pre_open = immediate
            && !self.no_editor
            && (sel.window_id.is_none() || self.window_defocus_uses_spinner(&sel));
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
        if let Some(o) = self.output_for_selection_state(sel) {
            return Some((o.output.clone(), o.logical_size));
        }
        // DRAGON-304: a picker-free IMMEDIATE capture (`--active-window` / `--active-monitor`)
        // returns before any overlay is minted, so `self.outputs` is EMPTY and the state
        // lookup above finds nothing — which left `preview_output` `None`, so the post-capture
        // preview never opened (present_capture fell through to a silent share): the reported
        // "monitor does nothing" bug. Resolve the output from a LIVE display query instead. Off
        // Linux the `OutputHandle` IS the output name, so this hands the preview a real monitor
        // to open on. Linux never takes the immediate path and keeps its WlOutput-handle
        // behaviour byte-identical (this fallback is cfg'd out there).
        #[cfg(not(target_os = "linux"))]
        {
            output_desc_for_selection(sel).map(|d| {
                (
                    d.name,
                    (d.logical_size.0.max(0) as u32, d.logical_size.1.max(0) as u32),
                )
            })
        }
        #[cfg(target_os = "linux")]
        None
    }

    /// The backing scale (physical / logical) of the CAPTURE SOURCE output a `sel` sits on —
    /// cached into `preview_output_scale` and used to divide the captured media's PHYSICAL
    /// pixels down to the LOGICAL points it occupied on screen so the windowed preview opens
    /// at true on-screen size (DRAGON-221). This is the SELECTION's monitor, NOT the trigger
    /// display the preview opens on (DRAGON-309): the media is physical pixels of the display
    /// it was grabbed from, so only that display's scale undoes its Retina/DPI factor. `1.0`
    /// on 100%/1× outputs — COSMIC fractional scale, Windows per-monitor DPI (DRAGON-131), and
    /// mac backing scale each resolve their own display's factor.
    pub(super) fn scale_for_selection(&self, sel: &Selection) -> f32 {
        #[cfg(target_os = "linux")]
        {
            self.output_for_selection_state(sel).map(|o| o.scale).unwrap_or(1.0)
        }
        // macOS: read the SOURCE display's live NSScreen backing scale by name.
        // `output_for_selection` resolves the selection's monitor even when `self.outputs` is
        // empty (the immediate-capture path, DRAGON-304), so a window / monitor grab on a
        // Retina display divides correctly regardless of which display triggered the capture.
        #[cfg(target_os = "macos")]
        {
            self.output_for_selection(sel)
                .as_ref()
                .and_then(|(name, _)| crate::platform::mac::scale_for(name))
                .map(|s| s as f32)
                .filter(|s| *s > 0.0)
                .unwrap_or(1.0)
        }
        // Windows (DRAGON-131): read the SELECTION monitor's per-monitor DPI scale
        // (`GetDpiForMonitor` → `dpi / 96`) by name. Under Per-Monitor-Aware-V2 the capture
        // is PHYSICAL pixels; dividing by this recovers the logical points the grab occupied,
        // so the windowed preview opens at true on-screen size on a 150%/200% display, not
        // dpi× too large. `output_for_selection` resolves the monitor even when `self.outputs`
        // is empty (the immediate-capture path), matching macOS. `1.0` on 100% monitors, so
        // every field stays byte-identical there.
        #[cfg(target_os = "windows")]
        {
            self.output_for_selection(sel)
                .as_ref()
                .and_then(|(name, _)| crate::platform::windows::scale_for(name))
                .filter(|s| *s > 0.0)
                .unwrap_or(1.0)
        }
        #[cfg(all(not(target_os = "linux"), not(target_os = "macos"), not(target_os = "windows")))]
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
        // DRAGON-549: the LAST resort used to be `outputs.first()`, i.e. wl_output
        // registration order. It is reached by a portal `--window` launch, whose selection is
        // a 1x1 placeholder that names no output and lands wherever the origin happens to be,
        // and its answer feeds `scale_for_selection` — the divisor that turns the capture's
        // physical pixels back into points. Prefer the output the PORTAL GRANT was made on,
        // which is the display the media really came from. Untouched (and `None`) on every
        // native-capture launch, where a real selection resolves above this.
        let portal_origin = || {
            #[cfg(target_os = "linux")]
            {
                let name = self.portal_origin_output.as_deref()?;
                self.outputs.iter().find(|o| o.name == name)
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        };
        self.outputs
            .iter()
            .find(|o| {
                let (lx, ly) = o.logical_pos;
                let (lw, lh) = o.logical_size;
                cx >= lx && cx < lx + lw as i32 && cy >= ly && cy < ly + lh as i32
            })
            .or_else(portal_origin)
            .or_else(|| self.outputs.first())
    }

    /// The TRIGGER display (DRAGON-309): the monitor that was ACTIVE when this capture was
    /// INITIATED, as `(OutputHandle, logical_size)` — where the post-capture preview should
    /// open, REGARDLESS of where the picked target (region / window / monitor) lands. Distinct
    /// from [`Self::output_for_selection`], which follows the SELECTION and so opens the
    /// preview on the wrong monitor when the user picks a target on a different display.
    ///
    /// The trigger display's NAME was SNAPSHOTTED at launch into [`Self::trigger_display`]
    /// (`platform::snapshot_trigger_display_name`, before any picker UI / cursor move / focus
    /// steal). Here we only RESOLVE that stored name to a rect — never re-sample the live
    /// cursor / focus (which by commit time point at the TARGET, not the trigger):
    /// - **Off Linux**: size the named display from the live `output_descs()` list. A
    ///   stale/unknown name still opens — the placement seams re-resolve the rect by name and
    ///   fall back to the pointer.
    /// - **Linux (COSMIC)**: match the stored name into `self.outputs` for its `WlOutput`
    ///   handle + logical size; fall back to the primary (first) output when the name is
    ///   unknown or none was captured.
    ///
    /// `None` when nothing resolves — the caller then falls back to
    /// [`Self::output_for_selection`], keeping the DRAGON-304 immediate-capture behavior.
    pub(super) fn active_trigger_display(&self) -> Option<(OutputHandle, (u32, u32))> {
        #[cfg(not(target_os = "linux"))]
        {
            let name = self.trigger_display.clone()?;
            // Size the display from the live list; a stale/unknown name still opens (the
            // placement seams re-resolve the rect by name and fall back to the pointer).
            let size = crate::screenshot::output_descs()
                .into_iter()
                .find(|d| d.name == name)
                .map(|d| (d.logical_size.0.max(0) as u32, d.logical_size.1.max(0) as u32))
                .unwrap_or((0, 0));
            Some((name, size))
        }
        #[cfg(target_os = "linux")]
        {
            // DRAGON-317 regression fix: prefer the pointer's output learned from the capture
            // overlay's FIRST pointer-enter — the monitor the user was actually on when the
            // picker appeared. This is the RELIABLE origin signal cosmic-comp itself uses to
            // place windows (it maps our overlay under the cursor, so the wl_pointer enter
            // names the pointer's output). The launch-snapshotted FOCUSED-toplevel output is a
            // strictly worse guess — it points at the focused window's monitor even when the
            // user is working on a different, empty one (the reported regression) — so it is
            // only the fallback now.
            if let Some(name) = self.capture_pointer_output.as_deref()
                && let Some(o) = self.outputs.iter().find(|o| o.name == name)
            {
                return Some((o.output.clone(), o.logical_size));
            }
            // Fallback: the launch focused-toplevel output name, matched into `self.outputs`
            // (empty at init, populated by commit) for its WlOutput handle + logical size.
            if let Some(name) = self.trigger_display.as_deref()
                && let Some(o) = self.outputs.iter().find(|o| o.name == name)
            {
                return Some((o.output.clone(), o.logical_size));
            }
            // DRAGON-549: the output a PORTAL grant was made on. A portal session has no
            // capture overlay, so neither signal above can exist there — the pointer output is
            // learned from an overlay that was never minted, and the focused-toplevel name
            // needs a protocol a sandboxed client cannot see. The grant is what knows, and it
            // has to be consulted BEFORE the fallback below, which is wl_output registration
            // order and names an arbitrary display. On the owner's box that arbitrary display
            // is an 800x480 panel beside a 5120x1440 ultrawide, and bounding the windowed
            // editor's open fit to it floors every capture at the same small window. `None` on
            // every native-capture launch, where the two signals above already answered.
            if let Some(name) = self.portal_origin_output.as_deref()
                && let Some(o) = self.outputs.iter().find(|o| o.name == name)
            {
                return Some((o.output.clone(), o.logical_size));
            }
            // No captured / resolvable trigger output: the primary (first) output.
            self.outputs.first().map(|o| (o.output.clone(), o.logical_size))
        }
    }

    /// Which capture this was, for the notification's first line (DRAGON-450). The picker
    /// mode IS the answer — an immediate `--active-window` / `--active-monitor` launch pins
    /// it at startup, and an overlay capture follows whatever the user last switched to.
    fn notify_kind(&self) -> crate::platform::services::NotifyKind {
        use crate::platform::services::NotifyKind;
        match self.mode {
            Mode::Region => NotifyKind::Region,
            Mode::Window => NotifyKind::Window,
            Mode::Monitor => NotifyKind::Monitor,
        }
    }

    /// Put the finished capture on the clipboard and report WHAT ACTUALLY HAPPENED.
    ///
    /// Two things this deliberately does not do (DRAGON-450, both regressions of the old
    /// two-line version):
    ///
    /// * It does not gate a REFERENCE copy on size. A recording — or any non-image path —
    ///   goes onto the clipboard as a path (`CF_HDROP` / a `file://` URL / `text/uri-list`),
    ///   a few hundred bytes however long the recording is. The old size check refused
    ///   exactly those, which is the one case it could never help; it now applies only to a
    ///   still image, whose bytes really are copied (see [`crate::share::copy_embeds_bytes`]).
    /// * It does not PREDICT the result. The old code compared the size, copied, threw the
    ///   copy's own return value away, and told the user "Copied to clipboard" whether or
    ///   not the write had landed.
    ///
    /// What the returned outcome is worth differs by platform, exactly as
    /// [`crate::share::copy_to_clipboard`] documents: on macOS/Windows the write is
    /// synchronous, so `Copied` is verified. On Linux the selection is served by a detached
    /// worker, so `Failed` is real (no worker started at all) while `Copied` means "handed
    /// to a worker that normally succeeds" — the furthest a one-shot process can honestly go
    /// there, and no weaker than what this path used to claim unconditionally.
    fn copy_for_delivery(
        &self,
        path: &std::path::Path,
        size: u64,
        is_video: bool,
    ) -> crate::platform::services::CopyOutcome {
        use crate::platform::services::{AUTO_COPY_MAX_BYTES, CopyOutcome};
        if crate::platform::services::copy_embeds_bytes(path, is_video) && size > AUTO_COPY_MAX_BYTES
        {
            log::warn!(
                "capture ({size} bytes) is over the automatic clipboard limit for an image \
                 copy; saved to {} but not copied",
                path.display()
            );
            return CopyOutcome::TooLarge;
        }
        if crate::platform::services::copy_to_clipboard(path, is_video) {
            CopyOutcome::Copied
        } else {
            CopyOutcome::Failed
        }
    }

    /// The user's CONFIGURED save folder for this media kind, tilde-expanded, with the blank
    /// setting falling back to [`DEFAULT_CAPTURE_DIR`].
    ///
    /// ONE reader for both the capture write and the editor's Save prefill, because
    /// DRAGON-467 made them two different questions with the same answer: the setting names
    /// where captures BELONG, whether or not this particular capture was written there.
    pub(in crate::app) fn capture_save_dir(&self, is_video: bool) -> std::path::PathBuf {
        let raw = if is_video { self.record_dir.trim() } else { self.screenshot_dir.trim() };
        let raw = if raw.is_empty() { DEFAULT_CAPTURE_DIR } else { raw };
        crate::util::expand_tilde(raw)
    }

    /// The folder this capture is written to, for a message shown to the USER (DRAGON-467
    /// review, minor 9). The same reader the write itself uses, minus the directory creation,
    /// so a save-failure alert can never name a folder the save did not touch.
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    pub(in crate::app) fn capture_write_dir_display(&self, is_video: bool) -> String {
        let save_originals = if is_video {
            self.preview_video_save_originals
        } else {
            self.preview_save_originals
        };
        capture_write_dir(save_originals, &self.capture_save_dir(is_video), &transient_dir(is_video))
            .display()
            .to_string()
    }

    /// WHERE this capture's file is written NOW: the configured folder, or the transient
    /// location for this medium when "Automatically save originals" is off
    /// ([`capture_write_dir`], DRAGON-467). Creates the directory, as the write sites always
    /// did.
    pub(in crate::app) fn capture_write_dir(&self, is_video: bool) -> std::path::PathBuf {
        let save_originals = if is_video {
            self.preview_video_save_originals
        } else {
            self.preview_save_originals
        };
        let dir = capture_write_dir(
            save_originals,
            &self.capture_save_dir(is_video),
            &transient_dir(is_video),
        );
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Finish a capture WITHOUT the preview editor: it's already saved. Copy it to the
    /// clipboard, then post a notification naming what was captured and where it went —
    /// "Region copied to clipboard", or "Region saved" with the reason when the copy did
    /// not happen. Clicking the notification reveals the file. The clipboard + notifier run
    /// as detached helper processes, so we just exit.
    ///
    /// This is the whole feedback a "(no editor)" capture gives (DRAGON-428), which is why
    /// DRAGON-450 made the text specific and the click work.
    ///
    /// DRAGON-353: the copy is UNCONDITIONAL — the "Automatically copy to clipboard"
    /// setting is gone, and the editor's own open-time auto-copy is this path's mirror for
    /// the (normal) case where an editor does open.
    pub(super) fn finish_share(
        &mut self,
        path: &std::path::Path,
        size: u64,
        is_video: bool,
    ) -> Task<cosmic::Action<Msg>> {
        let outcome = self.copy_for_delivery(path, size, is_video);
        crate::platform::services::notify(path, self.notify_kind(), outcome);
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
    /// runs. A no-op whenever the flats already landed or this capture wouldn't freeze.
    ///
    /// macOS-ONLY, and not because the other platforms grab synchronously. This said "a no-op
    /// on Linux (flats grabbed synchronously; `frozen_pending` never set)", which DRAGON-212
    /// made false: Linux and Windows defer the grab exactly like macOS does, they simply have
    /// no commit-race guard, so committing before the grab lands silently degrades freeze to a
    /// live grab there. That is a real asymmetry rather than a documented absence.
    ///
    /// Note for DRAGON-606: this is the ONE other place `frozen_pending` is cleared, and it
    /// does not arm the dim's fade. `overlay::dim_now`'s `Waiting` arm covers that; its
    /// comment carries the reasoning.
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

    /// Linux and Windows: no commit-race guard, so this is a no-op that keeps
    /// `begin_capture` platform-agnostic.
    ///
    /// The old wording here, "the flats are grabbed synchronously in `init`, so there's never
    /// a deferred grab to wait on", has been wrong since DRAGON-212 deferred the grab on these
    /// platforms too. There IS a deferred grab to wait on; nothing waits on it. Left as a
    /// no-op rather than quietly given macOS's 750ms poll, because adding a blocking wait to
    /// two platforms' commit path is a change that should be made deliberately and measured,
    /// not smuggled in as a doc fix.
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
            self.window_defocus_uses_spinner(sel),
            !self.no_editor,
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
        window_pick_preopen_decision(immediate, sel.window_id.is_some(), !self.no_editor)
    }

    /// DRAGON-305 (Windows): a WINDOWED single-window capture pre-opens its fullscreen loading
    /// BLOCKER (spinner + cancel X) to cover the whole grab + compose/save, then swaps it for the
    /// real preview window when the composed dims land — the macOS windowed flow. Gated on
    /// `preview_windowed`: overlay-preview mode already shows the fullscreen blocker after the
    /// grab, so it needs no pre-open. The cover is placed NON-ACTIVATING (see `win_preview_preopen`),
    /// so — like macOS's order-front-non-key — it never disturbs the target's foreground/active
    /// chrome the grab depends on.
    #[cfg(windows)]
    fn window_pick_preopens_window(&self, sel: &Selection, immediate: bool) -> bool {
        windows_window_pick_preopens(
            immediate,
            sel.window_id.is_some(),
            self.preview_windowed,
            !self.no_editor,
        )
    }

    /// Off macOS/Windows the window pre-open is Linux's focus-neutral overlay (handled above) or
    /// nothing; this pre-open never fires.
    #[cfg(not(any(target_os = "macos", windows)))]
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
        // The pre-open belongs to the IN-FLIGHT capture, so that document is the one to
        // resolve (DRAGON-336 phase 2).
        let Some(id) = self.capture_preview else {
            return Task::none();
        };
        if self.preview_for(id).is_none_or(|p| p.surface.is_window()) {
            return Task::none();
        }
        // NO OVERLAY ONCE A SECOND DOCUMENT EXISTS (DRAGON-336): the pre-opened cover is
        // minted FOCUS-NEUTRAL (`KeyboardInteractivity::None`), so it can coexist with
        // another document's surface — but promoting it to `Exclusive` below would leave a
        // fullscreen overlay behind that document's window (and, if that one were an
        // overlay too, two exclusive layer surfaces fighting over the keyboard grab — the
        // DRAGON-109 hazard). Whenever another document is open, resolve the spinner the
        // WINDOWED way instead: `preview_surface_for` mints a window for it and demotes any
        // sibling still on the overlay, exactly as it would for a fresh preview opened
        // alongside one. Inert with a single preview open, so the historical path is
        // unchanged.
        if self.preview_windowed || self.overlay_barred(Some(id)) {
            // DEFER the swap to `present_capture` (DRAGON-221 follow-up): the composed
            // image's dims aren't known until ShotSaved (padding/shadow/wallpaper
            // margins grow it past the selection), and a post-open `window::resize` is
            // not honored on COSMIC — so the cover stays up through compose/save and
            // the window opens ONCE at its correct size.
            self.windowed_swap_pending = true;
            Task::none()
        } else {
            super::shell::promote_preview_surface(id)
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
        //
        // `lab/flatpak`: the portal-frozen FALLBACK forces the freeze term. There the
        // active backend is the portal, whose `freeze` capability is honestly false
        // (finished frames, nothing to reconstruct), but the fallback seeding grabbed
        // a real frame of the granted monitor at launch, the selection window draws
        // over that still, and delivering anything OTHER than a crop of it would hand
        // the user different pixels than the ones they drew on. Freeze is structural on
        // that path, not a preference; a normal session never sets the term.
        self.mode == Mode::Region
            && (self.effective_capture_extras().freeze || self.overlay_fallback_active())
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
    /// DRAGON-460 dropped the scanner's own arm here: it reads a live region shot now, so
    /// it needs no frozen view to agree with. The freeze PREFERENCE still applies to it
    /// exactly as it does to any other kind.
    ///
    /// `lab/flatpak` (DRAGON-547 reopened): the countdown release above is the NATIVE
    /// session's rule only. The fallback term stays frozen under a configured delay;
    /// the pure [`freeze_backdrop_active`] states the by-path split and its why.
    pub(super) fn freeze_backdrop_active(&self) -> bool {
        // `lab/flatpak`: the fallback overlay shows the seed still for the region
        // SELECTION phase of EVERY kind, video included ([`fallback_backdrop`]), not
        // just the still kinds `freezing()` speaks for. The delay release applies
        // ONLY to the native `freezing()` term: a plain toplevel has no live desktop
        // to release to, so the pure fn keeps the fallback term frozen under a delay.
        freeze_backdrop_active(
            self.freezing(),
            fallback_backdrop(
                self.overlay_fallback_active(),
                self.mode == Mode::Region,
                !self.frozen.is_empty(),
            ),
            self.configured_delay_secs(),
        )
    }

    /// `lab/flatpak`: whether a REGION STILL commit on the fallback path delivers by
    /// cropping the seed-frozen frame instead of requesting a fresh portal frame. The
    /// decision itself is pure ([`fallback_still_from_frozen`]); this feeds it the live
    /// state. Linux-only: its one caller is `run_capture`'s Linux portal branch.
    #[cfg(target_os = "linux")]
    fn fallback_region_still_from_frozen(&self) -> bool {
        fallback_still_from_frozen(
            self.overlay_fallback_active(),
            self.configured_delay_secs(),
            !self.frozen.is_empty(),
        )
    }

    /// `lab/flatpak`: whether the SCANNER's region read comes from the seed-frozen frame
    /// rather than a live native grab. Same rule as
    /// [`Self::fallback_region_still_from_frozen`] with the delay term fixed at `0`,
    /// because the scanner never delays (`capture_live` excludes `Kind::Scanner`), so its
    /// WYSIWYG case always applies. False off Linux, where the live read is the only
    /// source and always works.
    pub(super) fn scan_reads_frozen(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            fallback_still_from_frozen(self.overlay_fallback_active(), 0, !self.frozen.is_empty())
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
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

    /// Per-output geometry for the WINDOW composite's `frozen_geom`, i.e. the ONE place the
    /// launch flats are read for their GEOMETRY rather than their PIXELS.
    ///
    /// The window composite runs POST-TEARDOWN (`destroy_surfaces` has already cleared
    /// `self.outputs`), so it has always sourced the geometry from the launch snapshot. That
    /// was safe only while the snapshot was unconditional: DRAGON-336's `launch_flats_needed`
    /// now SKIPS the flats grab entirely unless freeze is on or it's a scanner launch, which
    /// left `frozen_geom` empty on a freeze-off window capture and dropped the wallpaper
    /// backing in `composite_over_wallpaper` (which needs an output rect to place the
    /// wallpaper under the window). So: prefer the launch snapshot when it exists — the
    /// launch-instant geometry, byte-identical to before — and fall back to a fresh LIVE
    /// query (`crate::screenshot::output_descs()`, the same post-teardown-safe source
    /// `output_rect_for_window` and the window-composite diagnostic already use) only when
    /// it is empty.
    fn window_composite_geom(&self) -> Vec<crate::screenshot::OutputGeom> {
        let frozen: Vec<crate::screenshot::OutputGeom> = self
            .frozen
            .iter()
            .map(|(n, f)| (n.clone(), f.logical_pos, f.logical_size))
            .collect();
        pick_composite_geom(frozen, || {
            crate::screenshot::output_descs()
                .into_iter()
                .map(|o| (o.name, o.logical_pos, o.logical_size))
                .collect()
        })
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

    /// DRAGON-336: drop the launch-instant capture scene — the largest resident buffers
    /// this process owns (one full-resolution RGBA flat PER OUTPUT, ~30 MB on a
    /// 5120x1440 monitor, plus the per-window pixel maps and the picker wallpapers).
    /// Nothing ever released them, so they stayed resident through the whole preview
    /// phase even though the picture is already composed and written to disk.
    ///
    /// Called ONLY from [`Self::do_pixel_capture`], after that path's last read of every
    /// buffer, which is what makes it provably safe:
    ///
    /// - The commit is one-shot: `do_pixel_capture` opens by `take`-ing `self.capturing`,
    ///   so its backstop tick (and any later fire) returns before touching the scene, and
    ///   nothing re-arms `capturing` afterwards — `begin_capture` is the only writer and
    ///   is reachable only from the pre-commit `begin`/countdown path.
    /// - A COUNTDOWN is entirely upstream: `enter_countdown` holds the selection in
    ///   `pending` and can still be cancelled back to region select (`CancelCapture` /
    ///   Escape → `restore_interactive_overlays`), all of which happens BEFORE
    ///   `begin_capture`, let alone this. By the time we get here the overlays are torn
    ///   down and there is no path back to a selector.
    /// - The window commit has already CLONED what it needs (`fallback_px` out of
    ///   `active_win_px`/`frozen_win_px`, `frozen_geom` out of `frozen`) into the
    ///   `WindowCaptureJob` moved onto the capture worker, so the worker keeps its own
    ///   pixels regardless of what the App drops.
    /// - The region/monitor commit is synchronous: the composite (`frozen_captured`,
    ///   `crop_frozen`, `frozen_out_rects`, `frozen_scale_at`) is finished and the PNG is
    ///   on disk before this runs. Everything downstream (`present_capture`, the preview
    ///   editor, the share/finish paths) reads the SAVED FILE, never the scene.
    ///
    /// `frozen_toplevels` (geometry/z-order only, no pixels) and `frozen_cursor` (a
    /// pointer sprite) are deliberately kept: they are tiny, and `window_focus_grab` /
    /// `window_defocus_uses_spinner` still consult the toplevel list on this same path.
    /// NOT called on the recording path — a recording keeps the overlay alive (stop
    /// button, meters) and its release point is a separate question (see the ticket).
    fn release_capture_scene(&mut self) {
        // `clear` drops the pixel buffers (the ~30 MB allocations go straight back via
        // munmap) while keeping the tables themselves, so every reader keeps its
        // "empty map" semantics — `freezing()` already treats an empty `frozen` as
        // "no freeze", which is the same state a launch that skipped the grab is in.
        self.frozen.clear();
        self.frozen_win_px.clear();
        self.active_win_px.clear();
        self.wallpaper_handles.clear();
    }

    /// Filename stem `<timestamp>[-<descriptor>]` for a capture/recording of `sel`:
    /// a window appends its slugified title (or the literal `window`), a monitor
    /// its slugified name (or `monitor`), a region nothing extra.
    ///
    /// Deliberately EXTENSIONLESS — it is a stem, and every auto-save path turns it into a
    /// file name through [`still_save_name`] / [`recording_save_name`].
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

    /// DRAGON-562: build the portal window still's decoration bundle ON THE UI
    /// THREAD (settings, theme and border resolution all live here), so the
    /// worker only pays for the compose. Mirrors the native window branch's job
    /// wiring field for field, minus what the portal cannot supply: no live grab
    /// id (`frozen_px` is always the portal frame), no cursor sprite, and
    /// transparency off pending the alpha probe (both via the effective extras,
    /// which fold the portal caps table in).
    #[cfg(target_os = "linux")]
    fn portal_window_deco(&self, grant: super::PortalWindowGrant) -> PortalWindowDeco {
        let extras = self.effective_capture_extras();
        let borders = crate::decoration::WindowBorders::resolve(
            self.active_border_color,
            self.active_border_width,
            self.inactive_border_color,
            self.inactive_border_width,
        );
        let job = crate::screenshot::WindowCaptureJob {
            // Never grabbed live: `apply` always sets `frozen_px` (the frame).
            id: String::new(),
            cursor: false,
            // Finalized in `apply` from the grabbed frame's dimensions; the
            // grant's position seeds x/y there too.
            sel: Selection { x: 0, y: 0, width: 1, height: 1, output: None, window_id: None },
            capture_transparency: extras.transparency,
            capture_wallpaper: extras.wallpaper,
            window_radius: self.window_radius,
            // ALWAYS the Active border on the portal path, whatever the "Window
            // focus appearance" setting says (`single_window_border_active`'s
            // portal arm). The grant carries no activation info and the portal
            // cannot drive the window's real focus state, so the state-keyed
            // choice is unanswerable here; the portal-picked window was
            // interactively chosen by the user, so Active is the honest
            // deterministic default. The titlebar stays whatever the compositor
            // rendered.
            border: borders.for_active(single_window_border_active(
                true,
                self.window_single_active,
            )),
            window_shadow: self.window_drop_shadow,
            pad_logical: if self.window_padding {
                self.window_padding_px.value as f32
            } else {
                0.0
            },
            dark: super::theme_is_dark(),
            // The grant-time output snapshot: `self.outputs` is torn down by now.
            frozen_geom: grant.outputs.clone(),
            frozen_px: None,
            // No sprite session on the portal (embedded-or-hidden only).
            cursor_overlay: None,
        };
        PortalWindowDeco {
            fullscreen_aware: extras.fullscreen_aware,
            grant,
            job,
            recompositing: self.window_recompositing,
        }
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
        // other overlays so only this shot proceeds. DRAGON-322: a recording / preview
        // sibling is spared, so a still capture can run alongside a recording and can be
        // taken of the open preview.
        crate::instance::close_other_instances();

        // Destination path (shared by both the screencopy and PipeWire paths). DRAGON-467:
        // the FOLDER now depends on "Automatically save originals" — the configured one, or
        // the session runtime directory when the user does not want untouched captures
        // littering it. The NAME is unchanged either way.
        let dir = self.capture_write_dir(false);
        let path = dir.join(still_save_name(&self.capture_stem(&sel)));

        // PipeWire screenshot: a portal stream was granted at commit. Grab a single
        // frame off the UI thread, save it, then share via `PipewireShotSaved`.
        // Linux-only (the portal/PipeWire path); on macOS `pw_held` is always None.
        #[cfg(target_os = "linux")]
        if let Some(held) = self.pw_held.take() {
            let HeldStream { fd, node_id, crop, window_grant } = held;
            let fallback = path.clone();
            let meta = self.screenshot_metadata();
            // DRAGON-562: a WINDOW-mode portal still runs the SAME single-window
            // aesthetics pipeline a native capture does (padding / borders /
            // shadow / rounding / wallpaper-or-black backdrop), built here on the
            // UI thread and applied on the worker. `None` — the frame stays
            // untouched, the historical behavior — for monitor/region grants
            // (no single owning window), scanner shots (analysis wants raw
            // pixels), and any launch that held no window grant.
            let deco = if self.kind == Kind::Image {
                window_grant.map(|g| self.portal_window_deco(g))
            } else {
                None
            };
            // Grab + save on a dedicated OS thread (the PipeWire loop blocks); hand
            // the result back through a oneshot the Task awaits (executor-agnostic).
            let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                // DRAGON-415: keep the two failures apart — a portal frame that never
                // arrived and a frame that could not be written are different problems.
                let outcome = match crate::platform::pipewire::grab_frame(fd, node_id, crop) {
                    None => super::failure::ShotOutcome::NoImage,
                    Some(img) => {
                        // DRAGON-562: decorate a window grant like a native
                        // single-window capture; a truly-fullscreen window (and
                        // every non-window grant) keeps the bare frame.
                        let img = match deco {
                            Some(d) => d.apply(img),
                            None => img,
                        };
                        if crate::media::png::save_png(&img, &path, &meta) {
                            super::failure::ShotOutcome::Saved
                        } else {
                            super::failure::ShotOutcome::SaveFailed
                        }
                    }
                };
                let _ = tx.send((path, outcome));
            });
            // DRAGON-336: the portal path grabs its frame from the held stream and never
            // reads the launch scene at all, so it is dead the moment this branch is taken.
            self.release_capture_scene();
            return Task::perform(
                // DRAGON-419: `Err` here means the portal grab/save thread PANICKED — see the
                // window-mode seam below for why that must not be laundered into "failed".
                // DRAGON-415 carries that as its own `ShotOutcome` rather than as the generic
                // failure, so the downstream seam does not have to guess which of the three
                // things went wrong.
                async move {
                    rx.await.unwrap_or_else(|_| {
                        crate::diag::note_failure(
                            crate::diag::Failure::WorkerPanic,
                            "portal grab/save worker died before reporting (panicked)",
                        );
                        (fallback, super::failure::ShotOutcome::WorkerDied)
                    })
                },
                |(path, outcome)| {
                    cosmic::Action::App(Msg::Capture(CaptureMsg::ShotSaved(path, outcome)))
                },
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
                .is_some_and(|out| is_fullscreen((sel.x, sel.y, sel.width as i32, sel.height as i32), out, FULLSCREEN_TOL))
                || sel
                    .window_id
                    .as_deref()
                    .is_some_and(crate::screenshot::window_is_fullscreen));

        // The ONE bare-frame gate for this window capture's aesthetics (native and
        // portal both consult `window_recomposite`): the master "Enable recompositing
        // of window screenshots" toggle ANDed with the fullscreen owner rule. False
        // zeroes every aesthetic knob below, exactly the treatment a fullscreen
        // window always got, so master OFF delivers the bare frame.
        let recomposite = window_recomposite(self.window_recompositing, fullscreen_window);
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
        // A bare-frame capture (fullscreen window, or the recompositing master off):
        // strip both borders and the shadow so nothing is drawn over its pixels.
        let borders = if !recomposite {
            crate::decoration::WindowBorders {
                active: crate::decoration::BorderSpec { width: 0, ..borders.active },
                inactive: crate::decoration::BorderSpec { width: 0, ..borders.inactive },
            }
        } else {
            borders
        };
        let window_shadow = self.window_drop_shadow && recomposite;
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
            // DRAGON-292 (EXPERIMENT, opt-in behind CCK_MAC_WALLPAPER_BACKDROP=1): a macOS
            // single-window capture must run the LIVE wallpaper-backdrop sequence
            // (`WindowCaptureJob::run`'s experiment arm), which is reached ONLY when
            // `frozen_px` is None. The worker sets `frozen_px = active.or(fallback_px)`, so we
            // must suppress BOTH the off-thread focus grab AND the cached/frozen fallback
            // pixels here — otherwise `frozen_px` is `Some(...)` and the experiment never runs.
            // The experiment sequence raises (focuses) the target itself (step 2), so dropping
            // the separate focus grab loses nothing. Env unset -> both keep their normal values
            // and the default path is byte-identical.
            let backdrop_experiment = wallpaper_backdrop_experiment_active();
            // DRAGON-308 (Windows): float an opaque backdrop below a GLASS window during the
            // grab so its acrylic composites against the wallpaper (not the user's other
            // windows). `None` on mac/Linux (the Windows-only helper is cfg'd out), where the
            // focus grab already isolates the window's own alpha; consumed by the Windows arm.
            #[cfg(windows)]
            let backdrop = window_backdrop_kind(extras.transparency, extras.wallpaper, fullscreen_window);
            #[cfg(not(windows))]
            let backdrop: Option<bool> = None;
            let focus_grab = if backdrop_experiment {
                None
            } else {
                self.window_focus_grab(id, extras.transparency, backdrop)
            };
            // The FALLBACK pixel source, chosen + cloned on the UI thread (cheap map
            // lookups + a one-shot memcpy, not a focus-dependent wait): the pre-activation
            // active-window pixels, else the freeze scene's per-window pixels, else `None`
            // (the worker grabs the toplevel live). The off-thread `focus_grab` result,
            // when it lands, WINS over this (`active.or(fallback_px)` in the worker).
            let fallback_px = if backdrop_experiment {
                None
            } else {
                match window_pixel_source(
                    self.active_win_px.contains_key(id),
                    frozen && self.frozen_win_px.contains_key(id),
                ) {
                    WindowPixelSource::PreActivation => self.active_win_px.get(id).cloned(),
                    WindowPixelSource::FrozenScene => self.frozen_win_px.get(id).cloned(),
                    WindowPixelSource::Live => None,
                }
            };
            let job = crate::screenshot::WindowCaptureJob {
                id: id.clone(),
                // Don't PaintCursors onto the live grab: it stamps the cursor at its capture-instant
                // position (over the picker), not where it was on the window at launch. We overlay
                // the launch-locked cursor (below) at the correct window-relative spot instead.
                cursor: false,
                sel: sel.clone(),
                // Master OFF delivers the frame with its own alpha KEPT (no flatten,
                // no black backing): bare means the pixels as captured. Keyed on the
                // MASTER alone, deliberately NOT the folded `recomposite` gate: the
                // fullscreen bare rule always left this extra untouched and must
                // stay byte-identical. The grab side (the focus grab above) still
                // reads the extra, so WHICH pixels arrive is unchanged.
                capture_transparency: extras.transparency || !self.window_recompositing,
                // A fullscreen window covers the whole output: no background shows, so
                // suppress wallpaper-behind regardless of the pref (DRAGON-186 Phase 3);
                // a master-off capture is bare, so no backdrop either.
                capture_wallpaper: extras.wallpaper && recomposite,
                // A bare-frame capture keeps raw edge-to-edge pixels (no rounding). Any
                // other window rounds to the theme radius (macOS derives its real radius
                // from the captured alpha corner downstream and overrides this).
                window_radius: if recomposite { deco_radius } else { 0.0 },
                // A single-window capture's portrayal is the "Window focus appearance"
                // choice (Active by default): draw the Active or Inactive border
                // (`single_window_border_active`'s native arm; the portal arm pins
                // Active). A fullscreen window already had both widths zeroed above.
                border: borders.for_active(single_window_border_active(
                    false,
                    self.window_single_active,
                )),
                window_shadow,
                // TOTAL margin from the window edge; the active-hint border lives inside it,
                // so the wallpaper/shadow gap is padding - border. A bare-frame capture
                // (fullscreen, or master off) gets no padding. DRAGON-186 Phase 3.
                pad_logical: if self.window_padding && recomposite {
                    self.window_padding_px.value as f32
                } else {
                    0.0
                },
                dark: super::theme_is_dark(),
                // The overlay (and self.outputs) is torn down before this runs, so pass the
                // launch snapshot's per-output geometry for the composite — or, when this
                // launch skipped the flats grab entirely (freeze off, not a scanner), a
                // fresh live query. See [`App::window_composite_geom`]: without the
                // fallback a freeze-off window capture composites with NO output geometry
                // and loses its "Preserve wallpaper" backing.
                frozen_geom: self.window_composite_geom(),
                // Assigned in the worker: the off-thread focus grab (if any) OR the
                // UI-chosen fallback. `None` here so the field exists; never the final value.
                frozen_px: None,
                // DRAGON-278 (Windows only): the off-thread ACTIVATED grab, assigned by the
                // worker below. It wins FIRST in `run()` without touching the frozen/live
                // precedence; the field exists only on the Windows job struct.
                #[cfg(windows)]
                active_grab: None,
                // DRAGON-292 (macOS wallpaper-backdrop EXPERIMENT, opt-in behind
                // CCK_MAC_WALLPAPER_BACKDROP=1): the "Window focus appearance" intent, so the
                // experiment renders the target in its ACTIVE (focused) or INACTIVE (defocused)
                // appearance to MATCH the border it draws (`border` above is
                // `borders.for_active(self.window_single_active)`). The field exists only on the
                // mac job struct; the experiment reads it in `run()`. Env unset -> unused.
                #[cfg(target_os = "macos")]
                backdrop_active: self.window_single_active,
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
                // DRAGON-415: a window grab that produced nothing and a file that could not
                // be written are different failures with different advice, and both used to
                // be `false`.
                let outcome = match job.run() {
                    None => super::failure::ShotOutcome::NoImage,
                    Some(img) if crate::media::png::save_png(&img, &path, &meta) => {
                        super::failure::ShotOutcome::Saved
                    }
                    Some(_) => super::failure::ShotOutcome::SaveFailed,
                };
                let _ = tx.send((path, outcome));
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
            // DRAGON-336: every scene read this branch makes is already done — the job
            // (with its CLONED `fallback_px` + `frozen_geom`) has been moved onto the
            // worker thread above, and `window_defocus_uses_spinner` reads only
            // `frozen_toplevels`, which the release keeps. Drop the pixels now rather
            // than holding them through the grab + compose + the whole preview session.
            self.release_capture_scene();
            return Task::batch([
                Task::perform(async move { grab_rx.await.ok() }, move |_| {
                    cosmic::Action::App(Msg::Capture(CaptureMsg::WindowGrabbed(open_on_grab)))
                }),
                Task::perform(
                    async move {
                        match rx.await {
                            Ok(done) => done,
                            // DRAGON-418: the oneshot resolves to `Err` ONLY when the
                            // sender was dropped without sending — i.e. the capture
                            // worker thread DIED (a panic in the focus-then-grab, the
                            // composite, or the PNG write), never when a grab merely came
                            // back empty. The old `unwrap_or((fallback, false))` collapsed
                            // those two into the same `ShotSaved(_, false)`, so a panic
                            // was indistinguishable from "nothing to capture" and both
                            // ended as one silent `finish_session()`. That is precisely
                            // how this class of bug stays invisible, so name it here.
                            // (The session still ends the same way — giving failures a
                            // VOICE is DRAGON-415's job, not this seam's — but a panic
                            // must never again read as an ordinary empty capture.)
                            //
                            // DRAGON-419 routes it through `note_failure` so it also
                            // reaches the debug log with a stable code AND becomes the
                            // outcome `finish_session` reports — and so the target is
                            // named by SHAPE rather than by path (the log is emailed to
                            // us; a capture filename can name the user's document).
                            Err(_) => {
                                crate::diag::note_failure(
                                    crate::diag::Failure::WorkerPanic,
                                    &format!(
                                        "capture worker thread died before reporting a result \
                                         (panicked); no screenshot was written; target {}",
                                        crate::diag::path_shape(&fallback),
                                    ),
                                );
                                (fallback, super::failure::ShotOutcome::WorkerDied)
                            }
                        }
                    },
                    |(path, outcome)| {
                        cosmic::Action::App(Msg::Capture(CaptureMsg::ShotSaved(path, outcome)))
                    },
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
        // DRAGON-595 looked at gating this on the active backend's declared cursor
        // MECHANISM (`CursorDelivery`), to make the portal fallback's double-pointer
        // hazard unrepresentable: that path crops a seed frame which already carries
        // whatever pointer the stream was asked for, and stamping our sprite on top
        // would be a second one. It is the WRONG predicate here and the attempt is
        // recorded so it is not retried. The backend SELECTED is not the backend
        // SERVING: with layer shell present, the portal chosen, and the grant failing
        // `CastError::Unavailable`, the capture degrades to native screencopy and
        // reaches this line while `active_screenshot_backend()` still answers Portal.
        // Gating on it drops the cursor from that capture. The question this line
        // actually asks is "did the frozen scene come from the portal", which is
        // `overlay_fallback_active()`, and the very next line already uses it.
        let cursor_overlay = self.frozen_cursor.as_ref().filter(|_| extras.cursor);
        let frozen_source = frozen_region_source(extras.wallpaper, self.overlay_fallback_active());
        // DRAGON-454: the window path above hands its grab to a worker thread, but this one
        // composites AND encodes on the UI thread — and it sits directly in front of the
        // editor opening. Bracketed so the launch timeline says how much of the wait is here.
        crate::util::timing_mark("do_pixel_capture: region/monitor composite (begin, UI thread)");
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
            // DRAGON-419 (silent-exit path S1). The most-reported shape of "it just closes
            // itself": no file, no preview, no message. The branch that produced the `None`
            // is what a reader needs — a whole-output grab returning nothing is a degraded
            // capture API, a region grab returning nothing is geometry that intersects no
            // display, and they have nothing in common. Selection GEOMETRY is ours, not the
            // user's content; the display NAME is not logged.
            log::warn!("native pixel capture returned no image");
            crate::diag::note_failure(
                crate::diag::Failure::NoImage,
                &format!(
                    "branch={} sel={}x{}@{},{} frozen_flats={} wallpaper={}",
                    if sel.output.is_some() { "output" } else { "region" },
                    sel.width,
                    sel.height,
                    sel.x,
                    sel.y,
                    self.frozen.len(),
                    extras.wallpaper,
                ),
            );
            // DRAGON-415: and TELL the user. `fail_session` reports the ROOT cause this
            // process recorded, so an SCK stall or a permission denial noted upstream is
            // what gets named here rather than the "no image" symptom it produced.
            return self.fail_session();
        };
        if crate::util::timing_on() {
            crate::util::timing_mark(&format!(
                "do_pixel_capture: region/monitor composite (done, {}x{})",
                img.width(),
                img.height()
            ));
        }
        // Save the PNG straight to the screenshots folder (no external tool).
        if !crate::media::png::save_png(&img, &path, &self.screenshot_metadata()) {
            // DRAGON-419 (silent-exit path S3). `save_png` returns a bool, so the real
            // `io::Error` is already gone by here — the SHAPE of the target is what is left
            // to say, and it separates the three real causes (folder deleted, folder
            // read-only, name/path unusable) without carrying a byte of the path itself.
            log::warn!("failed to write screenshot to {}", path.display());
            crate::diag::note_failure(
                crate::diag::Failure::SaveFailed,
                &format!(
                    "save_png returned false; target {} px={}x{}",
                    crate::diag::path_shape(&path),
                    img.width(),
                    img.height(),
                ),
            );
            // DRAGON-415: the log gets the path SHAPE (it is emailed to us); the alert, shown
            // only on the user's own screen, can name their actual capture folder — which is
            // the whole actionable content of a "could not write the file" message.
            return self.fail_session();
        }
        crate::util::timing_mark("do_pixel_capture: save_png (done, capture is on disk)");
        // DRAGON-336: the region/monitor composite is finished and the PNG is on disk —
        // the frozen flats, the per-window pixels and the picker wallpapers can never be
        // read again in this process (the preview reads the saved file). Release them
        // before the preview opens, so the long-lived preview phase doesn't sit on them.
        self.release_capture_scene();
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
        // DRAGON-308 (Windows): whether to float a backdrop below the glass window during the
        // grab (`Some(true)`=wallpaper, `Some(false)`=black, `None`=none). Consumed only by the
        // Windows arm; mac/Linux ignore it (their grab already isolates the window's own alpha).
        backdrop: Option<bool>,
    ) -> Option<Box<dyn FnOnce() -> Option<image::RgbaImage> + Send>> {
        let intent = window_focus_intent(self.window_single_active);
        #[cfg(target_os = "linux")]
        {
            // Linux drives focus via the Wayland toplevel activate; transparency is chosen
            // downstream in the worker, not at the focus grab.
            let _ = (transparency, backdrop);
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
            // transparency choice is applied downstream, not at the focus grab. The mac
            // wallpaper-backdrop recomposite is a SEPARATE path (`wm/wallpaper_backdrop`),
            // so the Windows `backdrop` decision is unused here.
            // DRAGON-643: both mac arms wait on the WINDOW SERVER's z-order agreeing that the
            // picked window is the front one, not on the app reporting its own focus. Picking
            // a window of a MULTI-WINDOW app (typically the one on another monitor, since the
            // one on this monitor is usually the app's last-active window anyway) used to
            // confirm instantly off an attribute we had just written and grab the inactive
            // chrome. The wait is still bounded and still best-effort on timeout.
            let _ = (transparency, backdrop);
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
            // DRAGON-308: the backdrop float/grab floats its OWN opaque loader cover synchronously
            // (`engage_loader_cover`), so no cover state needs threading in from the UI thread.
            let id = id.to_string();
            Some(Box::new(move || match intent {
                WindowFocusIntent::Focus => {
                    crate::platform::windows::capture_window_active(&id, transparency, backdrop)
                }
                WindowFocusIntent::Defocus => {
                    crate::platform::windows::capture_window_inactive(&id, transparency, backdrop)
                }
            }))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = (id, intent, transparency, backdrop);
            None
        }
    }
}

/// DRAGON-308 (Windows): whether to float an opaque backdrop below a picked GLASS window
/// during the grab so its acrylic composites against the wallpaper, not the user's other
/// windows — `Some(true)` = the aligned desktop wallpaper, `Some(false)` = solid black,
/// `None` = float nothing. When wallpaper is on we float the wallpaper (the ticket's ask);
/// when off we float black, so the glass never shows the user's other windows either way.
///
/// DRAGON-426 dropped the FULLSCREEN suppression, and the reason is worth stating: this stopped
/// being a decision about how the capture LOOKS and became the precondition that lets it
/// preserve transparency at all (`wm/focus.rs`). The old reasoning — "a fullscreen window fills
/// its output, so there is nothing behind it" — is true of an OPAQUE fullscreen window and
/// false of a translucent one, which shows the desktop, and every other window on it, straight
/// through. Suppressing the backdrop there left exactly the case with the most to leak taking
/// an unprotected grab. Now the answer turns only on whether this grab keeps transparency;
/// `fullscreen` is accepted and ignored, kept in the signature because the caller's own
/// fullscreen handling (no padding, no rounding, no wallpaper composite) still reads from it.
/// Pure so the decision is unit-testable.
#[cfg(windows)]
pub(super) fn window_backdrop_kind(transparency: bool, wallpaper: bool, fullscreen: bool) -> Option<bool> {
    let _ = fullscreen;
    transparency.then_some(wallpaper)
}

/// Whether the DRAGON-292 macOS wallpaper-backdrop recomposite is active for this window
/// capture: on macOS this is the DEFAULT (see `wallpaper_backdrop::enabled` — the
/// `CCK_MAC_WALLPAPER_BACKDROP=0` escape hatch disables it). When true, a single-window
/// capture suppresses the cached/frozen/focus-grab pixel sources so `WindowCaptureJob::run`
/// reaches its live recomposite arm (which raises the target and grabs it over a floated
/// wallpaper backdrop). Always `false` off macOS, so every non-mac build is byte-identical.
fn wallpaper_backdrop_experiment_active() -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::platform::mac::wallpaper_backdrop::enabled()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The shared tolerance (logical px) for the fullscreen geometry gate: the ONE
/// constant both consumers of [`is_fullscreen`] pass — the native single-window
/// gate in `do_pixel_capture` and the portal-path gate
/// ([`portal_window_fullscreen`], DRAGON-562) — so the two rules cannot drift.
pub(super) const FULLSCREEN_TOL: i32 = 2;

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

/// Pure, unit-tested (`window_recomposite_tests`): whether a single-window capture
/// runs the aesthetics recomposite at all (borders / drop shadow / rounding /
/// padding / wallpaper backdrop), or delivers the BARE frame. This is THE one gate
/// both window-still paths consult, so native and portal cannot drift:
///
/// - the NATIVE window branch keys its aesthetic-knob zeroing on it (the treatment
///   a truly-fullscreen window always got, DRAGON-186 Phase 3);
/// - [`PortalWindowDeco::apply`] returns the untouched frame when it says bare.
///
/// Two inputs, ANDed: the MASTER "Enable single window aesthetic effects"
/// setting (`window_recompositing`; OFF = the user wants raw window frames, every
/// per-extra preference preserved for a later re-enable), and the fullscreen owner
/// rule (a truly-fullscreen application is never decorated, whatever the master
/// says).
pub(super) fn window_recomposite(master: bool, fullscreen: bool) -> bool {
    master && !fullscreen
}

/// Pure, unit-tested (DRAGON-562): the fullscreen bare-frame gate as the PORTAL
/// window-still path consumes it. A CONSUMER of the native rule, not a second
/// rule: the verdict is [`is_fullscreen`] with the shared [`FULLSCREEN_TOL`],
/// fed the grant's own facts — the window's global logical position
/// (`StreamInfo.position`), the grabbed frame's physical size mapped through the
/// origin output's buffer scale, and that output's logical rect (resolved at
/// grant time by `output_for_grant_position`, the DRAGON-549 containment).
///
/// Missing facts (a position-less portal impl, a position on no registered
/// output) answer false, the same shape as the native gate's `is_some_and`: an
/// undecidable window is decorated rather than silently stripped. The native
/// path's per-platform override (`crate::screenshot::window_is_fullscreen`) is
/// constant `false` on Linux, so consuming only the geometry gate loses nothing.
/// A degenerate `scale` (<= 0) falls back to 1.0 rather than dividing by zero.
// Its one production caller is the Linux portal still branch of
// `do_pixel_capture`; compiled into every test build so the decision is proven
// on any host (the house pattern).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn portal_window_fullscreen(
    fullscreen_aware: bool,
    pos: Option<(i32, i32)>,
    origin_rect: Option<(i32, i32, i32, i32)>,
    frame_px: (u32, u32),
    scale: f32,
) -> bool {
    if !fullscreen_aware {
        return false;
    }
    let Some(out) = origin_rect else {
        return false;
    };
    let s = if scale > 0.0 { scale } else { 1.0 };
    let w = (frame_px.0 as f32 / s).round() as i32;
    let h = (frame_px.1 as f32 / s).round() as i32;
    match pos {
        // A grant that carries its position gets the full rule, position and size both.
        Some((px, py)) => is_fullscreen((px, py, w, h), out, FULLSCREEN_TOL),
        // DRAGON-593: no position, so SIZE is the only signal, and it is enough. This is
        // not a degraded guess: we already know WHICH output the grant came from
        // (`origin_rect`), so a frame whose logical size matches that output's size is
        // covering it, and where it starts cannot change that.
        //
        // This arm is the common case, not the exception. cosmic-comp's portal sends
        // `position: None` for EVERY window stream (measured in DRAGON-562, which is how
        // the wallpaper backdrop was found to be silently skipped). The old code required
        // BOTH terms, so the guard tripped on every portal window capture and the
        // fullscreen rule could never fire at all: a fullscreen game came back padded,
        // bordered and rounded, which is exactly what the owner reported.
        None => {
            let (_, _, ow, oh) = out;
            (w - ow).abs() <= FULLSCREEN_TOL && (h - oh).abs() <= FULLSCREEN_TOL
        }
    }
}

/// DRAGON-562: everything a portal WINDOW still needs to run the native
/// single-window aesthetics off the UI thread. The JOB half is the SAME
/// [`crate::screenshot::WindowCaptureJob`] the native window branch builds (same
/// fields, same `run()`), fed the portal frame as its pixel source; the GRANT
/// half carries the geometry the fullscreen gate and the wallpaper crop need.
///
/// What is structurally absent here, on purpose: frosted glass (reproducing it
/// needs the scene BEHIND the window, which the portal cannot provide;
/// `run()`'s glass arm keys on `capture_transparency`, false on this backend)
/// and the cursor sprite overlay (the portal offers only embedded-or-hidden
/// cursors, never a sprite session).
#[cfg(target_os = "linux")]
pub(super) struct PortalWindowDeco {
    grant: super::PortalWindowGrant,
    job: crate::screenshot::WindowCaptureJob,
    fullscreen_aware: bool,
    /// The master "Enable single window aesthetic effects" setting, read on the
    /// UI thread at build time like every other knob here; [`Self::apply`] folds it
    /// with the fullscreen verdict through the ONE shared [`window_recomposite`]
    /// gate, the same gate the native window branch keys its zeroing on.
    recompositing: bool,
}

#[cfg(target_os = "linux")]
impl PortalWindowDeco {
    /// Run the portal frame through the native single-window pipeline, or return
    /// it untouched when the shared [`window_recomposite`] gate says bare frame:
    /// the fullscreen owner rule (a truly-fullscreen application is never
    /// decorated — no padding, border, shadow, corners, or wallpaper) or the
    /// recompositing master switched off. Worker-thread code: the compose is the
    /// slow part; every setting was read on the UI thread at build time.
    fn apply(mut self, img: image::RgbaImage) -> image::RgbaImage {
        let fullscreen = portal_window_fullscreen(
            self.fullscreen_aware,
            self.grant.pos,
            self.grant.origin_rect,
            (img.width(), img.height()),
            self.grant.scale,
        );
        // DRAGON-593: the decorate-or-not verdict, with the numbers it was reached from.
        // Geometry only, no pixels and no path, exactly the class of fact the capture log
        // already carries. It exists because "my fullscreen game came back decorated" could
        // not be answered from a log at all: whether the grant carried a position, what the
        // frame measured, and what output it was judged against are three facts that only
        // together say whether the rule fired or could not fire.
        log::debug!(
            "portal window deco: fullscreen={fullscreen} recompositing={} \
             frame={}x{}px scale={:.2} grant_pos={:?} origin={:?}",
            self.recompositing,
            img.width(),
            img.height(),
            self.grant.scale,
            self.grant.pos,
            self.grant.origin_rect,
        );
        if !window_recomposite(self.recompositing, fullscreen) {
            return img;
        }
        let scale = if self.grant.scale > 0.0 { self.grant.scale } else { 1.0 };
        // The window's logical rect: the grant position plus the frame's size
        // mapped back through the origin output's buffer scale. `run()` re-derives
        // its scale from exactly this pair (raw px / sel logical), so the two stay
        // consistent by construction.
        let lw = (img.width() as f32 / scale).round().max(1.0) as u32;
        let lh = (img.height() as f32 / scale).round().max(1.0) as u32;
        // The wallpaper anchor, and the ONE behavior line saying what was decided
        // (fifth live test: the silent no-position drop read as "the backdrop
        // never engages", with nothing in the log to say why).
        match (self.grant.pos, self.job.capture_wallpaper) {
            (Some((px, py)), wallpaper) => {
                self.job.sel.x = px;
                self.job.sel.y = py;
                log::debug!(
                    "portal window still: {} (grant position)",
                    if wallpaper { "wallpaper backdrop engaged" } else { "wallpaper extra off" },
                );
            }
            // No position — the MEASURED norm, not an edge case: COSMIC's own
            // portal builds every window stream with `position: None` (its
            // screencast source constructs monitor streams from the output's
            // logical position and window streams with an explicit None). So a
            // real anchor never arrives here, and dropping the wallpaper "until
            // one does" means the backdrop never engages at all. Instead:
            // synthesize one (centered on the largest registered output) so the
            // aesthetic the toggle promises survives a position-less portal. The
            // crop is a fiction either way — there is no desktop alignment to
            // preserve — and the black fallback stays for the outputless case.
            (None, true) => match synthetic_window_anchor(&self.grant.outputs, (lw, lh)) {
                Some((ax, ay)) => {
                    self.job.sel.x = ax;
                    self.job.sel.y = ay;
                    log::debug!(
                        "portal window still: wallpaper backdrop engaged (synthetic anchor; \
                         the portal gave this window stream no position)"
                    );
                }
                None => {
                    self.job.capture_wallpaper = false;
                    log::debug!(
                        "portal window still: wallpaper backdrop skipped (no position from \
                         the portal and no registered outputs to anchor a crop)"
                    );
                }
            },
            (None, false) => {
                log::debug!("portal window still: wallpaper extra off (no grant position)");
            }
        }
        self.job.sel.width = lw;
        self.job.sel.height = lh;
        self.job.frozen_px = Some(img);
        self.job
            .run()
            .expect("WindowCaptureJob with frozen_px set never fails to produce an image")
    }
}

/// Pure; unit-tested (`synthetic_window_anchor_tests`). Where to PLACE a portal
/// window frame whose grant carries no position, so the wallpaper crop has an
/// anchor (DRAGON-562 fix round). COSMIC's portal sends `position: None` for
/// EVERY window stream (measured against its screencast source: monitor streams
/// carry the output's logical position, window streams an explicit None), so
/// waiting for a real position means the wallpaper backdrop never engages at
/// all on the one compositor the Flatpak targets.
///
/// The anchor centers the frame on the LARGEST registered output (first wins a
/// tie; the WHICH-output half of the decision is [`largest_output_index`],
/// shared with the preview-origin resolution so the two cannot drift):
/// registration order is meaningless (this desktop's first-registered
/// output is an 800x480 side panel, the mistake DRAGON-563 already corrected
/// once), and the largest display is where desktop windows overwhelmingly
/// live. The anchor is clamped to the output's top-left so the frame's CENTER
/// stays on-output (the composite resolves the wallpaper by center
/// containment). `frame_logical` is the frame mapped through the grant scale;
/// a position-less grant resolves no origin output, so that scale is 1.0 and
/// the fiction is centered in physical pixels — acceptable, because with no
/// position there is no true desktop alignment to preserve anyway. `None` when
/// no output has positive area: black stays the honest fallback.
// Consumed by `PortalWindowDeco::apply` (Linux portal still path); compiled
// into every test build so the decision is proven on any host (the house
// pattern).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn synthetic_window_anchor(
    outputs: &[AnchorOutput],
    frame_logical: (u32, u32),
) -> Option<(i32, i32)> {
    let sizes: Vec<(i32, i32)> = outputs.iter().map(|(_, _, size)| *size).collect();
    let (_, pos, size) = &outputs[largest_output_index(&sizes)?];
    let ((ox, oy), (ow, oh)) = (*pos, *size);
    let (lw, lh) = (frame_logical.0 as i32, frame_logical.1 as i32);
    let x = ox + (ow - lw) / 2;
    let y = oy + (oh - lh) / 2;
    Some((x.max(ox), y.max(oy)))
}

/// Pure, unit-tested (`largest_output_index_tests`): WHICH registered output the synthetic
/// window anchor stands on, as an index into `sizes` (each entry one output's logical
/// size, in registration order). Largest logical area wins, first wins a tie; `None` when
/// no output has positive area.
///
/// One decision with two consumers, deliberately (DRAGON-549 reopened):
/// [`synthetic_window_anchor`] centers a position-less portal window frame's wallpaper
/// crop on it, and the portal grant handler (`App::on_pipewire_cast_ready`) resolves
/// `portal_origin_output` through it when a WINDOW grant carries no position. COSMIC's
/// portal sends `position: None` for EVERY window stream (the DRAGON-562 measurement), so
/// the DRAGON-549 containment can never fire on one, and without this the preview's
/// anchor ladder fell through to `outputs.first()`: registration order, the owner's
/// 800x480 side panel, which bounded every window capture's editor to the same floored
/// window (the sixth live test's `monitor=(800, 480)pt` lines). The wallpaper compose and
/// the preview must agree on the display a position-less window "belongs to"; a second
/// copy of this scan is how they would drift.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn largest_output_index(sizes: &[(i32, i32)]) -> Option<usize> {
    let mut best: Option<(i64, usize)> = None;
    for (i, size) in sizes.iter().enumerate() {
        let area = (size.0.max(0) as i64) * (size.1.max(0) as i64);
        if area > 0 && best.is_none_or(|(a, _)| area > a) {
            best = Some((area, i));
        }
    }
    best.map(|(_, i)| i)
}

/// One registered output as [`synthetic_window_anchor`] needs it — the same
/// `(name, logical_pos, logical_size)` triple `crate::screenshot::OutputGeom`
/// aliases on both desktop platforms, spelled locally so the pure decision's
/// signature names no platform module (and clippy's type-complexity gate stays
/// quiet without an allow).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
type AnchorOutput = (String, (i32, i32), (i32, i32));

/// Whether the frozen backdrop is shown DURING SELECTION, factored pure so the
/// countdown-release rule is testable on any OS. `freezing` is the native freeze gate
/// (`App::freezing`); `fallback` is the fallback overlay's seed-still term
/// ([`fallback_backdrop`]); `delay_secs` is the configured pre-capture delay
/// (`configured_delay_secs`).
///
/// The delay rule splits BY PATH (`lab/flatpak`, DRAGON-547 reopened):
///
/// - NATIVE sessions, the `freezing` term: DRAGON-186 Phase 4, unchanged. An armed
///   countdown (`delay_secs > 0`) releases the backdrop so selection previews the
///   LIVE screen the delayed shot will actually grab, mirroring how video mode
///   releases it (there `freezing` is already false). No delay re-freezes. The
///   release informs because a transparent layer-shell overlay really does show
///   the live desktop behind it.
/// - The FALLBACK path, the `fallback` term: the seed still shows REGARDLESS of the
///   delay. A plain toplevel has no live desktop composited behind it (the
///   compositor backs it with a flat fill), so a released backdrop there renders
///   flat gray for scanner, photo and video alike: the owner's seventh live test.
///   And the still lies about nothing the capture will take: a delayed fallback
///   capture re-grabs at fire time ([`fallback_still_from_frozen`]'s delay rule,
///   the DRAGON-546 round), so the delayed shot never delivers these pixels anyway.
///   The countdown PHASE is untouched (DRAGON-563: the fallback window closes at
///   countdown start); this governs the SELECTION phase only.
pub(super) fn freeze_backdrop_active(freezing: bool, fallback: bool, delay_secs: u64) -> bool {
    (freezing && delay_secs == 0) || fallback
}

// DRAGON-460 removed `scanner_backdrop_active`.
//
// DRAGON-456 added it to close a real bug: the scanner read the launch-instant flats while
// the overlay showed the LIVE desktop, so alt-tabbing left the scan returning the old
// window's text with nothing on screen to say why. Freezing the view made what is shown and
// what is read the same pixels.
//
// It is unnecessary now, because the scanner reads a live shot of the selection instead
// (`begin_scan_shot` / `run_scan_shot`). Shown and read are the same pixels for the stronger
// reason that they ARE the same pixels, so the overlay goes back to transparent and the user
// watches the real screen.
//
// Removing it also deletes the defect it introduced. The backdrop was the launch-instant
// flat, and that grab is deferred off the init thread (DRAGON-212 / DRAGON-148) so the
// overlay is already mapped while it runs — meaning the still had our own toolbar and region
// chrome photographed into it, which the user then saw presented as their screen.

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
///
/// # What this promises on a PORTAL session (DRAGON-592)
///
/// It promises PRESENCE, not position. The extra can now be on for a portal capture
/// (`Caps::cursor_toggle`), and a portal frame really will carry a pointer, so the
/// indicator saying "your capture will include it" is true. WHERE it lands is not
/// ours to preview there: the portal bakes the pointer in at grab time, which is a
/// beat after its permission dialog closes, while this sprite is the LAUNCH-LOCKED
/// one. Nothing in the tree can reconcile that, since the portal hands back a
/// finished frame and no sprite (`PortalBackend::cursor` answers `None`).
///
/// It is a narrow case: it needs a session that has BOTH a native cursor session to
/// lock a sprite from AND the portal method chosen in settings, so it cannot happen
/// in a Flatpak sandbox (no native sprite there, `frozen_cursor` stays `None` and the
/// indicator is inert by construction). Left as-is deliberately rather than gated:
/// suppressing it would mean asking which backend is running from view code, and the
/// capability table is the only thing allowed to answer that.
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
///
/// `portal_seeded` (`lab/flatpak`) is the fallback overlay's structural override: its
/// frozen scene is ONE finished portal frame of the granted monitor, with no
/// per-window pixels and no wallpaper channel, so the flat snapshot is the only honest source
/// whatever the wallpaper preference says. Routing wallpaper-OFF through the
/// windows-only composite there would build the empty-set canvas and deliver a black
/// rectangle for a selection the user drew over real content. Pure, unit-tested.
pub(super) fn frozen_region_source(keep_wallpaper: bool, portal_seeded: bool) -> FrozenRegionSource {
    if keep_wallpaper || portal_seeded {
        FrozenRegionSource::FlatSnapshot
    } else {
        FrozenRegionSource::WindowsOnly
    }
}

/// Pure, unit-tested (`lab/flatpak`, DRAGON-563): must `begin_capture` KEEP the
/// countdown-start preview anchor instead of overwriting it with its fresh resolution?
/// Only on the fallback path can a snapshot exist at all (EVERY fallback countdown
/// snapshots it and then tears the outputs down — tray or no tray, since the reopened
/// DRAGON-563 removed the window-countdown degrade there), and only there does the
/// fresh resolution come back empty at fire time. A fresh answer always wins, the same
/// precedence `stop_preview_anchor` gives the record-stop resolution. Every
/// non-fallback path answers false, keeping the historical overwrite byte-identical.
pub(super) fn keep_countdown_anchor(
    fallback: bool,
    fresh_resolved: bool,
    snapshot_held: bool,
) -> bool {
    fallback && !fresh_resolved && snapshot_held
}

/// Pure, unit-tested (`lab/flatpak`, the one-shot countdown round): does a countdown
/// that just FIRED consume the persisted timer, resetting `delay_idx` to the "No
/// delay" preset? The owner's rule: the countdown timer is ONE-SHOT. Actually
/// performing the delayed capture/recording spends it, so the next launch and every
/// menu title read "Countdown Timer: 00"; it is not saved forever. Consumption
/// happens at the FIRE instant, whether or not the action then succeeds: the
/// countdown ran and fired, so it is spent. This is global on purpose: the overlay's
/// delay chips, the tray radio picks and the fallback tray digits all share
/// `delay_idx`, so native and fallback sessions get the same one-shot semantic.
///
/// Deliberate limits, each here for a reason:
///
/// - A CANCELLED countdown keeps the setting: the user did not get their capture, so
///   the timer they configured is still owed to the retry. (Flagged for owner veto in
///   the round's report.)
/// - A CLI `--countdown` override (`cli_override`) drove this fire without reading
///   the persisted preset, so it must not spend somebody else's setting.
/// - `delay_idx == 0` has nothing to spend, and skipping it avoids a pointless
///   config write on every immediate capture.
pub(super) fn countdown_consumed(fired: bool, cli_override: bool, delay_idx: usize) -> bool {
    fired && !cli_override && delay_idx != 0
}

/// Pure, unit-tested (`lab/flatpak`): does the fallback overlay show the seed-frozen
/// backdrop during SELECTION? Keyed on region mode and the seed frame being present,
/// and DELIBERATELY not on the capture kind: a fullscreen toplevel has no live
/// desktop composited behind it (the compositor backs it with a flat fill, seen live
/// as "video mode is just a flat gray screen"), so the seed still is the only honest
/// backdrop for VIDEO region selection too. The normal-session rule is untouched:
/// video there releases the backdrop to show the genuinely live desktop, which the
/// fallback simply does not have. The countdown DELAY is deliberately not an input
/// either (DRAGON-547 reopened): [`freeze_backdrop_active`] keeps this term frozen
/// whatever the configured delay, because the flat gray fill is the only alternative
/// and a delayed fallback capture re-grabs at fire time anyway.
pub(super) fn fallback_backdrop(
    fallback: bool,
    region_mode: bool,
    frozen_has_pixels: bool,
) -> bool {
    fallback && region_mode && frozen_has_pixels
}

/// Pure, unit-tested (`lab/flatpak`): does a REGION STILL commit on the fallback
/// overlay crop from the SEED-FROZEN frame instead of requesting a fresh portal frame?
///
/// Yes exactly when the fallback is active, no delay is configured, and the seed frame
/// actually landed (`frozen_has_pixels`). The frozen crop is the WYSIWYG source: the
/// user drew their region over that still, so a fresh live frame could deliver
/// different pixels than the ones they selected. A configured DELAY flips it the other
/// way: the delay exists to change the screen, so the per-capture portal request (and
/// its live frame at fire time) is the honest source, exactly like the native path's
/// `capture_live` rule. Off the fallback this is always false, which is what keeps the
/// normal portal region-still request byte-identical.
// Its one production caller is the Linux portal branch of `run_capture`; compiled into
// every test build so the decision is proven on any host (the house pattern).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn fallback_still_from_frozen(
    fallback: bool,
    delay_secs: u64,
    frozen_has_pixels: bool,
) -> bool {
    fallback && delay_secs == 0 && frozen_has_pixels
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
    is_defocus_sink: bool,
    wants_editor: bool,
) -> bool {
    // DRAGON-353 removed the old `preview_enabled` term with the "Open in preview editor"
    // SETTING. DRAGON-428 reintroduces the same shape for a different reason: not a
    // persisted preference, but a per-launch `--no-editor`. The spinner exists ONLY to
    // cover the grab until the editor appears, so a launch that will never open one must
    // not mint it — otherwise the user sees a spinner overlay flash and vanish for a
    // capture that was always going straight to the clipboard.
    immediate && is_window_pick && !is_defocus_sink && wants_editor
}

/// DRAGON-216: the pure decision behind [`App::window_pick_preopens_window`] (macOS). A
/// window pick pre-opens its preview surface to cover the off-thread focus-then-grab, for
/// BOTH appearances now: the WINDOWED preview opens order-front, and the fullscreen OVERLAY
/// preview is placed WITHOUT taking focus (`gain_focus` deferred to `WindowGrabbed`) — so
/// neither disturbs the picked window's key/frontmost state the grab depends on. Fires when
/// the commit is immediate, it's a window pick, and the preview is enabled. There is no
/// defocus-sink term — macOS has no single-toplevel spinner focus-sink. Portable pure logic
/// (tested on every platform); the macOS and Windows callers consult it (DRAGON-305), so it is
/// dead code only on Linux + exotic targets.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn window_pick_preopen_decision(immediate: bool, is_window_pick: bool, wants_editor: bool) -> bool {
    // DRAGON-353 removed the old `preview_enabled` term with the "Open in preview editor"
    // SETTING; DRAGON-428 reintroduces the same shape for the per-launch `--no-editor`.
    // The pre-open exists only to cover the focus-then-grab until the editor appears, so a
    // launch that opens no editor must not pre-open a surface it will never fill.
    immediate && is_window_pick && wants_editor
}

/// DRAGON-305: the pure decision behind the Windows [`App::window_pick_preopens_window`] arm. A
/// single-window capture pre-opens the fullscreen loading BLOCKER cover (spinner + cancel X) —
/// but ONLY when the post-capture editor is WINDOWED (`windowed`): overlay-preview mode already
/// shows the fullscreen blocker after the grab, so it needs no pre-open. Otherwise it is exactly
/// [`window_pick_preopen_decision`] (immediate window pick with preview enabled). Windows-only.
#[cfg(windows)]
fn windows_window_pick_preopens(
    immediate: bool,
    is_window_pick: bool,
    windowed: bool,
    wants_editor: bool,
) -> bool {
    windowed && window_pick_preopen_decision(immediate, is_window_pick, wants_editor)
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

/// Pure, unit-tested (`single_window_border_tests`): the active-vs-inactive flag a
/// single-window capture's drawn border keys on (`WindowBorders::for_active`).
///
/// - NATIVE path (`portal` false): the persisted "Window focus appearance" choice.
///   Honest there because the capture DRIVES the window's real focus state to match
///   before grabbing (DRAGON-194), so the drawn border and the native chrome agree.
/// - PORTAL path (`portal` true): ALWAYS the Active border, whatever the setting
///   says. The grant carries no activation info and the portal cannot activate or
///   deactivate windows, so the state-keyed choice is unanswerable there; the
///   portal-picked window was interactively chosen by the user, so Active is the
///   honest deterministic default. (The settings page hides the focus-appearance
///   selector and the Inactive border rows on fallback sessions for the same reason.)
pub(super) fn single_window_border_active(portal: bool, single_active: bool) -> bool {
    portal || single_active
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

/// DRAGON-295: the [`crate::platform::backend::OutputDesc`] the pointer sits on, for the
/// picker-free "Capture Active Monitor" hotkey. Returns the display whose logical bounds
/// contain `pointer`; when the pointer maps to none (or is unknown), falls back to the
/// primary display (logical origin `(0, 0)`), else the first listed. `None` only when there
/// are NO displays. Pure (takes the pointer + the display list), so it unit-tests without
/// any window server.
///
/// On Linux the "pointer" is resolved from the momentary cursor-output probe
/// (`capture_pointer_output`) rather than a live coordinate, but the same fallback branch
/// (`pointer == None` → primary output) is what [`App::immediate_cursor_monitor`] leans on
/// when the probe can't resolve the cursor's monitor. The geometry is identical either way.
pub(crate) fn monitor_for_pointer(
    pointer: Option<(i32, i32)>,
    descs: &[crate::platform::backend::OutputDesc],
) -> Option<crate::platform::backend::OutputDesc> {
    if descs.is_empty() {
        return None;
    }
    if let Some((px, py)) = pointer
        && let Some(hit) = descs.iter().find(|d| {
            let (ox, oy) = d.logical_pos;
            let (w, h) = d.logical_size;
            px >= ox && px < ox + w && py >= oy && py < oy + h
        })
    {
        return Some(hit.clone());
    }
    // No display under the pointer: prefer the primary (origin), else the first.
    descs
        .iter()
        .find(|d| d.logical_pos == (0, 0))
        .or_else(|| descs.first())
        .cloned()
}

/// Linux: pick the toplevel a picker-free `--active-window` should capture,
/// from the flattened `(output_name, toplevel)` enumeration plus the cursor's output. The
/// rule, in priority order:
///
/// 1. **Any `Activated` toplevel** — the normal case, the compositor's focused window.
/// 2. **A window on the CURSOR's output** — when NOTHING is `Activated` (keyboard focus on
///    an empty desktop / a different monitor, so no window holds the compositor's activation
///    state, the confirmed cause of the "shows the picker" bug), prefer a window on the
///    monitor the user's cursor is actually on. cctk exposes NO z-order, so with ≥2 windows
///    on that monitor we can't know the frontmost; pick the LOWEST `id` as a stable,
///    heuristic tie-break (deterministic run-to-run rather than HashMap/enumeration order).
/// 3. **The single existing window** — if exactly ONE distinct window (by id) exists
///    anywhere, it is unambiguously the one to capture even with no activation and no cursor
///    match (the user's one-window-on-another-monitor case).
/// 4. Otherwise `None` — a genuinely ambiguous idle desktop (multiple windows, none active,
///    none under the cursor) falls the caller back to the window picker.
///
/// Pure (plain names + [`Toplevel`](crate::platform::compositor::Toplevel)s), so it
/// unit-tests without cctk.
#[cfg(target_os = "linux")]
pub(crate) fn pick_immediate_window(
    windows: &[(String, crate::platform::compositor::Toplevel)],
    cursor_output: Option<&str>,
) -> Option<crate::platform::compositor::Toplevel> {
    // 1. The Activated window.
    if let Some((_, t)) = windows.iter().find(|(_, t)| t.active) {
        return Some(t.clone());
    }
    // 2. A window on the cursor's output. z-order is unavailable, so break ties by the
    //    LOWEST id — a stable choice independent of the HashMap/enumeration order the flat
    //    list arrives in (never `find`, which would pick an arbitrary run-to-run window).
    if let Some(name) = cursor_output
        && let Some((_, t)) = windows
            .iter()
            .filter(|(out, _)| out == name)
            .min_by(|(_, a), (_, b)| a.id.cmp(&b.id))
    {
        return Some(t.clone());
    }
    // 3. Exactly one distinct window overall (a window spanning outputs appears once per
    //    output key, so dedupe by id before counting).
    let mut ids = std::collections::HashSet::new();
    for (_, t) in windows {
        ids.insert(t.id.as_str());
    }
    if ids.len() == 1 {
        return windows.first().map(|(_, t)| t.clone());
    }
    // 4. Ambiguous — let the caller show the picker.
    None
}

/// `lab/flatpak`: can an immediate, picker-free capture (`--active-window` /
/// `--active-monitor`) resolve and grab its target in this session at all? The immediate
/// flags exist to capture the ACTIVE target with zero interaction, so a portal dialog
/// cannot stand in; the answer has to come from the compositor's own protocols:
///
/// * `ActiveWindow` needs the toplevel list (to know WHICH window is active) AND the
///   native capture protocols (to grab it).
/// * `ActiveMonitor` needs the native capture protocols (`image_copy_capture` +
///   `output_source`). The cursor-output probe can degrade to the primary output, but
///   without screencopy there is no picker-free monitor grab to run at all.
///
/// `false` sends the launch to the honest failure path (`diag::note_failure` +
/// `App::fail_session`): the sessions that hide these protocols hide layer shell too, so
/// the picker overlay is not a fallback there. Keyed on the PROTOCOL probe, never on
/// sandbox detection, so a normal COSMIC session answers `true` for both flags and stays
/// byte-identical. The same rule covers a video kind: the gate runs before the kind
/// branches. macOS/Windows never consult this (their immediates resolve through OS APIs
/// that always exist). Pure; unit-tested in `immediate_target_tests`.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn immediate_target_resolvable(
    imm: ImmediateCapture,
    native_capture: bool,
    window_list: bool,
) -> bool {
    match imm {
        ImmediateCapture::ActiveWindow => native_capture && window_list,
        ImmediateCapture::ActiveMonitor => native_capture,
    }
}

/// The two protocol terms [`immediate_target_resolvable`] asks for, read once from the live
/// session (the probe itself is memoized). Split out of `immediate_capture` so the launch gate
/// and the settings page cannot disagree about what this session can do.
#[cfg(target_os = "linux")]
fn immediate_protocol_terms() -> (bool, bool) {
    let p = crate::platform::backend::wayland_protocols();
    // The window term is the SAME predicate behind `Caps.window_list` (DRAGON-620). It used to
    // be the bare `toplevel_list` flag, which wlroots sets while being unable to place a single
    // window, so `--active-window` there passed this gate and then resolved nothing. Answering
    // false sends the launch to `note_failure` + `fail_session`, which is the honest end for a
    // compositor that cannot locate the active window, and needs no new failure vocabulary.
    (
        p.image_copy_capture && p.output_source,
        crate::platform::backend::window_list_supported(&p),
    )
}

/// Runtime seam: [`immediate_target_resolvable`] for the session we are actually running in,
/// so a caller needs no `cfg` of its own (DRAGON-589).
///
/// This is the "is the action in this build AT ALL" question, and it is a different question
/// from "can the app bind a key to it". A launch that answers `false` here ends in
/// `App::fail_session` rather than capturing anything, so Settings must not list the action:
/// an unbindable action still WORKS from a terminal and earns a row with its command, while
/// an absent one earns no mention at all.
///
/// Linux answers from the compositor's advertised protocols. macOS and Windows answer `true`,
/// and that is a fact about their code rather than an assumption: both resolve the frontmost
/// window and the monitor under the cursor through OS APIs that are always present.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
pub(crate) fn immediate_capture_available(imm: ImmediateCapture) -> bool {
    #[cfg(target_os = "linux")]
    {
        let (native_capture, window_list) = immediate_protocol_terms();
        immediate_target_resolvable(imm, native_capture, window_list)
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// DRAGON-304: the [`crate::platform::backend::OutputDesc`] a selection sits on, chosen from
/// a display list — by NAME for a whole-monitor grab, else the display whose logical bounds
/// contain the selection's centre, else the primary (logical origin `(0, 0)`), else the
/// first. Pure (takes the list), so the immediate-capture output resolution unit-tests
/// without any window server. Not built on Linux (its capture keys are COSMIC custom
/// shortcuts, so the picker-free immediate path never runs there).
#[cfg(not(target_os = "linux"))]
fn output_desc_for_selection_in(
    sel: &Selection,
    descs: &[crate::platform::backend::OutputDesc],
) -> Option<crate::platform::backend::OutputDesc> {
    if descs.is_empty() {
        return None;
    }
    if let Some(name) = &sel.output
        && let Some(d) = descs.iter().find(|d| &d.name == name)
    {
        return Some(d.clone());
    }
    let cx = sel.x + sel.width as i32 / 2;
    let cy = sel.y + sel.height as i32 / 2;
    descs
        .iter()
        .find(|d| {
            let (ox, oy) = d.logical_pos;
            let (w, h) = d.logical_size;
            cx >= ox && cx < ox + w && cy >= oy && cy < oy + h
        })
        .or_else(|| descs.iter().find(|d| d.logical_pos == (0, 0)))
        .or_else(|| descs.first())
        .cloned()
}

/// DRAGON-304: [`output_desc_for_selection_in`] against a LIVE display query — the resolver
/// the picker-free immediate capture uses because its overlays (and thus `self.outputs`) are
/// never minted. Non-Linux only (the immediate path never runs on Linux).
#[cfg(not(target_os = "linux"))]
fn output_desc_for_selection(sel: &Selection) -> Option<crate::platform::backend::OutputDesc> {
    output_desc_for_selection_in(sel, &crate::screenshot::output_descs())
}

/// The pure half of [`App::window_composite_geom`]: the launch snapshot's geometry wins
/// whenever it has any, otherwise `live` is consulted (lazily — a live display query costs
/// a compositor round trip, so a freeze/scanner launch must never pay it).
///
/// Split out so the SELECTION is testable even though `output_descs()` isn't: the bug this
/// guards is precisely "empty snapshot silently becomes empty geometry".
fn pick_composite_geom<F>(
    frozen: Vec<crate::screenshot::OutputGeom>,
    live: F,
) -> Vec<crate::screenshot::OutputGeom>
where
    F: FnOnce() -> Vec<crate::screenshot::OutputGeom>,
{
    if frozen.is_empty() { live() } else { frozen }
}

/// DRAGON-429: the auto-save naming invariant — a capture is NEVER written without its
/// extension, whatever kind it is or what the window/monitor was called.
///
/// The ticket arrived as "region captures save with no extension". Triage falsified that for
/// the auto-save path (the real seam was the Windows Save As dialog, see
/// `platform::windows::file_panel`) — but the reason it took triage to falsify is that
/// nothing pinned it. These tests are that pin.
#[cfg(test)]
mod save_name_tests {
    use super::{recording_save_name, still_save_name, RECORDING_EXT, STILL_EXT};
    use crate::app::{capture_timestamp, slugify};

    /// Stems standing in for what `App::capture_stem` can build: a bare timestamp (a REGION
    /// capture — the kind the report named), and a timestamp plus each descriptor shape the
    /// window / monitor branches produce, including their empty-title fallbacks.
    fn representative_stems() -> Vec<String> {
        let ts = capture_timestamp();
        vec![
            // Region: no descriptor at all.
            ts.clone(),
            // Window, with a title, with the fallback, and with a title that is nothing but
            // punctuation (slugify eats it, so the fallback is what `capture_stem` uses).
            format!("{ts}-{}", slugify("My Document - Editor")),
            format!("{ts}-window"),
            format!("{ts}-{}", slugify("app v1.2 (beta)")),
            // Monitor, named and fallback.
            format!("{ts}-{}", slugify("DP-1")),
            format!("{ts}-monitor"),
        ]
    }

    #[test]
    fn every_still_save_name_ends_in_png() {
        for stem in representative_stems() {
            let name = still_save_name(&stem);
            assert!(name.ends_with(".png"), "{stem:?} produced {name:?}");
            // And the extension is the ONLY dot, so nothing can be read as a different type.
            assert_eq!(name.matches('.').count(), 1, "{name:?}");
        }
    }

    #[test]
    fn every_recording_save_name_ends_in_mp4() {
        for stem in representative_stems() {
            let name = recording_save_name(&stem);
            assert!(name.ends_with(".mp4"), "{stem:?} produced {name:?}");
            assert_eq!(name.matches('.').count(), 1, "{name:?}");
        }
    }

    #[test]
    fn the_helpers_are_exactly_the_format_they_replaced() {
        // Byte-identity with the inlined `format!` at both former call sites: this extraction
        // is a testability change, never a behaviour one.
        for stem in representative_stems() {
            assert_eq!(still_save_name(&stem), format!("{stem}.png"));
            assert_eq!(recording_save_name(&stem), format!("{stem}.mp4"));
        }
        assert_eq!(STILL_EXT, "png");
        assert_eq!(RECORDING_EXT, "mp4");
    }

    #[test]
    fn a_stem_can_never_smuggle_in_a_dot_or_a_path_separator() {
        // WHY the two tests above can stand in for driving `capture_stem` itself (which needs
        // a live `App`): a stem is only ever a timestamp plus an optional slugified
        // descriptor, and NEITHER part can contain a dot or a separator. So appending
        // ".png" can never produce a second extension, a hidden file, or an escape from the
        // capture folder, no matter what a window is called.
        assert!(!capture_timestamp().contains(['.', '/', '\\']));
        for title in [
            "app v1.2 (beta)",
            "notes.txt",
            "../../etc/passwd",
            "C:\\Windows\\System32",
            ".hidden",
            "....",
            "Ünïcödé — dashes",
            "",
        ] {
            let slug = slugify(title);
            assert!(
                !slug.contains(['.', '/', '\\']),
                "slugify({title:?}) leaked a path character: {slug:?}"
            );
        }
    }
}

#[cfg(test)]
mod composite_geom_tests {
    use super::pick_composite_geom;

    fn geom(name: &str, x: i32) -> crate::screenshot::OutputGeom {
        (name.to_string(), (x, 0), (2560, 1440))
    }

    /// A freeze / scanner launch HAS the flats: their launch-instant geometry is used and
    /// the live query is never made (byte-identical to the pre-fix behavior).
    #[test]
    fn a_non_empty_launch_snapshot_wins_and_skips_the_live_query() {
        let frozen = vec![geom("DP-1", 0), geom("DP-2", 2560)];
        let got = pick_composite_geom(frozen.clone(), || panic!("live query must not run"));
        assert_eq!(got, frozen);
    }

    /// DRAGON-336 regression: freeze OFF + window mode skips the flats grab, so the
    /// snapshot is empty — the composite must then get LIVE output geometry, never an
    /// empty vec (which drops the wallpaper backing in `composite_over_wallpaper`).
    #[test]
    fn an_empty_launch_snapshot_falls_back_to_live_geometry() {
        let live = vec![geom("DP-1", 0)];
        assert_eq!(pick_composite_geom(Vec::new(), || live.clone()), live);
        // The fallback is only as good as its source: an empty live query stays empty
        // (nothing to invent), which is the pre-existing no-outputs behavior.
        assert!(pick_composite_geom(Vec::new(), Vec::new).is_empty());
    }
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

// The active-vs-inactive border key for a single-window capture: the persisted choice
// on the native path, ALWAYS Active on the portal path (the grant carries no
// activation info and the portal cannot drive focus).
#[cfg(test)]
mod single_window_border_tests {
    use super::single_window_border_active;

    // Native: the drawn border follows the persisted "Window focus appearance".
    #[test]
    fn native_follows_the_persisted_choice() {
        assert!(single_window_border_active(false, true));
        assert!(!single_window_border_active(false, false));
    }

    // Portal: ALWAYS Active, both settings. The setting cannot be honored there (no
    // activation info, no way to drive focus), so the interactively-picked window is
    // portrayed Active deterministically.
    #[test]
    fn portal_always_pins_active() {
        assert!(single_window_border_active(true, true));
        assert!(single_window_border_active(true, false));
    }
}

// The ONE bare-frame gate for a single-window capture's aesthetics, consulted by the
// native window branch (knob zeroing) and the portal deco (untouched-frame return).
#[cfg(test)]
mod window_recomposite_tests {
    use super::window_recomposite;

    // Master ON, windowed: the existing behavior, the aesthetics composite runs.
    #[test]
    fn master_on_windowed_recomposites() {
        assert!(window_recomposite(true, false));
    }

    // Master OFF: the bare frame on BOTH paths, fullscreen or not. This is the
    // owner's contract: no borders, shadow, corners, padding, or wallpaper compose.
    #[test]
    fn master_off_is_bare_on_every_path() {
        assert!(!window_recomposite(false, false));
        assert!(!window_recomposite(false, true));
    }

    // Fullscreen stays bare whatever the master says (the DRAGON-186 owner rule the
    // gate folds in; master ON must not re-decorate a fullscreen window).
    #[test]
    fn fullscreen_is_bare_even_with_the_master_on() {
        assert!(!window_recomposite(true, true));
    }
}

// DRAGON-295: the pure "monitor under the pointer" resolver for the picker-free
// "Capture Active Monitor" hotkey. Compiled (and tested) on every platform now — Linux
// feeds it the active toplevel's centre (DRAGON-317).
#[cfg(test)]
mod monitor_for_pointer_tests {
    use super::monitor_for_pointer;
    use crate::platform::backend::OutputDesc;

    fn desc(name: &str, pos: (i32, i32), size: (i32, i32)) -> OutputDesc {
        OutputDesc { name: name.to_string(), logical_pos: pos, logical_size: size }
    }

    #[test]
    fn picks_the_display_containing_the_pointer() {
        let descs = vec![
            desc("A", (0, 0), (1920, 1080)),
            desc("B", (1920, 0), (2560, 1440)),
        ];
        // A pointer on the right monitor resolves to B.
        assert_eq!(
            monitor_for_pointer(Some((2000, 200)), &descs).unwrap().name,
            "B"
        );
        // A pointer on the left monitor resolves to A.
        assert_eq!(monitor_for_pointer(Some((10, 10)), &descs).unwrap().name, "A");
    }

    #[test]
    fn falls_back_to_primary_when_pointer_maps_to_none() {
        let descs = vec![
            desc("B", (1920, 0), (2560, 1440)),
            desc("A", (0, 0), (1920, 1080)),
        ];
        // A pointer off every display, or an unknown pointer, prefers the primary (origin).
        assert_eq!(monitor_for_pointer(Some((-500, -500)), &descs).unwrap().name, "A");
        assert_eq!(monitor_for_pointer(None, &descs).unwrap().name, "A");
    }

    #[test]
    fn falls_back_to_first_when_no_primary() {
        let descs = vec![desc("B", (1920, 0), (2560, 1440))];
        // No display at the origin: the first listed wins.
        assert_eq!(monitor_for_pointer(None, &descs).unwrap().name, "B");
    }

    #[test]
    fn no_displays_yields_none() {
        assert!(monitor_for_pointer(Some((0, 0)), &[]).is_none());
    }

    #[test]
    fn boundary_is_half_open() {
        let descs = vec![
            desc("A", (0, 0), (100, 100)),
            desc("B", (100, 0), (100, 100)),
        ];
        // The right edge of A (x=100) belongs to B (half-open [ox, ox+w)).
        assert_eq!(monitor_for_pointer(Some((100, 50)), &descs).unwrap().name, "B");
        assert_eq!(monitor_for_pointer(Some((99, 50)), &descs).unwrap().name, "A");
    }
}

// The pure `--active-window` picker (Activated → cursor's output → single
// window → picker). Linux-only (the helper is cfg'd to Linux).
#[cfg(all(test, target_os = "linux"))]
mod pick_immediate_window_tests {
    use super::pick_immediate_window;
    use crate::platform::compositor::Toplevel;

    fn win(id: &str, active: bool) -> Toplevel {
        Toplevel { rect: (0, 0, 100, 100), id: id.to_string(), active, title: id.to_string() }
    }

    fn on(output: &str, id: &str, active: bool) -> (String, Toplevel) {
        (output.to_string(), win(id, active))
    }

    #[test]
    fn prefers_the_activated_window() {
        // The Activated window wins regardless of the cursor's output.
        let ws = [on("HDMI-A-2", "a", false), on("DP-3", "b", true)];
        assert_eq!(pick_immediate_window(&ws, Some("HDMI-A-2")).unwrap().id, "b");
    }

    #[test]
    fn falls_back_to_a_window_on_the_cursor_output() {
        // Nothing Activated; two windows on two monitors. The cursor's monitor decides.
        let ws = [on("HDMI-A-2", "a", false), on("DP-3", "b", false)];
        assert_eq!(pick_immediate_window(&ws, Some("DP-3")).unwrap().id, "b");
        assert_eq!(pick_immediate_window(&ws, Some("HDMI-A-2")).unwrap().id, "a");
    }

    #[test]
    fn multi_window_on_cursor_output_breaks_ties_by_lowest_id_deterministically() {
        // Nothing Activated; the cursor's monitor holds THREE windows (z-order unavailable).
        // The pick is the LOWEST id ("a"), regardless of the order they appear in the flat
        // list — so it never varies with HashMap/enumeration order run-to-run.
        let forward = [
            on("DP-3", "c", false),
            on("DP-3", "a", false),
            on("DP-3", "b", false),
            on("HDMI-A-2", "z", false),
        ];
        let reversed = [
            on("HDMI-A-2", "z", false),
            on("DP-3", "b", false),
            on("DP-3", "a", false),
            on("DP-3", "c", false),
        ];
        assert_eq!(pick_immediate_window(&forward, Some("DP-3")).unwrap().id, "a");
        assert_eq!(pick_immediate_window(&reversed, Some("DP-3")).unwrap().id, "a");
    }

    #[test]
    fn single_window_captured_even_off_the_cursor_monitor() {
        // The user's repro: one window on the small monitor, cursor on the empty large one
        // (nothing Activated, no window on the cursor's output). The lone window is
        // unambiguous, so it is captured instead of showing the picker.
        let ws = [on("HDMI-A-2", "only", false)];
        assert_eq!(pick_immediate_window(&ws, Some("DP-3")).unwrap().id, "only");
        // Same when the cursor output is unknown (probe failed).
        assert_eq!(pick_immediate_window(&ws, None).unwrap().id, "only");
    }

    #[test]
    fn one_window_spanning_outputs_is_still_single() {
        // A window spanning two outputs appears once per output key (same id); it dedupes
        // to one distinct window, so the "single window" rule captures it.
        let ws = [on("HDMI-A-2", "span", false), on("DP-3", "span", false)];
        assert_eq!(pick_immediate_window(&ws, None).unwrap().id, "span");
    }

    #[test]
    fn ambiguous_multi_window_idle_desktop_yields_none() {
        // Multiple distinct windows, none Activated, none on the cursor's output → None
        // (the caller shows the picker). Cursor on a third, empty monitor.
        let ws = [on("HDMI-A-2", "a", false), on("HDMI-A-2", "b", false)];
        assert!(pick_immediate_window(&ws, Some("DP-9")).is_none());
        // No windows at all is also None.
        assert!(pick_immediate_window(&[], Some("DP-3")).is_none());
    }
}

// lab/flatpak: the pure "can an immediate capture work here at all?" gate. Compiled (and
// tested) on every platform; only the Linux immediate path consults it.
#[cfg(test)]
mod immediate_target_tests {
    use super::immediate_target_resolvable;
    use crate::app::ImmediateCapture;

    #[test]
    fn a_full_native_session_serves_both_flags() {
        // Normal COSMIC: every protocol present. Both flags keep today's behavior.
        assert!(immediate_target_resolvable(ImmediateCapture::ActiveWindow, true, true));
        assert!(immediate_target_resolvable(ImmediateCapture::ActiveMonitor, true, true));
    }

    #[test]
    fn a_sandboxed_session_serves_neither() {
        // The lab/flatpak measurement: cosmic-comp hides both the capture protocols and
        // the toplevel list from any client carrying a security context.
        assert!(!immediate_target_resolvable(ImmediateCapture::ActiveWindow, false, false));
        assert!(!immediate_target_resolvable(ImmediateCapture::ActiveMonitor, false, false));
    }

    #[test]
    fn active_window_needs_both_seams() {
        // A toplevel list alone (KDE-shaped: a list without ext-image-copy-capture)
        // cannot grab the window it resolves, and capture protocols alone cannot say
        // which window is active. Either half missing disables the flag.
        assert!(!immediate_target_resolvable(ImmediateCapture::ActiveWindow, false, true));
        assert!(!immediate_target_resolvable(ImmediateCapture::ActiveWindow, true, false));
    }

    #[test]
    fn active_monitor_ignores_the_window_list() {
        // A hidden toplevel list must not disable a monitor grab that can still run; a
        // list without capture protocols must not enable one that cannot.
        assert!(immediate_target_resolvable(ImmediateCapture::ActiveMonitor, true, false));
        assert!(!immediate_target_resolvable(ImmediateCapture::ActiveMonitor, false, true));
    }
}

// DRAGON-304: the pure output resolver the picker-free immediate capture uses when
// `self.outputs` is empty (no overlays minted). Non-Linux only (mirrors the immediate path).
#[cfg(all(test, not(target_os = "linux")))]
mod output_desc_for_selection_tests {
    use super::{output_desc_for_selection_in, Selection};
    use crate::platform::backend::OutputDesc;

    fn desc(name: &str, pos: (i32, i32), size: (i32, i32)) -> OutputDesc {
        OutputDesc { name: name.to_string(), logical_pos: pos, logical_size: size }
    }

    fn monitor_sel(output: Option<&str>, x: i32, y: i32, w: u32, h: u32) -> Selection {
        Selection {
            x,
            y,
            width: w,
            height: h,
            output: output.map(str::to_string),
            window_id: None,
        }
    }

    #[test]
    fn named_monitor_selection_resolves_by_name() {
        let descs = vec![
            desc("A", (0, 0), (1920, 1080)),
            desc("B", (1920, 0), (2560, 1440)),
        ];
        // An `--active-monitor` shot carries the output NAME; it wins regardless of geometry.
        let sel = monitor_sel(Some("B"), 1920, 0, 2560, 1440);
        assert_eq!(output_desc_for_selection_in(&sel, &descs).unwrap().name, "B");
    }

    #[test]
    fn window_selection_resolves_by_centre() {
        let descs = vec![
            desc("A", (0, 0), (1920, 1080)),
            desc("B", (1920, 0), (2560, 1440)),
        ];
        // A window (no output name) at x=2400 sits on B — resolved by its centre.
        let sel = monitor_sel(None, 2200, 200, 400, 300);
        assert_eq!(output_desc_for_selection_in(&sel, &descs).unwrap().name, "B");
    }

    #[test]
    fn offscreen_selection_falls_back_to_primary_then_first() {
        let descs = vec![
            desc("B", (1920, 0), (2560, 1440)),
            desc("A", (0, 0), (1920, 1080)),
        ];
        // A selection off every display prefers the PRIMARY (logical origin).
        let sel = monitor_sel(None, -5000, -5000, 100, 100);
        assert_eq!(output_desc_for_selection_in(&sel, &descs).unwrap().name, "A");
        // With no display at the origin, the first listed wins.
        let no_primary = vec![desc("B", (1920, 0), (2560, 1440))];
        assert_eq!(output_desc_for_selection_in(&sel, &no_primary).unwrap().name, "B");
    }

    #[test]
    fn unknown_name_falls_through_to_geometry() {
        let descs = vec![desc("A", (0, 0), (1920, 1080))];
        // A stale/unknown output name doesn't match, so it resolves by centre (then primary).
        let sel = monitor_sel(Some("GONE"), 100, 100, 200, 200);
        assert_eq!(output_desc_for_selection_in(&sel, &descs).unwrap().name, "A");
    }

    #[test]
    fn no_displays_yields_none() {
        let sel = monitor_sel(Some("A"), 0, 0, 100, 100);
        assert!(output_desc_for_selection_in(&sel, &[]).is_none());
    }
}

// DRAGON-216: the focus-neutral-spinner pre-open decision (Linux overlay only).
#[cfg(all(test, target_os = "linux"))]
mod neutral_spinner_tests {
    use super::window_pick_neutral_spinner_decision as decide;

    // The happy path: immediate window pick, not the defocus sink.
    #[test]
    fn window_pick_pre_opens_neutral() {
        assert!(decide(true, true, false, true));
    }

    // WINDOWED mode also pre-opens the neutral overlay now (DRAGON-216 follow-up): the
    // layer-shell None-interactivity surface is the only focus-safe primitive, so windowed
    // mode covers the grab with it too and swaps it for the real window on completion.
    // (The appearance is decided at `WindowGrabbed`, not here — the decision is mode-blind.)
    #[test]
    fn both_appearances_pre_open_neutral() {
        // Same inputs regardless of preview_windowed — the decision no longer takes it.
        assert!(decide(true, true, false, true));
    }

    // The single-toplevel defocus sink deliberately pre-opens Exclusive to BE the focus
    // sink, so it must NOT be routed through the neutral path.
    #[test]
    fn defocus_sink_is_not_neutral() {
        assert!(!decide(true, true, true, true));
    }

    // A delayed shot (not immediate) never pre-opens; nor does a non-window pick
    // (region/monitor). DRAGON-353: there is no longer a "preview off" miss — the editor
    // always opens.
    #[test]
    fn other_misses_defer() {
        assert!(!decide(false, true, false, true)); // delayed
        assert!(!decide(true, false, false, true)); // region/monitor
    }

    /// DRAGON-428: a `--no-editor` launch opens no editor, so there is nothing for the
    /// spinner to cover. Minting it anyway would flash an overlay on screen and tear it
    /// down again for a capture that went straight to the clipboard.
    #[test]
    fn a_no_editor_launch_never_pre_opens() {
        assert!(!decide(true, true, false, false));
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

    // Delayed / non-window both defer (DRAGON-353: "preview off" no longer exists).
    #[test]
    fn other_misses_defer() {
        assert!(!decide(false, true, true)); // delayed
        assert!(!decide(true, false, true)); // region/monitor
    }

    /// DRAGON-428: same rule as the Linux neutral spinner — no editor coming, no pre-open.
    #[test]
    fn a_no_editor_launch_never_pre_opens() {
        assert!(!decide(true, true, false));
    }
}

#[cfg(all(test, windows))]
mod windows_preopen_tests {
    use super::windows_window_pick_preopens as decide;

    // DRAGON-305: a WINDOWED immediate window pick with preview on pre-opens the fullscreen
    // blocker cover.
    #[test]
    fn windowed_window_pick_pre_opens() {
        assert!(decide(true, true, true, true));
    }

    // Overlay-preview mode (windowed = false) does NOT pre-open — it already shows the fullscreen
    // blocker after the grab.
    #[test]
    fn overlay_mode_defers() {
        assert!(!decide(true, true, false, true));
    }

    // The base misses (delayed / non-window) still defer even in windowed mode.
    #[test]
    fn base_misses_defer_even_when_windowed() {
        assert!(!decide(false, true, true, true)); // delayed
        assert!(!decide(true, false, true, true)); // region/monitor
    }

    /// DRAGON-428: no editor coming, so no blocker cover — even in windowed mode, which is
    /// the one shape that otherwise always pre-opens.
    #[test]
    fn a_no_editor_launch_never_pre_opens() {
        assert!(!decide(true, true, true, false));
    }
}

#[cfg(all(test, windows))]
mod windows_backdrop_kind_tests {
    use super::window_backdrop_kind as kind;

    // DRAGON-308: a transparency-preserving (glass) grab floats the WALLPAPER backdrop when
    // "preserve wallpaper" is on, and BLACK when it is off (so the glass never leaks other
    // windows either way).
    #[test]
    fn glass_grab_floats_wallpaper_or_black() {
        assert_eq!(kind(true, true, false), Some(true)); // glass + wallpaper → wallpaper
        assert_eq!(kind(true, false, false), Some(false)); // glass + no wallpaper → black
    }

    // An OPAQUE (transparency-off) grab renders the window in isolation — no glass to fix — so
    // it floats nothing, regardless of the wallpaper setting.
    #[test]
    fn opaque_grab_floats_nothing() {
        assert_eq!(kind(false, true, false), None);
        assert_eq!(kind(false, false, false), None);
    }

    // DRAGON-426: a FULLSCREEN window floats a backdrop too. It used to float nothing, on the
    // reasoning that a fullscreen window has nothing behind it — true when it is opaque, false
    // when it is translucent, and the translucent case is the one with a whole desktop's worth
    // of other windows to show through. The backdrop is the precondition for keeping
    // transparency at all now, so it must not be suppressed by a size.
    #[test]
    fn fullscreen_still_floats_a_backdrop() {
        assert_eq!(kind(true, true, true), Some(true));
        assert_eq!(kind(true, false, true), Some(false));
    }

    // An opaque grab floats nothing whatever its size — there is no transparency to protect.
    #[test]
    fn opaque_fullscreen_still_floats_nothing() {
        assert_eq!(kind(false, true, true), None);
        assert_eq!(kind(false, false, true), None);
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
        assert!(freeze_backdrop_active(true, false, 0));
    }

    // NATIVE freeze with a countdown armed: release the backdrop so selection previews
    // the LIVE screen the delayed shot will grab (the DRAGON-186 Phase 4 fix). Any
    // nonzero delay releases it.
    #[test]
    fn freeze_with_armed_countdown_releases_backdrop() {
        assert!(!freeze_backdrop_active(true, false, 3));
        assert!(!freeze_backdrop_active(true, false, 5));
        assert!(!freeze_backdrop_active(true, false, 10));
        assert!(!freeze_backdrop_active(true, false, 1));
    }

    // Freeze off (e.g. video mode, or freeze disabled) with no fallback term: never a
    // backdrop regardless of the delay, matching how video mode already releases it.
    #[test]
    fn freeze_off_never_shows_backdrop() {
        assert!(!freeze_backdrop_active(false, false, 0));
        assert!(!freeze_backdrop_active(false, false, 5));
    }

    // DRAGON-547 reopened: the delay release splits BY PATH. The native term releases
    // under a countdown (a transparent layer-shell overlay shows the live desktop the
    // delayed shot will grab); the FALLBACK term stays frozen whatever the delay,
    // because a plain toplevel has no live desktop behind it (released = flat gray,
    // the owner's seventh live test) and a delayed fallback capture re-grabs at fire
    // time, so the seed still misleads about nothing. The four-way pin:
    #[test]
    fn the_delay_release_splits_by_path() {
        // Native + no delay: frozen when freezing.
        assert!(freeze_backdrop_active(true, false, 0));
        // Native + delay: released (DRAGON-186 Phase 4, byte-identical).
        assert!(!freeze_backdrop_active(true, false, 5));
        // Fallback + no delay: frozen.
        assert!(freeze_backdrop_active(false, true, 0));
        // Fallback + delay: STILL frozen. The fix, for any nonzero delay.
        assert!(freeze_backdrop_active(false, true, 1));
        assert!(freeze_backdrop_active(false, true, 5));
        assert!(freeze_backdrop_active(false, true, 10));
    }
}

#[cfg(test)]
mod scanner_backdrop_tests {
    use super::freeze_backdrop_active;

    /// DRAGON-460: the scanner has NO backdrop rule of its own any more.
    ///
    /// DRAGON-456's four tests here pinned `scanner_backdrop_active` — that the scanner
    /// froze the view even with the freeze preference off, so that what was shown matched
    /// what was read. Reading a live region shot satisfies that property directly, so the
    /// function is gone and the tests with it.
    ///
    /// What must NOT come back is a scanner arm inside `freeze_backdrop_active`: that is
    /// what showed the user a launch-instant still with our own toolbar photographed into
    /// it. The freeze PREFERENCE still governs the scanner exactly as it governs any other
    /// kind, which is all this checks.
    #[test]
    fn the_scanner_has_no_backdrop_rule_of_its_own() {
        // Freeze off = live view, whatever the kind. No scanner exception.
        assert!(!freeze_backdrop_active(false, false, 0));
        // Freeze on, no delay = frozen, again with no kind involved in the decision.
        assert!(freeze_backdrop_active(true, false, 0));
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
        assert_eq!(frozen_region_source(false, false), FrozenRegionSource::WindowsOnly);
    }

    // Wallpaper ON uses the flat snapshot (the wallpaper is the wanted background).
    #[test]
    fn wallpaper_on_is_flat_snapshot() {
        assert_eq!(frozen_region_source(true, false), FrozenRegionSource::FlatSnapshot);
    }

    // `lab/flatpak`: a portal-seeded frozen scene is ONE finished frame with no
    // per-window pixels to composite, so the flat snapshot is the only honest source whatever the
    // wallpaper preference says. Without this override, wallpaper-OFF would build the
    // empty windows-only canvas and deliver a black rectangle.
    #[test]
    fn a_portal_seeded_scene_is_always_the_flat_snapshot() {
        assert_eq!(frozen_region_source(false, true), FrozenRegionSource::FlatSnapshot);
        assert_eq!(frozen_region_source(true, true), FrozenRegionSource::FlatSnapshot);
    }
}

#[cfg(test)]
mod fallback_backdrop_tests {
    use super::fallback_backdrop;

    // The owner's live report: switching the fallback overlay to VIDEO kind showed a
    // flat gray screen, because the backdrop keyed on the still kinds. The decision
    // deliberately has NO kind input, so the seed still shows for region selection of
    // every kind; this test documents that absence rather than enumerating kinds.
    // The DELAY is not an input either (DRAGON-547 reopened): the by-path delay split
    // lives in `freeze_backdrop_active`, whose four-way pin sits in
    // `freeze_backdrop_tests::the_delay_release_splits_by_path`.
    #[test]
    fn the_backdrop_ignores_the_capture_kind_by_construction() {
        assert!(fallback_backdrop(true, true, true));
    }

    // Off region mode there is no selection to back (the portal picker owns
    // monitor/window picks), and without the seed frame there is nothing to draw.
    #[test]
    fn only_region_mode_with_a_seed_frame_draws() {
        assert!(!fallback_backdrop(true, false, true));
        assert!(!fallback_backdrop(true, true, false));
    }

    // A normal session never takes this term; its backdrop stays `freezing()`'s
    // preference-and-caps decision alone.
    #[test]
    fn a_normal_session_never_takes_the_fallback_term() {
        assert!(!fallback_backdrop(false, true, true));
    }
}

#[cfg(test)]
mod keep_countdown_anchor_tests {
    use super::keep_countdown_anchor;

    // DRAGON-563: every fallback countdown (tray or not, since the reopened ticket
    // removed the window-countdown degrade) tears the outputs down at countdown start,
    // so at fire time the fresh resolution is empty and the countdown-start snapshot is
    // the only anchor left. This is the one combination that keeps it.
    #[test]
    fn an_empty_fresh_answer_keeps_the_snapshot_on_the_fallback_path() {
        assert!(keep_countdown_anchor(true, false, true));
    }

    // A fresh answer always wins, and with no snapshot there is nothing to keep.
    #[test]
    fn a_fresh_answer_or_a_missing_snapshot_takes_the_overwrite() {
        assert!(!keep_countdown_anchor(true, true, true));
        assert!(!keep_countdown_anchor(true, false, false));
    }

    // A normal session never keeps anything: the historical overwrite stands whatever
    // the other two terms claim, which is the byte-identity guarantee.
    #[test]
    fn a_normal_session_always_overwrites() {
        for fresh in [false, true] {
            for snapshot in [false, true] {
                assert!(!keep_countdown_anchor(false, fresh, snapshot));
            }
        }
    }
}

#[cfg(test)]
mod countdown_consumed_tests {
    use super::countdown_consumed;

    // The owner's one-shot rule: a fired countdown driven by the persisted preset is
    // spent, whatever the preset was.
    #[test]
    fn a_fired_preset_countdown_is_spent() {
        assert!(countdown_consumed(true, false, 1));
        assert!(countdown_consumed(true, false, 2));
        assert!(countdown_consumed(true, false, 3));
    }

    // A cancelled countdown keeps the setting: the user did not get their capture, so
    // the configured timer is still owed to the retry. (Owner-vetoable choice; the
    // round's report flags it.)
    #[test]
    fn a_cancelled_countdown_keeps_the_setting() {
        assert!(!countdown_consumed(false, false, 2));
        assert!(!countdown_consumed(false, true, 2));
    }

    // A CLI --countdown override never spends the persisted preset it did not read,
    // and a zero preset has nothing to spend (no pointless config write).
    #[test]
    fn overrides_and_zero_presets_spend_nothing() {
        assert!(!countdown_consumed(true, true, 2));
        assert!(!countdown_consumed(true, false, 0));
        assert!(!countdown_consumed(true, true, 0));
    }
}

#[cfg(test)]
mod fallback_still_tests {
    use super::fallback_still_from_frozen;

    // `lab/flatpak` WYSIWYG: on the fallback overlay, a non-delayed region still crops
    // the exact still the user drew their selection over.
    #[test]
    fn a_non_delayed_fallback_still_crops_the_seed_frame() {
        assert!(fallback_still_from_frozen(true, 0, true));
    }

    // A configured delay flips to the fresh per-capture portal frame: the delay exists
    // to change the screen, so the frozen still is the WRONG content by definition.
    #[test]
    fn a_delay_takes_the_fresh_portal_frame() {
        assert!(!fallback_still_from_frozen(true, 3, true));
    }

    // No seed frame landed (the grab failed a moment before commit, or the scene was
    // already released): nothing to crop, so the portal request stays the source.
    #[test]
    fn no_seed_frame_means_no_frozen_crop() {
        assert!(!fallback_still_from_frozen(true, 0, false));
    }

    // Off the fallback the answer is always false: the normal portal region-still
    // request is byte-identical, frozen scene or not.
    #[test]
    fn a_normal_session_never_takes_the_frozen_crop() {
        assert!(!fallback_still_from_frozen(false, 0, true));
        assert!(!fallback_still_from_frozen(false, 5, false));
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

/// DRAGON-562: the fullscreen bare-frame gate as the PORTAL window-still path
/// consumes it. These pin the CONSUMPTION — the grant-fact plumbing into the one
/// shared `is_fullscreen` rule — not the geometry itself, which
/// `fullscreen_tests` above already owns.
#[cfg(test)]
mod portal_window_fullscreen_tests {
    use super::portal_window_fullscreen;

    /// DRAGON-593, the owner's report: a fullscreen game came back padded and bordered.
    /// cosmic-comp's portal sends `position: None` for EVERY window stream, and the rule
    /// used to require both a position and an origin, so the guard tripped every time and
    /// the fullscreen rule could never fire on the portal path at all. Size alone answers
    /// it, because the origin output is already known.
    #[test]
    fn a_positionless_grant_still_recognises_a_fullscreen_window() {
        const OUT: Option<(i32, i32, i32, i32)> = Some((0, 0, 5120, 1440));
        // The owner's ultrawide, a window covering it, no position from the portal.
        assert!(portal_window_fullscreen(true, None, OUT, (5120, 1440), 1.0));
        // A HiDPI grant of the same window: physical pixels over the buffer scale.
        assert!(portal_window_fullscreen(true, None, OUT, (10240, 2880), 2.0));
        // A window that does NOT cover the output is still decorated, which is the whole
        // point of the rule: a maximised window is a window.
        assert!(!portal_window_fullscreen(true, None, OUT, (5120, 1400), 1.0));
        assert!(!portal_window_fullscreen(true, None, OUT, (2560, 1440), 1.0));
        // Without an origin there is nothing to compare against, so no claim is made.
        assert!(!portal_window_fullscreen(true, None, None, (5120, 1440), 1.0));
        // The extra stays the master switch: unaware means never bare.
        assert!(!portal_window_fullscreen(false, None, OUT, (5120, 1440), 1.0));
    }


    const OUT: Option<(i32, i32, i32, i32)> = Some((0, 0, 1920, 1080));

    // A window grant exactly filling its output at 1x scale is fullscreen: the
    // bare frame, no decoration.
    #[test]
    fn exact_fill_at_1x_is_fullscreen() {
        assert!(portal_window_fullscreen(true, Some((0, 0)), OUT, (1920, 1080), 1.0));
    }

    // On a 2x output the stream is physical pixels; the scale maps it back into
    // the logical space the output rect lives in.
    #[test]
    fn scaled_output_maps_physical_to_logical() {
        assert!(portal_window_fullscreen(true, Some((0, 0)), OUT, (3840, 2160), 2.0));
        // The same physical frame WITHOUT the scale applied would look 2x the
        // output and must not read as fullscreen.
        assert!(!portal_window_fullscreen(true, Some((0, 0)), OUT, (3840, 2160), 1.0));
    }

    // A second monitor at a global offset: the grant position carries the offset.
    #[test]
    fn offset_output_fills_by_its_own_origin() {
        let out = Some((1920, 0, 2560, 1440));
        assert!(portal_window_fullscreen(true, Some((1920, 0)), out, (2560, 1440), 1.0));
        assert!(!portal_window_fullscreen(true, Some((0, 0)), out, (2560, 1440), 1.0));
    }

    // A maximized-but-decorated window (a titlebar's worth short) is decorated —
    // the same verdict the native gate gives the same geometry.
    #[test]
    fn maximized_with_titlebar_is_decorated() {
        assert!(!portal_window_fullscreen(true, Some((0, 32)), OUT, (1920, 1048), 1.0));
    }

    // A missing ORIGIN answers false (decorate): with no output to compare against there
    // is nothing to be fullscreen ON, so no claim is made.
    //
    // DRAGON-593: this test used to assert that a missing POSITION also meant decorate,
    // which encoded the bug as if it were the intent. cosmic-comp sends `position: None`
    // for every window stream, so that arm was not an edge case at all, it was every
    // portal window capture, and it is why a fullscreen game came back padded and
    // bordered. Size against the known origin answers it without a position.
    #[test]
    fn a_missing_origin_means_decorated() {
        assert!(!portal_window_fullscreen(true, Some((0, 0)), None, (1920, 1080), 1.0));
        assert!(!portal_window_fullscreen(true, None, None, (1920, 1080), 1.0));
    }

    // The capability gate is ANDed first, exactly like the native branch's
    // `extras.fullscreen_aware &&`.
    #[test]
    fn capability_off_means_decorated() {
        assert!(!portal_window_fullscreen(false, Some((0, 0)), OUT, (1920, 1080), 1.0));
    }

    // A degenerate scale falls back to 1.0 instead of dividing by zero.
    #[test]
    fn degenerate_scale_falls_back_to_1x() {
        assert!(portal_window_fullscreen(true, Some((0, 0)), OUT, (1920, 1080), 0.0));
        assert!(portal_window_fullscreen(true, Some((0, 0)), OUT, (1920, 1080), -1.0));
    }
}

/// DRAGON-562 fix round: the synthetic wallpaper anchor for a position-less
/// window grant (COSMIC's portal sends `position: None` for every window
/// stream, so this is the NORMAL portal window still, not an edge case).
#[cfg(test)]
mod synthetic_window_anchor_tests {
    use super::synthetic_window_anchor;

    fn out(name: &str, pos: (i32, i32), size: (i32, i32)) -> (String, (i32, i32), (i32, i32)) {
        (name.to_string(), pos, size)
    }

    // The largest output wins regardless of registration order — the fifth
    // test's desktop registers an 800x480 side panel FIRST, and anchoring
    // there was the exact mistake DRAGON-563 corrected for the preview.
    #[test]
    fn largest_output_wins_over_registration_order() {
        let outs = [out("panel", (5120, 0), (800, 480)), out("ultrawide", (0, 0), (5120, 1440))];
        // 1194x962 centered on the ultrawide.
        assert_eq!(
            synthetic_window_anchor(&outs, (1194, 962)),
            Some(((5120 - 1194) / 2, (1440 - 962) / 2))
        );
    }

    // The anchor lives in the chosen output's own global space.
    #[test]
    fn offset_output_centers_in_global_coordinates() {
        let outs = [out("right", (1920, 200), (2560, 1440))];
        assert_eq!(
            synthetic_window_anchor(&outs, (560, 440)),
            Some((1920 + 1000, 200 + 500))
        );
    }

    // A frame larger than the output clamps to the output's top-left, keeping
    // the frame's center on-output (the composite resolves the wallpaper by
    // center containment).
    #[test]
    fn oversize_frame_clamps_to_the_output_origin() {
        let outs = [out("small", (100, 50), (800, 480))];
        assert_eq!(synthetic_window_anchor(&outs, (1200, 900)), Some((100, 50)));
    }

    // Ties keep the FIRST registered of the equal-area outputs (stable, and
    // pinned so a refactor to `max_by_key` — last-wins on ties — is caught).
    #[test]
    fn equal_areas_keep_the_first() {
        let outs = [out("a", (0, 0), (1920, 1080)), out("b", (1920, 0), (1920, 1080))];
        assert_eq!(synthetic_window_anchor(&outs, (400, 300)), Some((760, 390)));
    }

    // No outputs, or only degenerate ones: no anchor — the caller keeps the
    // honest black fallback.
    #[test]
    fn no_usable_output_means_no_anchor() {
        assert_eq!(synthetic_window_anchor(&[], (400, 300)), None);
        let outs = [out("dead", (0, 0), (0, 1080)), out("gone", (0, 0), (1920, 0))];
        assert_eq!(synthetic_window_anchor(&outs, (400, 300)), None);
    }
}

/// The WHICH-output half of the synthetic anchor (DRAGON-549 reopened), shared by
/// the wallpaper compose and the portal window-grant preview-origin resolution.
#[cfg(test)]
mod largest_output_index_tests {
    use super::largest_output_index;

    /// The owner's desktop, in registration order: the 800x480 side panel registers
    /// FIRST, the 5120x1440 ultrawide second. The decision must name the ultrawide;
    /// `outputs.first()` naming the panel is the exact defect the sixth live test
    /// logged (`monitor=(800, 480)pt` for every window capture).
    #[test]
    fn the_ultrawide_beats_the_first_registered_panel() {
        assert_eq!(largest_output_index(&[(800, 480), (5120, 1440)]), Some(1));
        // And the same answer when registration order flips.
        assert_eq!(largest_output_index(&[(5120, 1440), (800, 480)]), Some(0));
    }

    // Ties keep the FIRST registered (stable, matching the anchor's rule).
    #[test]
    fn equal_areas_keep_the_first() {
        assert_eq!(largest_output_index(&[(1920, 1080), (1920, 1080)]), Some(0));
    }

    // No outputs, or only degenerate ones: no answer, and the caller keeps its
    // existing fallback.
    #[test]
    fn no_positive_area_means_no_index() {
        assert_eq!(largest_output_index(&[]), None);
        assert_eq!(largest_output_index(&[(0, 1080), (1920, 0), (-5, -5)]), None);
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

#[cfg(test)]
mod capture_dir_tests {
    use super::capture_write_dir;
    use std::path::{Path, PathBuf};

    /// THE "handled through /tmp" rule (DRAGON-467): "Automatically save originals" picks
    /// between the user's configured folder and the session runtime directory, and picks
    /// nothing else. Modelled on the Windows 11 Snipping Tool's toggle of the same name.
    #[test]
    fn save_originals_chooses_between_the_save_folder_and_the_runtime_dir() {
        let configured = Path::new("/home/me/Capture");
        let transient = Path::new("/run/user/1000");
        assert_eq!(capture_write_dir(true, configured, transient), PathBuf::from(configured));
        assert_eq!(capture_write_dir(false, configured, transient), PathBuf::from(transient));
    }

    /// The NAME never moves — only the folder. This is what keeps a transient capture
    /// recognisable, and what lets `naming::save_prefill` reunite it with the configured
    /// folder when the user finally saves.
    #[test]
    fn only_the_folder_moves_never_the_file_name() {
        let configured = Path::new("/home/me/Capture");
        let transient = Path::new("/run/user/1000");
        let name = super::still_save_name("Screenshot 2026-07-29");
        for save_originals in [true, false] {
            let path = capture_write_dir(save_originals, configured, transient).join(&name);
            assert_eq!(
                path.file_name().and_then(|n| n.to_str()),
                Some(name.as_str()),
                "save_originals={save_originals}"
            );
        }
        // And the two really are different places, or the setting would do nothing.
        assert_ne!(
            capture_write_dir(true, configured, transient),
            capture_write_dir(false, configured, transient)
        );
    }

    /// Recordings take the same fork against their own container, so a `.mp4` in the
    /// transient folder is still a `.mp4` when the user saves it out.
    #[test]
    fn recordings_take_the_same_fork() {
        let name = super::recording_save_name("Recording 2026-07-29");
        let path =
            capture_write_dir(false, Path::new("/home/me/Videos"), Path::new("/var/cache/cck"))
                .join(&name);
        assert_eq!(path, PathBuf::from("/var/cache/cck/Recording 2026-07-29.mp4"));
    }

    /// THE MEDIUM SPLIT (DRAGON-467 review, major 3): an unsaved RECORDING must not buffer
    /// into `$XDG_RUNTIME_DIR`, which is a tmpfs sized at ~10% of RAM. A take writes its live
    /// `.recording` temp AND its finished file there, so a long one could ENOSPC in the
    /// middle of capturing, losing the take it was recording.
    ///
    /// Stills deliberately stay in the runtime dir: a few MB written once is exactly what it
    /// is for, and its clear-at-logout lifetime is the right one for them.
    #[test]
    fn recordings_and_stills_have_different_transient_homes() {
        let runtime = PathBuf::from(crate::util::runtime_dir());
        assert_eq!(super::transient_dir(false), runtime, "a still stays in the runtime dir");
        let video = super::transient_dir(true);
        assert_ne!(
            video, runtime,
            "an unsaved recording must not buffer into the RAM-backed runtime dir"
        );
        // Whatever it resolved to, it EXISTS: the write site joins a file name straight onto
        // it, so a folder that was never created would fail the capture rather than the copy.
        assert!(video.is_dir(), "the transient recording folder is created on demand");
    }

    /// The sweep is safe to run against a live folder: it removes nothing that is young, and
    /// it never touches subdirectories. Driven for real, because the risk here is deleting a
    /// user's capture, and a value-only test would not exercise the filesystem walk at all.
    #[test]
    fn the_transient_sweep_keeps_recent_files() {
        let Some(dir) = crate::util::transient_recording_dir() else { return };
        let keep = dir.join("cck-sweep-test-recent.mp4");
        std::fs::write(&keep, b"x").expect("the transient dir must be writable");
        crate::util::sweep_transient_recordings();
        assert!(keep.exists(), "a file written moments ago must survive the sweep");
        let _ = std::fs::remove_file(&keep);
        // And the age bound is a WEEK, not something incidental — the constant is the
        // promise the setting's description makes to the user.
        assert_eq!(
            crate::util::TRANSIENT_MAX_AGE,
            std::time::Duration::from_secs(7 * 24 * 60 * 60)
        );
    }
}

/// DRAGON-599: the walls a keyboard region nudge stops at.
#[cfg(test)]
mod desktop_bounds_tests {
    use super::desktop_bounds_of;

    /// One display is its own rect, right/bottom EXCLUSIVE (position plus size), which is the
    /// same convention `GlobalRect` uses, so a region flush against the far edge reads as
    /// flush rather than one pixel over.
    #[test]
    fn a_single_output_is_its_own_rect() {
        assert_eq!(
            desktop_bounds_of([((0, 0), (1920, 1080))]),
            Some((0, 0, 1920, 1080))
        );
    }

    /// Side-by-side displays make one box wide enough for a region that spans both, which is
    /// the case this exists for: a dragged region can already cross a monitor border, so a
    /// nudge must be able to move it there.
    #[test]
    fn two_displays_make_one_box_wide_enough_for_both() {
        assert_eq!(
            desktop_bounds_of([((0, 0), (5120, 1440)), ((5120, 960), (800, 480))]),
            Some((0, 0, 5920, 1440)),
            "the owner's own layout"
        );
    }

    /// A display at a NEGATIVE origin (one placed left of, or above, the primary) moves the
    /// box's origin with it. Walling at zero would strand a region on that monitor.
    #[test]
    fn a_negative_origin_moves_the_box() {
        assert_eq!(
            desktop_bounds_of([((0, 0), (1920, 1080)), ((-1280, -200), (1280, 1024))]),
            Some((-1280, -200, 1920, 1080))
        );
    }

    /// A zero-sized output contributes nothing. Folding its ORIGIN in anyway would drag the
    /// box out to a place no pixel exists, and every region on the real display would then be
    /// able to walk into nothing.
    #[test]
    fn a_zero_sized_output_is_skipped() {
        assert_eq!(
            desktop_bounds_of([((0, 0), (1920, 1080)), ((9000, 9000), (0, 0))]),
            Some((0, 0, 1920, 1080))
        );
        assert_eq!(desktop_bounds_of([((5, 5), (1920, 0))]), None);
    }

    /// No outputs, no walls, and the caller must handle it: the nudge arm declines rather than
    /// inventing a desktop.
    #[test]
    fn no_outputs_have_no_bounds() {
        assert_eq!(desktop_bounds_of([]), None);
    }
}

/// DRAGON-600: the tray-menu dropdown hold. These pin the ORDERING rule, which is the
/// whole fix: the dismissal is caused by this process, so the wait has to sit after the
/// cause, not before it.
#[cfg(test)]
mod menu_flats_hold_tests {
    use super::*;

    /// Only a menu launch that grabs flats holds. The other three combinations are the
    /// launches that must pay nothing: a PrintScreen capture, and any launch with no
    /// flats to protect.
    #[test]
    fn only_a_menu_launch_that_grabs_flats_holds() {
        assert!(menu_flats_hold_needed(true, true));
        assert!(!menu_flats_hold_needed(true, false));
        assert!(!menu_flats_hold_needed(false, true));
        assert!(!menu_flats_hold_needed(false, false));
    }

    /// Before focus there is nothing to settle from, so no amount of elapsed time short of
    /// the outer budget releases the hold. This is the property that makes the mechanism a
    /// SIGNAL rather than a timer: waiting alone never satisfies it.
    #[test]
    fn without_focus_only_the_outer_budget_releases() {
        assert!(!menu_hold_release(None, 0));
        assert!(!menu_hold_release(None, MENU_DISMISS_SETTLE_MS));
        assert!(!menu_hold_release(None, MENU_HOLD_BUDGET_MS - 1));
        assert!(menu_hold_release(None, MENU_HOLD_BUDGET_MS));
        assert!(menu_hold_release(None, MENU_HOLD_BUDGET_MS + 5_000));
    }

    /// Once focus lands, the settle is counted from THAT instant, not from launch.
    #[test]
    fn focus_starts_the_settle_and_the_settle_releases() {
        assert!(!menu_hold_release(Some(0), 0));
        assert!(!menu_hold_release(Some(MENU_DISMISS_SETTLE_MS - 1), 0));
        assert!(menu_hold_release(Some(MENU_DISMISS_SETTLE_MS), 0));
    }

    /// A late focus still gets its full settle, right up until the outer budget takes
    /// over. Pinned because the tempting simplification, releasing on focus alone, would
    /// grab while the panel is still tearing the popup down.
    #[test]
    fn a_late_focus_still_gets_its_settle_until_the_budget_wins() {
        let nearly = MENU_HOLD_BUDGET_MS - 1;
        assert!(!menu_hold_release(Some(0), nearly));
        assert!(menu_hold_release(Some(0), MENU_HOLD_BUDGET_MS));
    }

    /// The two bounds, pinned to the values a reader would otherwise have to go and look
    /// up. Their RELATION is the compile-time assert beside the constants, so it is not
    /// restated here; what this adds is that both are finite and named, which is the
    /// DRAGON-118 rule applied to the launch path.
    #[test]
    fn the_hold_is_bounded_at_both_ends() {
        assert_eq!(MENU_DISMISS_SETTLE_MS, 150);
        assert_eq!(MENU_HOLD_BUDGET_MS, 1200);
    }
}
