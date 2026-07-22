//! Preview surface lifecycle and top-level views: opening (spinner/window/
//! overlay), the appearance toggle, resize/focus handling, and the composed
//! preview/loading/dialog views.
//! Split from `preview/mod.rs` (DRAGON-115) — pure code motion.

use super::*;


impl App {
    /// Open the preview overlay *now* with a loading spinner (no content, no path yet),
    /// for captures whose grab doesn't read the live composited screen (a window grab, a
    /// freeze crop, or a finished recording finalizing off-thread). The user sees the
    /// preview the instant they accept/stop, instead of a blank desktop while the work
    /// runs; [`Self::present_capture`] later fills in the saved path and the content.
    /// Returns the surface task (or none if preview is disabled or the output is unknown).
    pub(in crate::app) fn open_preview_spinner(
        &mut self,
        kind: PreviewKind,
        dims: Option<(u32, u32)>,
    ) -> Task<cosmic::Action<Msg>> {
        if !self.preview_after_capture {
            return Task::none();
        }
        let Some((output, monitor)) = self.preview_output.clone() else {
            return Task::none();
        };
        // A pre-opened video spinner is a stopped recording; pause other media right now if it
        // captured audio ("either mic or stereo"). An image grab (window/freeze) has no sound.
        let with_audio = matches!(kind, PreviewKind::Video(_))
            && (self.record_mic || self.record_system_audio);
        // Size the window to the capture up front (dims are known at grab time) so the
        // compositor honours the initial size — a post-open resize gets overridden.
        let extra_h = transport_h_for(&kind, PreviewSurface::Window);
        let source_scale = self.preview_source_scale(Some(&output));
        let (id, open_task, monitor, surface) =
            self.preview_surface_for(Some(output), monitor, dims, extra_h);
        self.preview = Some(PreviewState {
            window: id,
            surface,
            max_hint_pending: surface.is_window(),
            display_dims: dims,
            path: None,
            size: None,
            external: false,
            monitor,
            source_scale,
            loading_msg: random_loading_msg(),
            kind,
            edit: EditState::default(),
            view: Viewport::default(),
        });
        self.engage_preview_duck(with_audio);
        open_task
    }

    /// The SOURCE display's point→pixel backing scale for a capture anchored to
    /// `output`, stored on the [`PreviewState`] so the WINDOW open-fit divides the
    /// capture's PHYSICAL pixels back into the LOGICAL points it occupied on screen
    /// (a Retina grab opens at true on-screen size, not 2×). Linux (and any capture
    /// with no known output) is always `1.0` — the compositor's screencopy already
    /// lands logical-sized, and `1.0` keeps the sizing math byte-identical there.
    pub(super) fn preview_source_scale(&self, output: Option<&OutputHandle>) -> f32 {
        #[cfg(target_os = "macos")]
        if let Some(name) = output {
            return crate::platform::mac::scale_for(name)
                .map(|s| s as f32)
                .filter(|s| *s > 0.0)
                .unwrap_or(1.0);
        }
        let _ = output;
        // Linux (DRAGON-221): the capture output's buffer scale, cached from
        // `output_for_selection` before the overlay tore `self.outputs` down. `1.0`
        // on 1× displays (and for `--preview`, which sets `source_scale = 1.0`
        // directly), so the sizing math stays byte-identical there. Every other
        // platform is `1.0`.
        #[cfg(target_os = "linux")]
        {
            self.preview_output_scale
        }
        #[cfg(not(target_os = "linux"))]
        {
            1.0
        }
    }

