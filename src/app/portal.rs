use super::*;

impl App {
    /// Whether THIS session's compositor offers native screencopy at all
    /// (ext-image-copy-capture + output sources).
    #[cfg(target_os = "linux")]
    fn native_capture_available(&self) -> bool {
        let p = crate::platform::backend::wayland_protocols();
        p.image_copy_capture && p.output_source
    }

    /// macOS/Windows: no Wayland screencopy — capture is SCK/WGC. The portal path
    /// (which this gates, alongside `pipewire_available`) doesn't exist here.
    #[cfg(not(target_os = "linux"))]
    fn native_capture_available(&self) -> bool {
        false
    }

    /// Whether the SAVED screenshot-method choice (a `platform::backend` id),
    /// clamped to this session, lands on the portal: a preference for native
    /// screencopy can't apply on a compositor without it. Multi-session installs
    /// (COSMIC + GNOME/KDE logins sharing one config) hit this — a choice saved
    /// under COSMIC must not break captures after a GNOME login. Session-scoped;
    /// the persisted setting itself is never rewritten.
    pub(super) fn screenshot_uses_portal(&self) -> bool {
        self.screenshot_backend == crate::platform::backend::PORTAL_ID
            || !self.native_capture_available()
    }

    /// The saved recording-method choice, clamped the same way.
    pub(super) fn recording_uses_portal(&self) -> bool {
        self.record_backend == crate::platform::backend::PORTAL_ID
            || !self.native_capture_available()
    }

    /// The ACTIVE screenshot backend's capabilities (DRAGON-186). "Active" is the
    /// backend a still capture would actually run on right now: the portal when the
    /// session-clamped choice lands there AND the portal is reachable (the same
    /// pairing every portal dispatch gate uses), otherwise native screencopy —
    /// including the portal-unreachable fallback, which really does grab through
    /// the compositor.
    #[cfg(target_os = "linux")]
    pub(super) fn active_screenshot_caps(&self) -> crate::platform::backend::Caps {
        use crate::platform::backend::{
            wayland_protocols, CaptureBackend, PortalBackend, ScreencopyBackend,
        };
        if self.screenshot_uses_portal() && self.pipewire_available {
            PortalBackend {
                available: self.pipewire_available,
                ffmpeg: self.ffmpeg_available,
            }
            .caps()
        } else {
            ScreencopyBackend { protocols: wayland_protocols(), ffmpeg: self.ffmpeg_available }
                .caps()
        }
    }

    /// macOS: ScreenCaptureKit is the only backend, so it is always the active one.
    #[cfg(target_os = "macos")]
    pub(super) fn active_screenshot_caps(&self) -> crate::platform::backend::Caps {
        use crate::platform::backend::CaptureBackend;
        crate::platform::backend::MacBackend { ffmpeg: self.ffmpeg_available }.caps()
    }

    /// The persisted capture-extra preferences as one set (DRAGON-186).
    /// `fullscreen_aware` has no user toggle (a behavior capability), so it
    /// passes through as `true` and the capability alone decides.
    fn capture_extra_prefs(&self) -> crate::platform::backend::CaptureExtras {
        crate::platform::backend::CaptureExtras {
            freeze: self.freeze,
            cursor: self.capture_cursor,
            transparency: self.capture_transparency,
            wallpaper: self.capture_wallpaper,
            fullscreen_aware: true,
        }
    }

    /// The extras a capture actually applies (DRAGON-186): each persisted
    /// preference gated by the active backend's capability, so a stale "on"
    /// persisted under a supporting backend can't leak onto one (the portal)
    /// that can't honor it. The preferences themselves are never rewritten.
    pub(super) fn effective_capture_extras(&self) -> crate::platform::backend::CaptureExtras {
        self.active_screenshot_caps().capture_extras().and(self.capture_extra_prefs())
    }

    /// Whether the chosen capture source for the current kind is the PipeWire
    /// portal (video recording, or PipeWire screenshots) and it's reachable. Drives
    /// whether monitor/window go through the portal picker.
    pub(super) fn mode_uses_portal(&self) -> bool {
        self.pipewire_available
            && ((self.kind == Kind::Video && self.recording_uses_portal())
                || (matches!(self.kind, Kind::Image | Kind::Scanner)
                    && self.screenshot_uses_portal()))
    }

