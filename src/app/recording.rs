use super::*;

/// Push-to-talk is HIDDEN for now (DRAGON-109): its hold hotkey can't be
/// delivered while our surfaces are unfocused on COSMIC (no GlobalShortcuts
/// portal yet), which left the mic seeded muted with no working un-mute. The
/// settings row is gone and the behavior inert — the persisted setting is
/// KEPT, so flipping this back on restores everything once hold delivery
/// exists (the portal bind in `platform::global_shortcuts`, or a successor).
pub(super) const PTT_AVAILABLE: bool = false;

impl App {
    /// Record a live channel toggle against the running recording (timestamped
    /// now, baked in at the finalize pass). Instant — just an in-memory append, so
    /// the icon flips with no perceptible delay. A no-op when not recording.
    pub(super) fn log_audio_toggle(&self, chan: crate::record::AudioChannel, on: bool) {
        self.log_audio_toggle_at(std::time::Instant::now(), chan, on);
    }

    /// [`Self::log_audio_toggle`] with an explicit timestamp — for portal hotkey
    /// events, which are stamped at SIGNAL arrival so the mute timeline stays
    /// exact however late the UI drains them.
    pub(super) fn log_audio_toggle_at(
        &self,
        at: std::time::Instant,
        chan: crate::record::AudioChannel,
        on: bool,
    ) {
        if let Some(rec) = &self.recording
            && let Ok(mut evs) = rec.events.lock() {
                evs.push((at, chan, on));
            }
    }

    /// Whether the mic is captured for a recording: the user armed it, OR push-to-talk
    /// is on (which captures the mic so it can be gated by the hold, muted otherwise).
    pub(super) fn mic_armed(&self) -> bool {
        self.record_mic || self.ptt_active()
    }

    /// The EFFECTIVE push-to-talk state: the persisted setting gated by
    /// [`PTT_AVAILABLE`]. Every behavior site reads this, never the raw setting.
    pub(super) fn ptt_active(&self) -> bool {
        PTT_AVAILABLE && self.push_to_talk
    }

    /// The input cleanup config built from the current audio settings, shared by the
    /// mic test (and, later, the recording path).
    pub(super) fn input_config(&self) -> crate::audio::InputConfig {
        crate::audio::InputConfig {
            noise_suppression: self.noise_reduction,
            echo_cancellation: self.echo_cancellation,
            auto_gain: self.auto_gain,
            gate: true,
            gate_auto: self.input_sensitivity_auto,
            gate_threshold: self.input_sensitivity,
            advanced_vad: self.advanced_vad,
        }
    }

    /// The effective recording resolution cap as a `(max_w, max_h)` box; `(0, 0)`
    /// means no user limit (only the encoder hard max applies).
    pub(super) fn res_limit(&self) -> (u32, u32) {
        if self.record_res_preset as usize == RES_CUSTOM {
            (self.record_max_width.value, self.record_max_height.value)
        } else {
            res_dims(self.record_res_preset as usize)
        }
    }