    /// Create the preview surface per the appearance setting: a resizable WINDOW when
    /// `preview_windowed`, else the fullscreen layer-shell overlay. Returns the window
    /// id, the open task, the initial content size to scale to (the window's own
    /// size when windowed — corrected later by resize events — else the capture
    /// monitor), and the [`PreviewSurface`] kind that was minted (for the caller to
    /// record on `PreviewState`). `output` anchors the overlay (Some = capture
    /// monitor, None = active). `extra_h` is the media kind's transport-strip
    /// reserve for a WINDOW surface — callers derive it from the kind being opened
    /// via [`transport_h_for`] (a plain value, since the kind may not be stored in
    /// `self.preview` yet when a spinner pre-opens).
    pub(super) fn preview_surface_for(
        &mut self,
        output: Option<OutputHandle>,
        capture_monitor: (u32, u32),
        media_hint: Option<(u32, u32)>,
        extra_h: f32,
    ) -> (window::Id, Task<cosmic::Action<Msg>>, (u32, u32), PreviewSurface) {
        // A window pick pre-opens its spinner as the FULLSCREEN overlay even in WINDOWED mode
        // (the only focus-safe cover), then swaps it for the real window at `WindowGrabbed`.
        // Force the overlay branch while that pre-open is pending, regardless of
        // `preview_windowed`. Linux gates on `window_spinner_neutral` (the focus-neutral layer
        // surface); macOS (DRAGON-219) gates on `mac_preview_preopen` (the visible:false
        // shielding-level cover). Windows never sets either, keeping its decision byte-identical.
        #[cfg(target_os = "macos")]
        let force_neutral_overlay = self.mac_preview_preopen;
        // Windows (DRAGON-305): a WINDOWED window pick pre-opens the fullscreen blocker cover; force
        // the overlay branch while that pre-open is pending (the non-activating shielding cover).
        #[cfg(windows)]
        let force_neutral_overlay = self.win_preview_preopen;
        #[cfg(not(any(target_os = "macos", windows)))]
        let force_neutral_overlay = self.window_spinner_neutral;
        // Linux (layer-shell), macOS, and now Windows (DRAGON-233 fix 5 — a real
        // PlainWindows overlay, below) all honor `preview_windowed`. The cfg! folds to
        // `false` on all three (it only forces the WINDOW on an exotic platform with no
        // overlay implementation), so their decisions stay byte-identical to before —
        // and Windows now switches modes from the appearance toggle instead of always
        // opening a window.
        if (self.preview_windowed && !force_neutral_overlay)
            || cfg!(all(
                not(target_os = "linux"),
                not(target_os = "macos"),
                not(target_os = "windows")
            ))
        {
            // Open sized to the media (aspect-matched, no letterbox), from the caller's hint
            // (a fresh region capture knows its pixel size before the decode) or the already-
            // loaded content (toggling to windowed). Only fall back to ~80% of the monitor
            // when the size is genuinely unknown (a pre-opened spinner still decoding), since
            // the compositor won't shrink an over-large window after the fact.
            let media = media_hint
                // Videos size to their captured footprint (the encode upscales back
                // into it); stills to their decoded pixels — the same precedence as
                // `preview_viewport`, so a toggle reopens at the size that's shown.
                .or_else(|| self.preview.as_ref().map(|p| p.sizing_media()))
                .filter(|&(w, h)| w > 0 && h > 0);
            // The media arrives in PHYSICAL capture pixels; divide by the SOURCE
            // display's backing scale so the window opens to the LOGICAL points the
            // picture occupied on screen (a Retina grab isn't shown 2× too large).
            // `1.0` on Linux (and any unknown output), so `media` is unchanged there.
            let source_scale = self.preview_source_scale(output.as_ref());
            let media = media.map(|px| sizing::to_points(px, source_scale));
            // Both platforms size through the SAME `windowed_fit_size`, which caps the
            // window at 80% of the monitor height (rule 3, DRAGON-221) so it clears the
            // Dock / menu bar / panels neither compositor can measure client-side —
            // macOS won't shrink an over-large window after open, and this keeps the
            // request inside the usable area up front. Only fall back to ~80% of the
            // monitor when the size is genuinely unknown (a pre-opened spinner still
            // decoding), since the compositor won't shrink an over-large window later.
            let (w, h) = match media {
                Some(m) => windowed_fit_size(m, Some(capture_monitor), extra_h),
                None => (
                    (capture_monitor.0 as f32 * 0.8).clamp(super::shell::PREVIEW_MIN_W, 1600.0),
                    (capture_monitor.1 as f32 * 0.8).clamp(super::shell::PREVIEW_MIN_H, 1000.0),
                ),
            };
            // The windowed preview is only minted AFTER the grab (a window pick covers the
            // grab with the fullscreen overlay, then swaps to this window, DRAGON-219), so it
            // always opens visible and takes focus normally.
            let (id, open) = super::shell::preview_window(
                (w, h),
                (capture_monitor.0 as f32, capture_monitor.1 as f32),
            );
            // Title the window (server-side decorations show it in the titlebar; on
            // macOS the native finalize matches this exact title — hence the shared const).
            let title =
                self.set_window_title(super::shell::PREVIEW_WINDOW_TITLE.to_string(), id);
            (id, Task::batch([open, title]), (w as u32, h as u32), PreviewSurface::Window)
        } else {
            // macOS: a fullscreen always-on-top window covering the capture's display
            // (the pointer's display for `--preview`, which has no capture anchor) —
            // placed/finished natively after open (see `shell::preview_overlay_window`).
            // The process drops to the ACCESSORY activation policy FIRST, before the
            // open task runs: the DRAGON-154 AeroSpace popup opt-out is decided at the
            // window's first AX exposure and requires accessory (a windowed-preview
            // finalize or a `--preview` boot may have promoted us to Regular).
            #[cfg(target_os = "macos")]
            {
                crate::platform::mac::window::ensure_accessory_policy();
                let (pos, size) =
                    crate::platform::mac::overlay_preview_rect(output.as_deref());
                // DRAGON-216: while pre-opening to cover the grab, open `visible:false` so it
                // can't key/activate us; `finalize_preview_overlay` places + orders it front
                // non-key (no `gain_focus` until `WindowGrabbed`).
                let visible = !self.mac_preview_preopen;
                let (id, open) = super::shell::preview_overlay_window(pos, size, visible);
                let title = self
                    .set_window_title(super::shell::PREVIEW_OVERLAY_TITLE.to_string(), id);
                (id, Task::batch([open, title]), size, PreviewSurface::Overlay)
            }
            // Windows (DRAGON-233 fix 5): the fullscreen OVERLAY preview — a transparent
            // always-on-top window covering the capture's display (the pointer's for
            // `--preview --overlay`, which has no capture anchor), placed + shown natively
            // AFTER open (`shell::preview_overlay_window` → `finalize_preview_overlay` →
            // `place_overlay`, which OR-s in the komorebi opt-out bit before the show).
            #[cfg(target_os = "windows")]
            {
                let (pos, size) =
                    crate::platform::windows::window::preview_overlay_rect(output.as_deref());
                let (id, open) = super::shell::preview_overlay_window(pos, size);
                let title = self
                    .set_window_title(super::shell::PREVIEW_OVERLAY_TITLE.to_string(), id);
                (id, Task::batch([open, title]), size, PreviewSurface::Overlay)
            }
            #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
            {
                let id = window::Id::unique();
                let task = match output {
                    Some(o) => self.preview_surface(o, id),
                    None => self.preview_surface_active(id),
                };
                (id, task, capture_monitor, PreviewSurface::Overlay)
            }
        }
    }