    /// macOS/Windows: no xdg portal picker (capture is SCK/WGC). `mode_uses_portal`
    /// is always false here, so this is never reached — an inert `None` keeps the seam.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn portal_for_mode(&mut self, _m: Mode) -> Option<Task<cosmic::Action<Msg>>> {
        None
    }

    /// Launch the portal picker for a monitor/window mode (PipeWire source), so it
    /// replaces the native picker. `None` for region (which keeps its draw + the
    /// at-commit portal request). Used by the mode/kind toggles.
    #[cfg(target_os = "linux")]
    pub(super) fn portal_for_mode(&mut self, m: Mode) -> Option<Task<cosmic::Action<Msg>>> {
        use ashpd::desktop::screencast::SourceType;
        let (source, sel) = match m {
            Mode::Monitor => (SourceType::Monitor, self.monitor_placeholder_sel()),
            Mode::Window => (
                SourceType::Window,
                Selection {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                    output: None,
                    window_id: None,
                },
            ),
            Mode::Region => return None,
        };
        Some(self.request_pipewire(source, None, sel))
    }

    /// A placeholder whole-monitor selection (the first output) for naming + the
    /// recording overlay anchor when the portal — not our overlay — picks the source.
    /// Linux-only: consumed by the Linux `portal_for_mode` / PipeWire cast path.
    #[cfg(target_os = "linux")]
    fn monitor_placeholder_sel(&self) -> Selection {
        match self.outputs.first() {
            Some(o) => Selection {
                x: o.logical_pos.0,
                y: o.logical_pos.1,
                width: o.logical_size.0,
                height: o.logical_size.1,
                output: Some(o.name.clone()),
                window_id: None,
            },
            None => Selection {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                output: Some("monitor".into()),
                window_id: None,
            },
        }
    }

    /// Yield the overlay and fire the async ScreenCast request for `source` (so the
    /// portal dialog — a normal window — isn't hidden behind our Overlay layer). The
    /// result lands in `pw_slot` and is handled by [`Self::on_pipewire_cast_ready`].
    #[cfg(target_os = "linux")]
    pub(super) fn request_pipewire(
        &mut self,
        source: ashpd::desktop::screencast::SourceType,
        region: Option<RegionTarget>,
        sel: Selection,
    ) -> Task<cosmic::Action<Msg>> {
        if self.pw_pending.is_some() {
            return Task::none(); // a request is already in flight
        }
        self.pw_pending = Some(PwPending { sel, region });
        let mut cmds = self.yield_overlays();
        let slot = self.pw_slot.clone();
        let token = self.pw_restore_token.clone();
        cmds.push(Task::perform(
            async move {
                let r = crate::platform::screencast::request(source, token).await;
                if let Ok(mut g) = slot.lock() {
                    *g = Some(r);
                }
            },
            |()| cosmic::Action::App(Msg::Capture(CaptureMsg::PipewireCastReady)),
        ));
        Task::batch(cmds)
    }

    /// Process the async ScreenCast result: hold the granted stream and continue the
    /// capture (countdown then record), fall back to screencopy when the portal is
    /// unavailable, or — on cancel / a region wrong-monitor pick — return to the
    /// selector (with a toast for the wrong monitor).
    pub(super) fn on_pipewire_cast_ready(&mut self) -> Task<cosmic::Action<Msg>> {
        let result = self.pw_slot.lock().ok().and_then(|mut g| g.take());
        let Some(pending) = self.pw_pending.take() else {
            return self.restore_interactive_overlays();
        };
        let session = match result {
            Some(Ok(s)) => s,
            // User cancelled / no result: go back to region select, clear the token
            // so the next attempt re-prompts.
            Some(Err(crate::platform::screencast::CastError::Cancelled)) | None => {
                self.pw_restore_token = None;
                self.mode = Mode::Region;
                self.save_state();
                return self.restore_interactive_overlays();
            }
            Some(Err(crate::platform::screencast::CastError::Unavailable(e))) => {
                log::warn!("portal unavailable ({e}); falling back to screencopy");
                return self.proceed_capture(pending.sel);
            }
        };
        self.pw_restore_token = session.restore_token.clone();
        self.save_state();
        let Some(stream) = session.streams.into_iter().next() else {
            return self.proceed_capture(pending.sel);
        };

        // Region: the granted monitor must be the one the region was clamped to;
        // otherwise return to draw with a message. Then crop to the region.
        let crop = match &pending.region {
            Some(rt) => {
                if stream.position != Some(rt.out_pos) {
                    self.toast = Some("Selected region not found in selected output".into());
                    self.pw_restore_token = None;
                    self.save_state();
                    return self.restore_interactive_overlays();
                }
                let (sw, sh) = stream
                    .size
                    .unwrap_or((rt.out_size.0 as i32, rt.out_size.1 as i32));
                let scale_x = sw as f32 / rt.out_size.0.max(1) as f32;
                let scale_y = sh as f32 / rt.out_size.1.max(1) as f32;
                let relx = (rt.rect.0 - rt.out_pos.0) as f32;
                let rely = (rt.rect.1 - rt.out_pos.1) as f32;
                let even = |v: f32| (v.round().max(0.0) as u32) & !1;
                Some((
                    (relx * scale_x).round().max(0.0) as u32,
                    (rely * scale_y).round().max(0.0) as u32,
                    even(rt.rect.2 as f32 * scale_x),
                    even(rt.rect.3 as f32 * scale_y),
                ))
            }
            None => None,
        };

        // Monitor mode: the placeholder selection guessed `outputs.first()`, but the
        // portal dialog let the user pick ANY monitor — re-anchor the selection to
        // the output the stream was actually granted on (matched by the stream's
        // logical position), so the filename, the preview's monitor bound, and the
        // toolbar/tray logic all track the RECORDED monitor, not the first one.
        // (Proven wrong the other way: a 5120×1440 stream saved under another
        // output's name, with the preview fitted to that other monitor's size.)
        let mut sel = pending.sel;
        if self.mode == Mode::Monitor
            && pending.region.is_none()
            && let Some(pos) = stream.position
            && let Some(o) = self.outputs.iter().find(|o| o.logical_pos == pos)
        {
            sel = Selection {
                x: o.logical_pos.0,
                y: o.logical_pos.1,
                width: o.logical_size.0,
                height: o.logical_size.1,
                output: Some(o.name.clone()),
                window_id: None,
            };
        }

        // Hold the stream and run the normal commit flow (countdown then record);
        // `start_recording` consumes the held stream when recording actually begins.
        self.pw_held = Some(HeldStream {
            fd: session.fd,
            node_id: stream.node_id,
            crop,
        });
        self.proceed_capture(sel)
    }

    /// Clamp `sel` to the output it overlaps most — the target monitor + the region
    /// trimmed to it (global logical coords). `None` if no output overlaps.
    /// Linux-only: the Wayland region-capture flow (`capture_flow.rs`) is the sole caller.
    #[cfg(target_os = "linux")]
    pub(super) fn region_clamped(&self, sel: &Selection) -> Option<RegionTarget> {
        let (rx, ry) = (sel.x, sel.y);
        let (rx1, ry1) = (rx + sel.width as i32, ry + sel.height as i32);
        let overlap = |o: &OutputState| {
            let (ox, oy) = o.logical_pos;
            let (ow, oh) = (o.logical_size.0 as i32, o.logical_size.1 as i32);
            let ix = rx1.min(ox + ow) - rx.max(ox);
            let iy = ry1.min(oy + oh) - ry.max(oy);
            (ix.max(0) as i64) * (iy.max(0) as i64)
        };
        let o = self.outputs.iter().max_by_key(|o| overlap(o))?;
        if overlap(o) == 0 {
            return None;
        }
        let (ox, oy) = o.logical_pos;
        let (ow, oh) = (o.logical_size.0 as i32, o.logical_size.1 as i32);
        let cx0 = rx.max(ox);
        let cy0 = ry.max(oy);
        let cx1 = rx1.min(ox + ow);
        let cy1 = ry1.min(oy + oh);
        Some(RegionTarget {
            out_pos: o.logical_pos,
            out_size: o.logical_size,
            rect: (cx0, cy0, (cx1 - cx0) as u32, (cy1 - cy0) as u32),
        })
    }
}