    /// Capture source label for metadata: PipeWire portal vs the COSMIC compositor,
    /// for the current kind.
    pub(super) fn source_label(&self) -> &'static str {
        let pw = match self.kind {
            Kind::Video => self.recording_uses_portal(),
            Kind::Image | Kind::Scanner => self.screenshot_uses_portal(),
        };
        if pw && self.pipewire_available {
            "pipewire"
        } else {
            "cosmic"
        }
    }

    pub(super) fn mode_label(&self) -> &'static str {
        match self.mode {
            Mode::Region => "region",
            Mode::Window => "window",
            Mode::Monitor => "monitor",
        }
    }

    fn res_label(&self) -> String {
        let p = self.record_res_preset as usize;
        if p == RES_CUSTOM {
            format!("{}x{}", self.record_max_width.value, self.record_max_height.value)
        } else if p == 0 {
            "original".to_string()
        } else {
            RES_LABELS[p].to_string()
        }
    }

    /// The active encoder family's speed/quality preset, for the metadata line:
    /// the NVENC `p1`–`p7` ladder, the x264 named ladder, or VAAPI's compression
    /// level (`cl3`; `cl-default` when left to the driver).
    fn preset_label(&self, encoder: &str) -> String {
        match encoder {
            "nvenc" => self.nvenc_preset.clone(),
            "software" => self.x264_preset.clone(),
            "vaapi" => match self.vaapi_compression_level {
                -1 => "cl-default".to_string(),
                cl => format!("cl{cl}"),
            },
            _ => "-".to_string(),
        }
    }

    /// One-line, parseable metadata describing how a recording was made — embedded in
    /// the file (mp4 `comment`) and read back by `--inspect`. Starts with the app name.
    /// `codec=auto` is annotated with the RESOLVED codec by the finalize pass (the
    /// encode plan picks H.264 vs HEVC only once the first frame fixes the dims).
    pub(super) fn recording_metadata(&self) -> String {
        let encoder = self.effective_encoder();
        let encoder = encoder.as_str();
        let audio = match (self.mic_armed(), self.record_system_audio) {
            (true, true) => "mic+system",
            (true, false) => "mic",
            (false, true) => "system",
            (false, false) => "none",
        };
        let zc = if self.record_zero_copy { "on" } else { "off" };
        format!(
            "Cosmic Capture Kit | type=video | source={} | mode={} | encoder={} | \
             preset={} | codec={} | zero_copy={} | max_res={} | fps={} | bitrate={}k | audio={}",
            self.source_label(),
            self.mode_label(),
            encoder,
            self.preset_label(encoder),
            self.record_codec,
            zc,
            self.res_label(),
            self.record_fps.value,
            self.record_bitrate_kbps.value,
            audio,
        )
    }

    /// The user's chosen encoder presets (passed to the encode pipeline).
    pub(super) fn presets(&self) -> crate::encode::Presets {
        crate::encode::Presets {
            nvenc: self.nvenc_preset.clone(),
            x264: self.x264_preset.clone(),
            vaapi_cl: self.vaapi_compression_level,
            codec: self.record_codec.clone(),
        }
    }

    /// The probed encoder list (friendly labels), resolved lazily on first read
    /// (DRAGON-201): the underlying `ffmpeg -encoders` probe runs the first time this
    /// is called, never on the screenshot-launch init path.
    pub(super) fn encoders(&self) -> &[crate::encode::EncoderInfo] {
        self.encoders.list()
    }

    /// The live preferred-encoder id, resolved lazily on first read (DRAGON-201).
    pub(super) fn preferred_encoder(&self) -> String {
        self.encoders.preferred()
    }

    /// Overwrite the live preferred-encoder id (user pick / persist apply).
    pub(super) fn set_preferred_encoder(&self, id: String) {
        self.encoders.set_preferred(id);
    }

    /// Windows (DRAGON-238): kick the encoder probe OFF the UI thread if it hasn't run
    /// yet, so opening the settings video / Health page never blocks on the `ffmpeg
    /// -encoders` scan plus the hardware probe-encodes (seconds). The result lands as
    /// `SettingsMsg::EncodersProbed` and fills the process-wide cache; until then the video
    /// page shows a placeholder. A no-op if already probed or a probe is already in flight.
    /// Linux/mac never call this — their first read probes synchronously (timing untouched).
    #[cfg(windows)]
    pub(in crate::app) fn kick_encoder_probe(&self) -> Task<cosmic::Action<Msg>> {
        if !self.encoders.begin_probe() {
            return Task::none();
        }
        Task::perform(
            async {
                tokio::task::spawn_blocking(crate::app::EncoderResolve::probe_list)
                    .await
                    .unwrap_or_default()
            },
            |list| cosmic::Action::App(Msg::Settings(SettingsMsg::EncodersProbed(list))),
        )
    }

    /// Windows (DRAGON-238): a non-blocking peek at the probed encoder list (`None` until
    /// the off-thread probe finishes) — the settings video page renders a placeholder
    /// until it lands, instead of blocking the UI thread on the first read.
    #[cfg(windows)]
    pub(in crate::app) fn encoders_peek(&self) -> Option<&[crate::encode::EncoderInfo]> {
        self.encoders.peek()
    }

    /// The encoder a recording will actually use right now — software when hardware
    /// encoding is off, otherwise the preferred encoder (resolved to the best
    /// available when it isn't a concrete id). Drives which preset row the settings
    /// show.
    pub(super) fn effective_encoder(&self) -> String {
        match self.preferred_encoder().as_str() {
            id @ ("nvenc" | "vaapi" | "software") => id.to_string(),
            _ => self
                .encoders()
                .first()
                .map(|e| e.id.clone())
                .unwrap_or_else(|| "software".to_string()),
        }
    }

    /// Kick off the encoder benchmark (~1.5s of black frames per available backend)
    /// unless one is already running. Each encoder is tested at the SELECTED monitor's
    /// TRUE capture footprint, resolved THROUGH the recording encode plan (DRAGON-163):
    /// the same `encoder_capped_resolution` + `fit_within` + codec routing the recording
    /// workers use, so the red/green verdict predicts real recording on that monitor
    /// (software downscaled at 5K, h264 routed to HEVC above 4096). Progress lands in
    /// `self.bench`, polled by `BenchPoll`.
    pub(super) fn spawn_encoder_bench(&mut self) -> Task<cosmic::Action<Msg>> {
        if self.bench.as_ref().is_some_and(|b| {
            b.lock().map(|g| !g.finished).unwrap_or(false)
        }) {
            return Task::none(); // already running
        }
        let backends: Vec<(String, String)> = self
            .encoders()
            .iter()
            .map(|e| (e.id.clone(), e.label.clone()))
            .collect();
        // The monitor under test: the dropdown selection's TRUE footprint, or the
        // configured max-res box (fallback: the historical 4K proxy) when no monitor
        // enumerated (no permission / not a settings launch).
        let monitor = self.bench_monitors.get(self.bench_monitor_idx);
        let (bw, bh) = match monitor.map(|m| (m.px_w, m.px_h)) {
            Some(dims) => dims,
            None => match self.res_limit() {
                (0, 0) => (3840, 2160),
                dims => dims,
            },
        };
        let monitor_label = monitor
            .map(|m| m.label.clone())
            .unwrap_or_else(|| format!("{bw}x{bh}"));
        let shared = std::sync::Arc::new(std::sync::Mutex::new(EncoderBench {
            total: backends.len(),
            current: backends.first().map(|b| b.1.clone()).unwrap_or_default(),
            monitor_label,
            ..Default::default()
        }));
        self.bench = Some(shared.clone());
        let bitrate = self.record_bitrate_kbps.value;
        let presets = self.presets();
        let fps = self.record_fps.value;
        // The user's max-resolution box, so the plan mirror honours a manual cap too.
        let max_res = self.res_limit();
        std::thread::spawn(move || {
            for (id, label) in backends {
                if let Ok(mut g) = shared.lock() {
                    g.current = label.clone();
                }
                // Mirror the recording plan for THIS encoder at the monitor's true dims:
                // the encode size + resolved codec a real recording would use.
                let plan = crate::encode::bench_plan_for(&id, bw, bh, max_res, fps, &presets);
                // ~1.5s of black frames at the plan-resolved dims + bitrate + preset. On
                // macOS include the real capture-thread per-frame cost (the BGRA→RGBA
                // swizzle, DRAGON-168) so the number predicts a real recording of THIS
                // monitor+encoder, not encoder-only throughput. On Linux the capture
                // delivers RGBA/dmabuf with no such per-frame CPU swizzle, so the
                // encoder-only measurement already reflects reality there.
                let capture_cost = cfg!(target_os = "macos");
                let score = crate::encode::bench_encoder_pipeline(
                    &id, plan.width, plan.height, bitrate, &presets, 1.5, capture_cost,
                );
                if let Ok(mut g) = shared.lock() {
                    g.results.push(BenchResult {
                        label,
                        score,
                        enc_w: plan.width,
                        enc_h: plan.height,
                        is_hevc: plan.is_hevc,
                    });
                    g.done += 1;
                }
            }
            if let Ok(mut g) = shared.lock() {
                g.finished = true;
            }
        });
        Task::none()
    }

    pub(super) fn start_recording(&mut self, sel: Selection) -> Task<cosmic::Action<Msg>> {
        // Bind the recording hotkeys (PTT hold + stop) through the portal
        // GlobalShortcuts interface, once per process — they then fire focus-free,
        // which is what hold-to-talk needs while the recorded app has focus
        // (DRAGON-109). Where the desktop doesn't ship the interface (COSMIC
        // today) the bind fails fast and the keyboard paths stand unchanged.
        if self.hotkeys.is_none() {
            let ptt = self
                .keymap
                .get(crate::shortcuts::Action::RecordToggleMic)
                .and_then(|sc| sc.xdg_trigger());
            let stop = self
                .keymap
                .get(crate::shortcuts::Action::RecordStop)
                .and_then(|sc| sc.xdg_trigger());
            self.hotkeys = Some(crate::platform::global_shortcuts::start(ptt, stop));
        }
        // Hotkey presses from BEFORE this recording (a stray stop between takes)
        // must not act on it.
        if let Some(hk) = &self.hotkeys
            && let Ok(mut g) = hk.events.lock()
        {
            g.clear();
        }
        // A live mic test holds its own capture of the same mic + runs the cleanup chain
        // and a vsync waveform loop. Recording opens its own mic capture, so leaving the
        // test running double-captures the device and burns CPU on a preview nobody is
        // looking at during a recording — close it first.
        self.close_mic_test();
        // macOS (Bug B) / Windows (DRAGON-248): release the armed-idle system-audio metering
        // capture BEFORE the recording's owned capture starts — the system-audio stream must
        // not be claimed by two captures at once. The recording capture then owns the meter
        // (publishing its own RMS); this hands over race-free.
        #[cfg(any(target_os = "macos", windows))]
        self.stop_sys_idle_meter();
        // Push the current mic cleanup settings so the recording's ffmpeg captures the
        // cleaned mic (the spawn reads this global, like the mic source).
        crate::audio::config::set_recording_mic_config(self.input_config(), &self.speaker_device);
        // Same idiom for the system-track ducking flag (DRAGON-128): the pump reads
        // this global when it's configured.
        crate::audio::config::set_recording_duck_system(self.duck_system_audio);
        // Close any other instances so only this overlay records.
        crate::instance::close_other_instances();
        // Recording starts (after any countdown) → restore focus to the window we
        // expect: the captured window when we picked one (screencopy window mode),
        // otherwise whatever was focused before we launched (origin_window — also the
        // value the screenshot path restores). So it records focused and you can type
        // into it without clicking it again — the overlay is click-through here. Works
        // for every mode/path (origin_window is ours, not the capture's). Off the UI
        // thread because activate() waits for the toplevel list to settle.
        if let Some(id) = sel.window_id.clone().or_else(|| self.origin_window.clone()) {
            std::thread::spawn(move || crate::platform::compositor::activate(&id));
        }
        // A portal stream granted at commit (held across any countdown) → record it;
        // otherwise direct screencopy.
        if let Some(held) = self.pw_held.take() {
            return self.start_held_pipewire_recording(held, sel);
        }
        self.start_screencopy_recording(sel)
    }

    /// Begin recording the already-granted portal stream `held`.
    fn start_held_pipewire_recording(
        &mut self,
        held: HeldStream,
        sel: Selection,
    ) -> Task<cosmic::Action<Msg>> {
        let out_path = self.record_output_path(&sel);
        let (max_w, max_h) = self.res_limit();
        let handle = crate::record::start_pipewire_recording(crate::record::PipewireRecordParams {
            fd: held.fd,
            node_id: held.node_id,
            crop: held.crop,
            settings: crate::record::RecordSettings {
                fps: self.record_fps.value,
                preferred_encoder: self.preferred_encoder(),
                presets: self.presets(),
                zero_copy: self.record_zero_copy,
                mic: self.mic_armed(),
                system_audio: self.record_system_audio,
                bitrate_kbps: self.record_bitrate_kbps.value,
                audio_offset_ms: self.audio_sync_offset_ms.value,
                // Auto mode probes + folds in the live device latency (system channel
                // only); manual mode keeps the offset above exactly as set (DRAGON-119).
                auto_device_compensation: self.audio_sync_auto,
                max_res: (max_w, max_h),
                metadata: self.recording_metadata(),
                out_path: out_path.clone(),
            },
        });
        self.recording = Some(handle);
        self.recording_started = Some(std::time::Instant::now());
        self.recording_paused_at = None;
        self.recording_paused_accum = std::time::Duration::ZERO;
        self.recording_path = Some(crate::record::recording_temp_path(&out_path));
        self.pending = Some(sel);
        self.begin_recording_tray();
        self.recreate_active_overlays()
    }

    /// Direct cosmic-screencopy recording (the default path, and the fallback when
    /// the portal/PipeWire is unavailable).
    fn start_screencopy_recording(&mut self, sel: Selection) -> Task<cosmic::Action<Msg>> {
        let out_path = self.record_output_path(&sel);
        // Record only what's inside the visible line: a region is inset by the
        // line width (the outline, drawn on the original rect, then sits just
        // outside the recorded crop); window/monitor record the full target.
        let rec = inset_region(sel.clone());
        let (max_w, max_h) = self.res_limit();
        let preferred_encoder = self.preferred_encoder();
        let handle = crate::record::start_region_recording(crate::record::RegionRecordParams {
            x: rec.x,
            y: rec.y,
            w: rec.width,
            h: rec.height,
            cursor: self.capture_cursor,
            // macOS SCK target: window mode records the picked window directly
            // (occlusion-independent), monitor mode the chosen display, region a crop.
            // Linux never sees this field (it's cfg'd out), so its literal is unchanged.
            #[cfg(target_os = "macos")]
            mac_target: mac_record_target(&rec),
            // Windows WGC target (DRAGON-229): the analog of `mac_target` — window /
            // monitor / region. Linux/mac never see this field (cfg'd out).
            #[cfg(windows)]
            win_target: win_record_target(&rec),
            settings: crate::record::RecordSettings {
                fps: self.record_fps.value,
                preferred_encoder: preferred_encoder.clone(),
                presets: self.presets(),
                // GPU zero-copy applies to a full output (Monitor mode, no crop) with a
                // hardware encoder; Region/Window crop, so they take the CPU path.
                zero_copy: self.mode == Mode::Monitor
                    && self.record_zero_copy
                    && preferred_encoder != "software",
                mic: self.mic_armed(),
                system_audio: self.record_system_audio,
                bitrate_kbps: self.record_bitrate_kbps.value,
                audio_offset_ms: self.audio_sync_offset_ms.value,
                // Auto mode probes + folds in the live device latency (system channel
                // only); manual mode keeps the offset above exactly as set (DRAGON-119).
                auto_device_compensation: self.audio_sync_auto,
                max_res: (max_w, max_h),
                metadata: self.recording_metadata(),
                out_path: out_path.clone(),
            },
        });
        self.recording = Some(handle);
        self.recording_started = Some(std::time::Instant::now());
        self.recording_paused_at = None;
        self.recording_paused_accum = std::time::Duration::ZERO;
        // The live size readout tracks the temp capture as it grows; the final file
        // (after the finalize pass) is what `RecordingPoll` reports on `done`.
        self.recording_path = Some(crate::record::recording_temp_path(&out_path));
        // Keep the ORIGINAL selection for the overlay/border + toolbar anchor.
        self.pending = Some(sel);
        self.begin_recording_tray();
        // Recreate the overlay click-through except the toolbar's region so the
        // recorded apps stay usable. (A no-op rebuild from the countdown, which is
        // already active, but cheap.)
        self.recreate_active_overlays()
    }

    /// The tray glyph tint (DRAGON-179): the app's EFFECTIVE, RESOLVED accent — the same
    /// colour the chrome actually draws — so the icon can never disagree with it. Routed
    /// through [`theme::resolved_appearance_accent_rgba`] (DRAGON-289) so the Automatic
    /// Contrast Boost applies here in lockstep with the fills/text (and with the resident
    /// daemon, which tints from the same resolver): a boosted accent tints a boosted icon.
    /// The resolver already folds in the override-vs-system pick, so this no longer reads
    /// the raw persisted override directly.
    fn tray_accent(&self) -> [u8; 3] {
        let [r, g, b, _] = crate::app::theme::resolved_appearance_accent_rgba();
        [r, g, b]
    }

    /// Post-start hooks shared by both recording paths: raise the recording status
    /// icon and, for push-to-talk, seed the mic muted so it's silent until the
    /// hotkey is held. The own icon exists ONLY while a recording is live
    /// (DRAGON-182) — there is no idle session icon anymore, so an icon is always
    /// raised fresh here and dropped in `end_recording_tray`.
    fn begin_recording_tray(&mut self) {
        // Prefer a resident/daemon relay: when the mac menu-bar daemon / Linux resident is
        // present, ALL in-recording controls belong in ITS one icon (DRAGON-170/173).
        // Failure (no resident: a terminal / CLI recording, or the resident off) raises
        // this process's OWN recording icon so the controls exist.
        if let Some(relay) =
            crate::tray::TraySession::start_daemon(self.record_mic, self.record_system_audio)
        {
            self.tray = Some(relay);
        } else {
            self.tray = crate::tray::TraySession::start_recording(
                self.record_mic,
                self.record_system_audio,
                self.tray_accent(),
            );
        }
        // DRAGON-174: the ONLY thing that hides the in-frame toolbar now is the user's
        // "hide toolbar on full screen captures" setting AND a capture the toolbar can't
        // fit outside of. Nothing about the tray/icon depends on this (the icon ALWAYS
        // carries the controls); daemon-attached no longer implies hiding.
        self.tray_hides_toolbar = toolbar_hidden(
            self.hide_toolbar_fullscreen,
            self.recording_toolbar_oversized(),
        );
        // Push-to-talk: the mic (armed via `mic_armed`) starts muted (an off event at
        // t≈0), so it's only audible while the hotkey is held (which logs on/off around
        // the held span).
        if self.ptt_active() {
            self.ptt_held = false;
            self.log_audio_toggle(crate::record::AudioChannel::Mic, false);
        }
    }

    /// Drop the recording status icon when the recording ends (DRAGON-182): the own
    /// icon exists ONLY while a recording is live, so it comes down with the
    /// recording (a daemon relay drop likewise reverts the resident to its own idle
    /// menu). Also clears the toolbar-hidden flag (no toolbar exists post-recording).
    pub(super) fn end_recording_tray(&mut self) {
        self.tray = None;
        self.tray_hides_toolbar = false;
    }

    /// Tear the session status icon down entirely (DRAGON-174) — the end of the whole
    /// capture session (`finish_session`). Drops any own icon (removing it from the
    /// menu bar / tray) or relay.
    pub(super) fn drop_session_icon(&mut self) {
        self.tray = None;
        self.tray_hides_toolbar = false;
    }

    /// Apply the portal hotkey events delivered since the last poll: PTT hold
    /// (press un-mutes, release re-mutes — timestamps from signal arrival keep the
    /// mute timeline exact) and stop. The portal delivers these focus-free, so
    /// the recording overlays never need a keyboard grab on its behalf. Runs on
    /// the recording poll.
    pub(super) fn drain_portal_hotkeys(&mut self) -> Task<cosmic::Action<Msg>> {
        use crate::platform::global_shortcuts::HotkeyEvent as Ev;
        if self.hotkeys.is_none() {
            return Task::none();
        }
        let mut tasks: Vec<Task<cosmic::Action<Msg>>> = Vec::new();
        let events = self
            .hotkeys
            .as_ref()
            .and_then(|h| h.events.lock().ok().map(|mut g| std::mem::take(&mut *g)))
            .unwrap_or_default();
        for (at, ev) in events {
            match ev {
                Ev::PttPressed if self.ptt_active() => {
                    // Same dedup as the keyboard path: only the first press of a
                    // held span un-mutes.
                    if !self.ptt_held {
                        self.ptt_held = true;
                        if self.recording.is_some() {
                            self.log_audio_toggle_at(at, crate::record::AudioChannel::Mic, true);
                        }
                    }
                }
                Ev::PttReleased if self.ptt_active() => {
                    if self.ptt_held {
                        self.ptt_held = false;
                        self.log_audio_toggle_at(at, crate::record::AudioChannel::Mic, false);
                    }
                }
                Ev::Stop if self.recording.is_some() => {
                    tasks.push(self.stop_recording());
                }
                // PTT events with push-to-talk off: the portal 'ptt' shortcut only
                // has hold semantics; the plain mic TOGGLE stays a keyboard action.
                _ => {}
            }
        }
        Task::batch(tasks)
    }

    /// Whether the recording toolbar can't fit OUTSIDE the captured area — the case
    /// the tray placement is for. Always true for a full-screen monitor capture; for a
    /// region / window it's true only when the toolbar's own placement overlaps the
    /// captured rect (i.e. it had to fall back inside — the region is too large to
    /// leave room around it). Runs at recording start (`self.pending` = the capture).
    fn recording_toolbar_oversized(&self) -> bool {
        if self.mode == Mode::Monitor {
            return true;
        }
        // The captured rect in global logical coords (region or window selection).
        let Some(sel) = self.pending.as_ref() else {
            return false;
        };
        // A portal-picked window commits a 1×1 placeholder (the portal never
        // reports the window's geometry — see `portal_for_mode`), so the overlap
        // check below can't see the real footprint. Treat it as oversized, like
        // Monitor: the picked window may well be maximized, and the toolbar can't
        // anchor beside a rect it doesn't know.
        if self.mode == Mode::Window && sel.width <= 1 && sel.height <= 1 {
            return true;
        }
        let (cx, cy, cw, ch) = (sel.x as f32, sel.y as f32, sel.width as f32, sel.height as f32);
        // The toolbar overlaps the capture on the output it sits on → no outside room.
        self.outputs.iter().any(|o| {
            let Some((tb, _)) = self.toolbar_layout(o) else {
                return false;
            };
            // `toolbar_layout` returns output-local coords; shift to global to compare.
            let (tx, ty) = (tb.x + o.logical_pos.0 as f32, tb.y + o.logical_pos.1 as f32);
            let overlap_x = tx < cx + cw && tx + tb.width > cx;
            let overlap_y = ty < cy + ch && ty + tb.height > cy;
            overlap_x && overlap_y
        })
    }

    /// Whether the live recording is paused (nothing being captured).
    pub(super) fn recording_paused(&self) -> bool {
        self.recording.is_some() && self.recording_paused_at.is_some()
    }

    /// The RECORDED elapsed time of the live recording, in whole seconds —
    /// wall time minus every paused stretch, so the readout freezes while
    /// paused instead of counting wall clock.
    pub(super) fn recording_elapsed_secs(&self) -> u64 {
        let Some(started) = self.recording_started else {
            return 0;
        };
        let until = self.recording_paused_at.unwrap_or_else(std::time::Instant::now);
        until
            .saturating_duration_since(started)
            .saturating_sub(self.recording_paused_accum)
            .as_secs()
    }

    /// Pause the recording, or resume it when paused. The worker ends its
    /// current segment (its capture connection stays open) and starts a fresh
    /// one on resume; the final stop stitches the segments together. Instant
    /// from the UI's side — just an atomic flip + tray/toolbar refresh.
    pub(super) fn toggle_pause(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(rec) = &self.recording else {
            return Task::none();
        };
        match self.recording_paused_at.take() {
            // Paused → resume. Account the pause into the accumulator so the
            // elapsed readout picks up where it froze.
            Some(paused_at) => {
                self.recording_paused_accum += paused_at.elapsed();
                rec.paused.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            None => {
                self.recording_paused_at = Some(std::time::Instant::now());
                rec.paused.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        if let Some(tray) = &self.tray {
            tray.set_paused(self.recording_paused_at.is_some());
        }
        Task::none()
    }

    /// Stop the recording: signal the worker (it finalizes the file) and clear the
    /// overlay, opening the video preview overlay (a spinner) right away to cover the
    /// finalize wait. `RecordingPoll` fills in the poster once the file is ready.
    pub(super) fn stop_recording(&mut self) -> Task<cosmic::Action<Msg>> {
        self.end_recording_tray();
        if let Some(rec) = &self.recording {
            rec.stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // DRAGON-309: open the finalized-recording preview on the TRIGGER display (the monitor
        // active when the recording was initiated), NOT the recorded region's monitor, matching
        // the still-capture path. Fall back to the selection's output when the trigger can't be
        // resolved. Captured before the overlay (and `self.outputs`) tears down; the finalize
        // pass is a file op (no live-screen read), so the preview overlay is safe to show.
        if let Some(sel) = self.pending.clone() {
            self.preview_output =
                self.active_trigger_display().or_else(|| self.output_for_selection(&sel));
            self.preview_output_scale = self.scale_for_selection(&sel);
        }
        let mut cmds = self.destroy_surfaces();
        // The recording's CAPTURED footprint sizes the windowed preview at open, so
        // the window opens showing the capture at the size it occupied on screen —
        // a resolution-capped encode upscales back into that box for display
        // (`contain_dims`), instead of shrinking the preview to the encode. The
        // worker PUBLISHED the footprint when its first frame fixed it (physical
        // pixels), covering every path incl. HiDPI scale and the portal, whose
        // selection geometry the UI can't know. Only a stop before the first frame
        // leaves it unset; then the selection's logical size stands in (scale
        // unknowable here) and the `PosterReady` re-fit corrects any drift.
        let dims = self
            .recording
            .as_ref()
            .and_then(|r| r.dims.lock().ok().and_then(|g| *g))
            .or_else(|| {
                self.pending
                    .as_ref()
                    // The portal's placeholder selections (1×1) carry no real
                    // footprint — fall through to the size-unknown open instead.
                    .filter(|s| s.width > 1 && s.height > 1)
                    .map(|s| (s.width, s.height))
            });
        cmds.push(self.open_preview_spinner(
            preview::PreviewKind::Video(preview::VideoPreview::loading()),
            dims,
        ));
        Task::batch(cmds)
    }

    /// Cancel the recording: stop the worker like a normal stop, but flag it so
    /// `RecordingPoll` deletes the finalized file and exits without saving or
    /// notifying.
    pub(super) fn cancel_recording(&mut self) -> Task<cosmic::Action<Msg>> {
        self.end_recording_tray();
        self.recording_cancelled = true;
        if let Some(rec) = &self.recording {
            rec.stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Task::batch(self.destroy_surfaces())
    }

    /// Destination file for a new recording of `sel`: `<dir>/<stem>.mp4` (same
    /// `<timestamp>[-<descriptor>]` naming as screenshots).
    pub(super) fn record_output_path(&self, sel: &Selection) -> std::path::PathBuf {
        let raw = if self.record_dir.trim().is_empty() {
            "~/Capture"
        } else {
            self.record_dir.trim()
        };
        let dir = crate::util::expand_tilde(raw);
        dir.join(format!("{}.mp4", self.capture_stem(sel)))
    }
}

/// Whether the in-frame recording toolbar is HIDDEN (DRAGON-172), decided from the two
/// two facts (DRAGON-174 simplification): the user's setting AND whether the toolbar can
/// fit outside the capture.
///
/// * `hide_setting` — the "Hide toolbar on full screen captures" setting is ON.
/// * `cant_fit_outside` — the toolbar can't sit OUTSIDE the captured area (a full-screen
///   monitor capture, or a region so large the toolbar has no room around it), i.e.
///   `recording_toolbar_oversized()`.
///
/// The toolbar hides ONLY when the user asked for it AND it can't fit outside. Nothing
/// else hides it anymore — daemon-attached does NOT (the tray always carries the controls,
/// independently of this decision, DRAGON-174). Pure so the setting x fit matrix is
/// unit-testable.
fn toolbar_hidden(hide_setting: bool, cant_fit_outside: bool) -> bool {
    hide_setting && cant_fit_outside
}

/// Map a resolved [`Selection`] to its macOS SCK recording target (DRAGON-130): a
/// picked window (`window_id`, a `CGWindowID` string) records that window directly
/// (occlusion-independent); a monitor selection (`output`, a `Display-<id>` name)
/// records the whole display; anything else is a region crop. Window id wins over
/// output (a window selection never carries both, but the precedence is explicit).
#[cfg(target_os = "macos")]
pub(super) fn mac_record_target(sel: &Selection) -> crate::record::MacRecordTarget {
    if let Some(id) = sel.window_id.as_deref().and_then(|s| s.parse::<u32>().ok()) {
        crate::record::MacRecordTarget::Window(id)
    } else if let Some(name) = &sel.output {
        crate::record::MacRecordTarget::Display(name.clone())
    } else {
        crate::record::MacRecordTarget::Region
    }
}

/// Map a resolved [`Selection`] to its Windows WGC recording target (DRAGON-229): a picked
/// window (`window_id`, an `HWND` decimal string) records that window directly
/// (occlusion-independent); a monitor selection (`output`, a `\\.\DISPLAYn` name) records
/// the whole monitor; anything else is a region crop. Window id wins over output (the
/// precedence is explicit). Mirrors [`mac_record_target`]; the `HWND` id stays a STRING
/// (isize, wider than u32).
#[cfg(windows)]
pub(super) fn win_record_target(sel: &Selection) -> crate::record::WinRecordTarget {
    if let Some(id) = sel.window_id.as_deref().filter(|s| s.parse::<isize>().is_ok()) {
        crate::record::WinRecordTarget::Window(id.to_string())
    } else if let Some(name) = &sel.output {
        crate::record::WinRecordTarget::Display(name.clone())
    } else {
        crate::record::WinRecordTarget::Region
    }
}

#[cfg(test)]
mod toolbar_visibility_tests {
    use super::toolbar_hidden;

    // The simplified setting x fit matrix (DRAGON-174): the toolbar hides ONLY when the
    // "Hide toolbar on full screen captures" setting is ON AND the toolbar can't fit
    // outside the capture. Nothing else hides it — daemon-attached is irrelevant here
    // (the tray always carries the controls, independently of this decision).

    #[test]
    fn toolbar_stays_when_the_setting_is_off() {
        // Default (setting OFF): the toolbar shows even on an oversized capture (it goes
        // in-frame), and of course when it fits too.
        assert!(!toolbar_hidden(false, false));
        assert!(!toolbar_hidden(false, true));
    }

    #[test]
    fn toolbar_stays_when_it_fits_outside_even_if_hiding_is_on() {
        // Setting ON but the toolbar CAN fit outside the capture → it shows (there is
        // nothing to hide from). Hiding only kicks in when it can't fit.
        assert!(!toolbar_hidden(true, false));
    }

    #[test]
    fn toolbar_hides_only_when_hiding_is_on_and_it_cant_fit() {
        // The single hide case: the user asked to hide AND the toolbar can't fit outside
        // (a full-screen / oversized capture). The tray icon carries the controls.
        assert!(toolbar_hidden(true, true));
    }
}


#[cfg(all(test, target_os = "macos"))]
mod mac_target_tests {
    use super::mac_record_target;
    use crate::record::MacRecordTarget;
    use crate::selection::Selection;

    fn sel(output: Option<&str>, window_id: Option<&str>) -> Selection {
        Selection {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
            output: output.map(str::to_string),
            window_id: window_id.map(str::to_string),
        }
    }

    #[test]
    fn window_id_maps_to_window_target() {
        assert_eq!(
            mac_record_target(&sel(None, Some("12345"))),
            MacRecordTarget::Window(12345)
        );
    }

    #[test]
    fn output_name_maps_to_display_target() {
        assert_eq!(
            mac_record_target(&sel(Some("Display-7"), None)),
            MacRecordTarget::Display("Display-7".to_string())
        );
    }

    #[test]
    fn bare_region_maps_to_region_target() {
        assert_eq!(mac_record_target(&sel(None, None)), MacRecordTarget::Region);
    }

    #[test]
    fn window_id_wins_over_output() {
        assert_eq!(
            mac_record_target(&sel(Some("Display-7"), Some("42"))),
            MacRecordTarget::Window(42)
        );
    }

    #[test]
    fn unparseable_window_id_falls_through() {
        // A non-numeric window id can't be a CGWindowID → fall through to output/region
        // rather than silently recording the wrong thing.
        assert_eq!(
            mac_record_target(&sel(Some("Display-3"), Some("not-a-number"))),
            MacRecordTarget::Display("Display-3".to_string())
        );
        assert_eq!(mac_record_target(&sel(None, Some("nope"))), MacRecordTarget::Region);
    }
}