    /// DRAGON-216/219 (windowed): swap the pre-opened fullscreen OVERLAY spinner that covered
    /// the grab for the real preview WINDOW now that the grab is done. A real toplevel takes
    /// activation (cosmic-comp's initial-map focus path is unconditional; macOS keys/activates
    /// on open), so it can only open AFTER the grab; before that the fullscreen overlay covered
    /// the wait without stealing the picked window's focus. Mints the window, re-points the
    /// existing `PreviewState` (still a loading spinner — content arrives later via
    /// `present_capture`), and keeps the OLD overlay alive painting its cover
    /// ([`Self::grab_cover_view`]) until the window's FIRST configure closes it
    /// ([`Self::preview_resized`]), so the window maps UNDER the cover with no desktop flash.
    /// One-shot; the appearance setting is NOT flipped. Portable (Linux neutral layer surface
    /// / macOS shielding-level overlay).
    pub(in crate::app) fn swap_neutral_spinner_to_window(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_ref() else {
            return Task::none();
        };
        let old_overlay = p.window;
        let dims = p.display_dims;
        let fallback_monitor = p.monitor;
        let extra_h = transport_h_for(&p.kind, PreviewSurface::Window);
        // Anchor the window to the capture monitor (as a fresh windowed preview would),
        // falling back to the spinner's own monitor.
        let (output, monitor) = match self.preview_output.clone() {
            Some((o, m)) => (Some(o), m),
            None => (None, fallback_monitor),
        };
        // `window_spinner_neutral` was already taken (false) by the caller, so
        // `preview_surface_for` mints the WINDOW branch, not another neutral overlay.
        let (new_id, open_task, new_monitor, surface) =
            self.preview_surface_for(output, monitor, dims, extra_h);
        if let Some(p) = self.preview.as_mut() {
            p.window = new_id;
            p.surface = surface;
            p.monitor = new_monitor;
            p.max_hint_pending = surface.is_window();
        }
        // Keep the overlay painting its cover until the window's first configure closes it,
        // so there's never a bare desktop between the two.
        self.grab_overlay_closing = Some(old_overlay);
        open_task
    }

    /// Flip the open preview between the fullscreen overlay and a resizable window,
    /// live: tear down the CURRENT surface (via its actual recorded kind, not the
    /// just-flipped setting), mint one of the other kind, and re-point the existing
    /// `PreviewState` (window, monitor, surface) at it — so the loaded content and
    /// every edit survive. The new appearance is also persisted as the default.
    /// No-op when no preview is open.
    pub(super) fn toggle_preview_appearance(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_ref() else {
            return Task::none();
        };
        let old_id = p.window;
        let old_surface = p.surface;
        let external = p.external;
        let fallback_monitor = p.monitor;

        // Persist the flip so it also becomes the default for the next capture.
        self.preview_windowed = !self.preview_windowed;
        self.save_state();

        // Tear down the CURRENT surface — via the kind that's ACTUALLY open, not the
        // (already-flipped) setting, so this can't mis-close a surface mid-toggle.
        let close = old_surface.close(old_id);

        // `--preview` files anchor to the active output (None); in-app captures to the
        // capture monitor. Fall back to the preview's last-known monitor size otherwise.
        let (output, monitor) = if external {
            (None, fallback_monitor)
        } else {
            match self.preview_output.clone() {
                Some((o, m)) => (Some(o), m),
                None => (None, fallback_monitor),
            }
        };
        // preview_surface_for reads the now-flipped `preview_windowed`, so this mints the
        // OTHER kind of surface.
        let extra_h = self
            .preview
            .as_ref()
            .map(|p| transport_h_for(&p.kind, PreviewSurface::Window))
            .unwrap_or(0.0);
        let (new_id, open_task, new_monitor, surface) =
            self.preview_surface_for(output, monitor, None, extra_h);
        if let Some(p) = self.preview.as_mut() {
            p.window = new_id;
            p.monitor = new_monitor;
            p.surface = surface;
            // A freshly-minted window carries the transient max_size hint again.
            p.max_hint_pending = surface.is_window();
        }
        // The new appearance has a different viewport, so a pan that was in-range before can
        // now leave the picture's edge floating — re-clamp it to the new bounds.
        if let Some(((minx, maxx), (miny, maxy))) =
            self.preview.as_ref().map(|p| self.preview_pan_bounds(p))
            && let Some(p) = self.preview.as_mut()
        {
            p.view.pan.0 = p.view.pan.0.clamp(minx, maxx);
            p.view.pan.1 = p.view.pan.1.clamp(miny, maxy);
        }
        Task::batch([close, open_task])
    }

    /// Pause OTHER apps' media the instant a preview overlay opens — not when its asset finishes
    /// loading — for as long as the overlay stays up (resumed in `stop_preview_playback`, or when
    /// we exit). Only when the user wants it and the preview has sound (`with_audio`). The pause
    /// runs in a child process, so this is just a quick spawn: no D-Bus on our threads, no UI hitch.
    pub(super) fn engage_preview_duck(&mut self, with_audio: bool) {
        if self.mute_others_during_preview && with_audio && self.preview_duck.is_none() {
            self.preview_duck = Some(crate::audio::ducking::OtherAudioDuck::engage());
        }
    }

    /// Open the preview overlay for `path` now (surface + content prep), on
    /// `preview_output`. `external` marks a pre-existing file (`--preview`). Returns the
    /// surface+content batch, or none if the output is unknown.
    pub(super) fn open_preview(
        &mut self,
        path: PathBuf,
        size: u64,
        is_video: bool,
        external: bool,
        dims: Option<(u32, u32)>,
    ) -> Task<cosmic::Action<Msg>> {
        let Some((output, monitor)) = self.preview_output.clone() else {
            return Task::none();
        };
        let (kind, task) = if is_video {
            (PreviewKind::Video(VideoPreview::loading()), video::poster_task(path.clone()))
        } else {
            (PreviewKind::Image(ImagePreview::loading()), image::decode_task(path.clone()))
        };
        let extra_h = transport_h_for(&kind, PreviewSurface::Window);
        let source_scale = self.preview_source_scale(Some(&output));
        let (id, open_task, monitor, surface) = self.preview_surface_for(Some(output), monitor, dims, extra_h);
        self.preview = Some(PreviewState {
            window: id,
            surface,
            max_hint_pending: surface.is_window(),
            display_dims: dims,
            path: Some(path),
            size: Some(size),
            external,
            monitor,
            source_scale,
            loading_msg: random_loading_msg(),
            kind,
            edit: EditState::default(),
            view: Viewport::default(),
        });
        // Post-capture overlay just opened — pause other media now if this recording has audio.
        self.engage_preview_duck(is_video && (self.record_mic || self.record_system_audio));
        Task::batch([open_task, task])
    }

    /// Open a preview for a pre-existing file (`--preview <file>`) on the *active* output
    /// (the monitor the user is on) — read-only toward the file: Save hidden, Cancel just
    /// exits, Save As copies. The monitor size starts as a placeholder and is corrected by
    /// the surface's resize event.
    pub(in crate::app) fn open_external_preview(
        &mut self,
        path: PathBuf,
        size: u64,
        is_video: bool,
    ) -> Task<cosmic::Action<Msg>> {
        let (kind, task) = if is_video {
            (PreviewKind::Video(VideoPreview::loading()), video::poster_task(path.clone()))
        } else {
            (PreviewKind::Image(ImagePreview::loading()), image::decode_task(path.clone()))
        };
        // Size the windowed preview to the file up front (the compositor won't shrink it
        // later): the picture's header dims (cheap read; video probes async, so unknown here)
        // against the largest known output as the monitor bound.
        let dims = if is_video {
            None
        } else {
            ::image::image_dimensions(&path).ok()
        };
        let monitor = self
            .outputs
            .iter()
            .map(|o| o.logical_size)
            .max_by_key(|(w, h)| *w as u64 * *h as u64)
            .unwrap_or((1920, 1080));
        let extra_h = transport_h_for(&kind, PreviewSurface::Window);
        let (id, open_task, monitor, surface) = self.preview_surface_for(None, monitor, dims, extra_h);
        self.preview = Some(PreviewState {
            window: id,
            surface,
            max_hint_pending: surface.is_window(),
            display_dims: dims,
            path: Some(path),
            size: Some(size),
            external: true,
            monitor,
            // A `--preview` file is a plain viewer: its pixels ARE its own resolution,
            // with no capture backing-scale to undo, so present it 1:1 in logical points.
            source_scale: 1.0,
            loading_msg: random_loading_msg(),
            kind,
            edit: EditState::default(),
            view: Viewport::default(),
        });
        // `--preview` is a viewer for an arbitrary file; we can't know if it has sound without
        // probing (the delay we're avoiding), so pause other media for any video as it opens.
        self.engage_preview_duck(is_video);
        Task::batch([open_task, task])
    }

    /// Update the open preview's monitor size when its surface is (re)sized — needed for
    /// `--preview`, which opens on the active output before its size is known. Also the
    /// windowed preview's post-map hook: the first configure means the surface is mapped,
    /// so the transient `max_size` hint (only there to disarm cosmic-comp's initial-map
    /// 2/3 reshape — see [`super::shell::preview_window`]) is cleared and the user
    /// resizes freely from here on.
    pub(in crate::app) fn preview_resized(&mut self, id: window::Id, w: u32, h: u32) -> Task<cosmic::Action<Msg>> {
        if w == 0 || h == 0 {
            return Task::none();
        }
        if self.preview.as_ref().is_none_or(|p| p.window != id) {
            return Task::none();
        }
        let clear_hint = match self.preview.as_mut() {
            Some(p) if p.max_hint_pending => {
                p.max_hint_pending = false;
                window::set_max_size(id, None)
            }
            _ => Task::none(),
        };
        // The stored zoom is FIT-relative, so a plain resize would change the displayed size
        // while the % readout stayed put. Preserve the native scale across the resize (except
        // "Fit to screen", which re-fits to the new width) so 100% stays 100%.
        let old_fit = self.preview.as_ref().map(|p| self.preview_fit_scale(p)).unwrap_or(1.0);
        let old_native = self.preview.as_ref().map(|p| p.view.zoom).unwrap_or(1.0) * old_fit;
        if let Some(p) = &mut self.preview {
            p.monitor = (w, h);
        }
        let fit_to_screen = self.preview.as_ref().is_some_and(|p| p.view.zoom_preset == Some(0));
        let new_fit = self.preview.as_ref().map(|p| self.preview_fit_scale(p)).unwrap_or(1.0);
        if let Some(p) = &mut self.preview {
            // "Fit to screen" re-fits (whole picture, zoom 1.0); any other zoom keeps its
            // native scale across the resize.
            let z = if fit_to_screen {
                Viewport::FIT
            } else {
                old_native / new_fit.max(0.0001)
            };
            p.view.set_zoom(z);
        }
        // DRAGON-216 (Linux windowed swap): this configure is the swapped-in window's FIRST
        // map (it's now `p.window`), so the neutral overlay that covered the grab has served
        // its purpose — close it. The window mapped UNDER the cover, so closing it now reveals
        // the window with no desktop flash. (macOS closes the cover from
        // `finalize_preview_window` instead — its first `Resized` can fire before the native
        // placement, which would drop the cover too early; DRAGON-219.)
        #[cfg(target_os = "linux")]
        if let Some(overlay) = self.grab_overlay_closing.take() {
            return Task::batch([clear_hint, super::super::shell::close_surface(overlay)]);
        }
        clear_hint
    }

    /// Re-focus the preview WINDOW (windowed mode only). No-op for the overlay (a layer
    /// surface with its own exclusive keyboard grab).
    pub(super) fn focus_preview_window(&self) -> Task<cosmic::Action<Msg>> {
        if let Some(p) = self.preview.as_ref()
            && p.surface.is_window()
        {
            return window::gain_focus(p.window);
        }
        Task::none()
    }

    /// Route a finished capture: show it in the preview overlay (when enabled), else fall
    /// back to the immediate copy/notify/exit share.
    pub(in crate::app) fn present_capture(
        &mut self,
        path: PathBuf,
        size: u64,
        is_video: bool,
        dims: Option<(u32, u32)>,
    ) -> Task<cosmic::Action<Msg>> {
        // Region "Copy selection" quick-action: force-copy to the clipboard and finish,
        // bypassing BOTH the preview and the persisted copy-to-clipboard toggle. Consume
        // the one-shot flag so a later capture in the same process behaves normally.
        if std::mem::take(&mut self.copy_selection_pending) {
            return self.finish_share_forced_copy(&path, size, is_video);
        }
        if !self.preview_after_capture {
            return self.finish_share(&path, size, is_video);
        }
        // Pre-opened (window/freeze grab, or a stopped recording): the spinner overlay is
        // already up — record where the capture landed and kick off the content prep into
        // the existing surface (image decode, or video poster extraction).
        if self.preview.is_some() {
            if let Some(p) = &mut self.preview {
                p.path = Some(path.clone());
                p.size = Some(size);
            }
            // DRAGON-221 follow-up: a WINDOWED window-pick deferred its cover→window
            // swap to THIS moment, when the COMPOSED dims are finally known (the
            // selection dims under-size the window — padding/shadow/wallpaper margins
            // grew the picture — and a post-open resize is not honored on COSMIC).
            // Feed the composed dims into the sizing state, mint the real window at
            // its correct size, and prep the content into it.
            let swap = if std::mem::take(&mut self.windowed_swap_pending) {
                if let (Some(d), Some(p)) = (dims, self.preview.as_mut()) {
                    p.display_dims = Some(d);
                }
                self.swap_neutral_spinner_to_window()
            } else {
                Task::none()
            };
            let prep = match self.preview.as_ref().map(|p| &p.kind) {
                Some(PreviewKind::Image(_)) => image::decode_task(path),
                Some(PreviewKind::Video(_)) => video::poster_task(path),
                None => Task::none(),
            };
            return Task::batch([swap, prep]);
        }
        // Not pre-opened (live region/monitor image grab, which needs a clean screen):
        // open the overlay now (spinner), then prep the content. The caller's `dims` let the
        // windowed preview open already sized to the picture (the compositor won't shrink it
        // after the fact).
        if self.preview_output.is_some() {
            return self.open_preview(path, size, is_video, false, dims);
        }
        self.finish_share(&path, size, is_video)
    }

    /// The preview overlay: a spinner while loading, then the media-specific content
    /// (image or video poster) with its action bar, centred on a dimmed full-screen
    /// surface.
    pub(in crate::app) fn preview_view(&self) -> Element<'_, Msg> {
        let Some(preview) = &self.preview else {
            return widget::space::Space::new()
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        };
        // Shrink the toolbar buttons in the windowed preview (its smaller window wants
        // tighter chrome) by 18%; full size for the fullscreen overlay. Built once and
        // threaded explicitly into every builder below (the button helpers take it).
        // Frosted glass (DRAGON-217): the WINDOWED preview's chrome paints translucent
        // so the compositor blur (enrolled on the window surface) shows through the
        // toolbars, titlebar, and image-background margins. The fullscreen overlay is a
        // layer-shell surface and is NEVER frosted (DRAGON-166), so glass is None there.
        let glass = if preview.surface.is_window() { self.glass } else { None };
        let tb = Tb { scale: preview.surface.btn_scale(), glass };
        let content: Element<'_, Msg> = if preview.is_loading() {
            self.preview_loading_view(preview, tb)
        } else {
            match &preview.kind {
                PreviewKind::Image(img) => self.image_loaded_view(preview, img, tb),
                PreviewKind::Video(vid) => self.video_loaded_view(preview, vid, tb),
            }
        };

        let base: Element<'_, Msg> = if preview.surface.is_window() {
            // A real window with CLIENT-side decorations (like the settings window): our own
            // header bar drawn into the surface (so a window capture keeps the title bar),
            // over the editor content, wrapped in the framework's rounded background + hairline
            // border. The transparent surface only shows through outside the rounded corners.
            let focused = self.core.focused_window() == Some(preview.window);
            let header = widget::header_bar()
                .title(super::shell::PREVIEW_WINDOW_TITLE)
                .focused(focused)
                .on_drag(Msg::Preview(PreviewMsg::WindowDrag))
                .on_double_click(Msg::Preview(PreviewMsg::WindowMaximize));
            // macOS (DRAGON-146): native traffic lights own close/min/zoom (kept +
            // centred in `finalize_preview_window`), so no CSD window buttons here;
            // native close routes through WindowCloseRequested → preview Cancel.
            // Windows (DRAGON-284): the native DWM caption buttons (installed post-show in
            // `finalize_preview_window`) likewise own close/minimize/maximize, so OMIT the
            // CSD buttons and reserve the trailing `WIN_CAPTION_INSET` spacer so the header
            // content clears the cluster. Native ✕ → WindowCloseRequested → preview Cancel;
            // the header double-click → the same native `toggle_maximize` as the max button.
            #[cfg(windows)]
            let header = header.end(
                widget::Space::new().width(Length::Fixed(crate::app::settings::WIN_CAPTION_INSET)),
            );
            #[cfg(all(not(target_os = "macos"), not(windows)))]
            let header = header
                .on_close(Msg::Preview(PreviewMsg::Cancel))
                .on_maximize(Msg::Preview(PreviewMsg::WindowMaximize))
                .on_minimize(Msg::Preview(PreviewMsg::WindowMinimize));
            // The editor fills the space below the header; centre it so the loading spinner
            // sits in the middle (the loaded view already fills, so centring is a no-op there).
            let body = widget::container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill);
            let stacked = widget::column(vec![header.into(), body.into()])
                .width(Length::Fill)
                .height(Length::Fill);
            // Frosted glass (DRAGON-217): the windowed preview's background paints
            // translucent so the compositor blur enrolled on this surface (shell.rs)
            // shows through the chrome. No-op (opaque, today's look) off COSMIC / when
            // frosted windows are off. The fullscreen OVERLAY branch below is a
            // layer-shell surface and is NEVER frosted (DRAGON-166).
            let glass = self.glass;
            widget::container(stacked)
                .padding(1)
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(move |theme| {
                    let cosmic = theme.cosmic();
                    // Windows (DRAGON-276) + macOS: the compositor / window server rounds the
                    // window and clips the content to it; don't double-round the outer container
                    // (thick corner) or draw a 1px app border (which reads as a corner fringe).
                    // Fill SQUARE and draw no border; the OS owns the edge. Linux keeps the
                    // app-drawn radius + border (its window edge is the app's). See the matching
                    // note in `settings::config_window_view`.
                    #[cfg(target_os = "linux")]
                    let radius = crate::app::theme::rounding(theme).window();
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(crate::app::theme::frost_color(
                            cosmic.background.base.into(),
                            glass,
                        ))),
                        border: Border {
                            color: cosmic.bg_divider().into(),
                            #[cfg(target_os = "macos")]
                            width: 0.0,
                            #[cfg(not(target_os = "macos"))]
                            width: 1.0,
                            #[cfg(not(target_os = "linux"))]
                            radius: 0.0.into(),
                            #[cfg(target_os = "linux")]
                            radius: radius.into(),
                        },
                        ..Default::default()
                    }
                }))
                .into()
        } else {
            // Overlay: dim the whole monitor (opacity from settings); centre the content.
            let dim = self.preview_overlay_opacity;
            widget::container(content)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(40)
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(move |_theme| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(cosmic::iced::Color::from_rgba(
                            0.0, 0.0, 0.0, dim,
                        ))),
                        ..Default::default()
                    }
                }))
                .into()
        };
        // A pending overwrite confirmation floats a modal dialog over everything.
        if preview.edit.confirm_overwrite {
            return self.overwrite_dialog(base);
        }
        base
    }

    /// The overwrite-confirmation modal shown when Save is pressed on an edited capture
    /// (a fresh capture's auto-saved file or a `--preview` original): a dimming backdrop
    /// that swallows (but doesn't dismiss on) clicks, with a centered Overwrite / Cancel
    /// card. Rendered in-app (stacked over the base) so it's clickable over the overlay's
    /// keyboard grab as well as in the window.
    pub(super) fn overwrite_dialog<'a>(&self, base: Element<'a, Msg>) -> Element<'a, Msg> {
        let backdrop: Element<'a, Msg> = widget::mouse_area(
            widget::container(widget::Space::new().width(Length::Fill).height(Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(|_t| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(crate::app::theme::SCRIM)),
                        ..Default::default()
                    }
                })),
        )
        .on_press(Msg::WindowChrome(WindowChromeMsg::Ignore)) // swallow; never dismiss on a stray click
        .interaction(cosmic::iced::mouse::Interaction::Idle)
        .into();
        let buttons: Element<'a, Msg> = widget::row(vec![
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::standard("Cancel")
                    .on_press(Msg::Preview(PreviewMsg::CancelOverwrite)),
            ),
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::destructive("Overwrite")
                    .on_press(Msg::Preview(PreviewMsg::ConfirmOverwrite)),
            ),
        ])
        .spacing(8.0)
        .into();
        // The file that's about to be overwritten (shown so the user knows exactly where).
        let path = self
            .preview
            .as_ref()
            .and_then(|p| p.path.as_ref())
            .map(|p| p.display().to_string());
        let mut col: Vec<Element<'a, Msg>> = vec![
            widget::text::title4("Overwrite original file?").into(),
            widget::text("Your edits will be written into the file on disk. This can't be undone.")
                .size(13)
                .into(),
        ];
        if let Some(path) = path {
            col.push(
                widget::container(widget::text(path).size(12).font(cosmic::font::mono()))
                    .padding([6.0, 10.0])
                    .width(Length::Fill)
                    .class(cosmic::theme::Container::custom(|theme| {
                        let c = theme.cosmic();
                        cosmic::iced::widget::container::Style {
                            background: Some(Background::Color(c.background.component.base.into())),
                            border: Border {
                                radius: crate::app::theme::rounding(theme).s.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    }))
                    .into(),
            );
        }
        col.push(buttons);
        let card = widget::container(widget::column(col).spacing(16.0))
        .padding(24.0)
        .max_width(420.0)
        .class(cosmic::theme::Container::custom(|theme| {
            let c = theme.cosmic();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(c.background.base.into())),
                border: Border {
                    radius: crate::app::theme::rounding(theme).m.into(),
                    width: 1.0,
                    color: c.background.divider.into(),
                },
                ..Default::default()
            }
        }));
        let centered: Element<'a, Msg> = widget::container(card)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        cosmic::iced::widget::stack(vec![base, backdrop, centered]).into()
    }

    /// Loading state: a centred spinner + a playful status line (picked from the kind's
    /// message set), with the close (x) cancel group below so the wait is cancellable — this
    /// portable view is what mac (its existing X affordance) and Windows both show, so the SAME
    /// X abort is present on both platforms. The X fires `PreviewMsg::Cancel` — the SAME abort
    /// the Esc hotkey triggers (`keyboard.rs`: `Action::PreviewCancel -> PreviewMsg::Cancel`),
    /// which routes through `finish_session` for a clean one-shot teardown (focus / menu-bar /
    /// window-order state restored, then exit). It stays clickable even while the window
    /// focus-then-grab is slow/hung because that grab runs OFF the UI thread
    /// (`window_focus_grab`'s `Send` closure on the capture worker, DRAGON-215), so the iced
    /// loop keeps pumping this surface's events.
    ///
    /// The whole loading content is wrapped in a `mouse_area` that SWALLOWS stray background
    /// presses (`Ignore`), so while windows are being rearranged behind this overlay a click
    /// off the cancel affordance can never fall through to a window underneath — only the X is
    /// interactive. On macOS/Windows the surface is itself an opaque, full-display winit window
    /// (never a click-through layer), so it already eats all pointer events; the swallow is the
    /// portable belt-and-suspenders that matches `overwrite_dialog`.
    pub(super) fn preview_loading_view(&self, preview: &PreviewState, tb: Tb) -> Element<'_, Msg> {
        let msgs: &[&str] = match &preview.kind {
            PreviewKind::Image(_) => &PREVIEW_LOADING_MESSAGES,
            PreviewKind::Video(_) => &video::PREVIEW_VIDEO_LOADING_MESSAGES,
        };
        let status = widget::column(vec![
            widget::indeterminate_circular().size(56.0).into(),
            widget::text(msgs[preview.loading_msg % msgs.len()])
                .size(16)
                .into(),
        ])
        .spacing(20.0)
        .align_x(Alignment::Center);
        let content = widget::column(vec![status.into(), tb.cancel_group()])
            .spacing(20.0)
            .align_x(Alignment::Center);
        // Swallow any press that lands off the cancel affordance so it can't reach a window
        // being rearranged behind the overlay. `Interaction::Idle` keeps the normal cursor over
        // the dead area (the X provides its own pointer feedback).
        widget::mouse_area(content)
            .on_press(Msg::WindowChrome(WindowChromeMsg::Ignore))
            .interaction(cosmic::iced::mouse::Interaction::Idle)
            .into()
    }

    /// Whether `id` is the open preview window.
    pub(in crate::app) fn is_preview_window(&self, id: window::Id) -> bool {
        self.preview.as_ref().is_some_and(|p| p.window == id)
    }

    /// DRAGON-216 (Linux windowed): the transient loading cover painted by the pre-opened
    /// neutral OVERLAY after its preview was swapped to a real window — kept up (over the
    /// mapping window) until the window's first configure closes it, so there's no desktop
    /// flash. Matches the overlay's loading look (same dim + spinner + message) so the swap
    /// is seamless; no cancel group (it's about to close, and the real window carries one).
    /// Portable view code (the overlay swap that drives it is Linux-only, so on other
    /// platforms `grab_overlay_closing` is never set and this is never reached).
    pub(in crate::app) fn grab_cover_view(&self) -> Element<'_, Msg> {
        let dim = self.preview_overlay_opacity;
        let msg_idx = self.preview.as_ref().map(|p| p.loading_msg).unwrap_or(0);
        let status = widget::column(vec![
            widget::indeterminate_circular().size(56.0).into(),
            widget::text(PREVIEW_LOADING_MESSAGES[msg_idx % PREVIEW_LOADING_MESSAGES.len()])
                .size(16)
                .into(),
        ])
        .spacing(20.0)
        .align_x(Alignment::Center);
        widget::container(status)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .padding(40)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(move |_t| {
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(cosmic::iced::Color::from_rgba(
                        0.0, 0.0, 0.0, dim,
                    ))),
                    ..Default::default()
                }
            }))
            .into()
    }
}
