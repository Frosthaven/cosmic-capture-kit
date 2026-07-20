use super::*;

impl cosmic::Application for App {
    type Executor = cosmic::executor::Default;
    /// How the app was launched (`--settings`, `--preview <file>`, or a normal capture).
    type Flags = super::Startup;
    type Message = Msg;
    const APP_ID: &'static str = "dev.frosthaven.CosmicCaptureKit";

    fn core(&self) -> &app::Core {
        &self.core
    }
    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn init(core: app::Core, startup: super::Startup) -> (Self, Task<cosmic::Action<Msg>>) {
        crate::util::timing_mark("App::init entry (iced runtime + winit + wgpu preinit done)");
        let settings_only = startup.settings_only;
        // `--permissions` (macOS): open only the permission-checker window; like
        // `--settings`/`--preview` it captures nothing. On Linux the flag is inert
        // (there is no permission window), so this stays false there and the launch
        // is byte-identical to a bare one.
        #[cfg(target_os = "macos")]
        let permissions_only = startup.permissions_only;
        #[cfg(not(target_os = "macos"))]
        let permissions_only = false;
        // `--preview <file>`: open straight into the preview overlay for an existing file
        // (image or video by extension), with no capture machinery.
        let startup_preview = startup
            .preview
            .as_ref()
            .map(|p| (p.clone(), super::preview::is_video_path(p)));
        let preview_mode = startup_preview.is_some();
        let dummy_id = window::Id::unique();
        let dummy = super::shell::bootstrap_surface(dummy_id);
        // Detect the very first launch (no state file yet) before anything writes
        // one, so we can choose smart capture-method defaults once.
        let first_launch = !crate::state::file_exists();
        let persisted = crate::state::load();
        // Recordings + meters read the mic source from this global; set it from the
        // persisted choice now so it applies even if settings is never opened.
        crate::audio::config::set_mic_source(&persisted.mic_device);
        // First-run / missing-permission routing (macOS, DRAGON-130): a capture
        // launch that lacks the Screen Recording grant would only produce a blank
        // capture, so instead of proceeding we open the dedicated permission-checker
        // window — the richer CleanShot/Rectangle-style surface with live status +
        // Request / Open Settings / Relaunch. This SUPERSEDES the old bare one-shot
        // `request_screen_capture()`: the checker fires that same prompt from its
        // Request button (and marks it spent), so the once-guard semantics are kept
        // — `mac_first_run_seen` is still the record of whether the unspent prompt is
        // available, now consulted by the card action rather than spent silently here.
        // Skipped in --settings/--preview/--permissions (they capture nothing). A bare
        // resident launch never reaches the GUI (it early-branches to the menu-bar
        // daemon in `main`), so every launch that gets here is a real capture /
        // settings / preview / permissions instance.
        // Probe every grant ONCE for a capture launch (screen preflight + mic + the
        // bundle-gated notification query), so the routing decision and the window's
        // seed share one snapshot. Skipped where it would never route anyway.
        // DRAGON-201: use the FAST probe here — it skips the notification query, which
        // blocks up to 1.5s before the overlay maps. Routing only needs the required
        // Screen Recording grant + the (non-blocking) mic status; a NotDetermined
        // notification is a soft nag the permission window re-probes live via its 1s
        // poll, so it no longer gates launch. This snapshot both drives the routing
        // decision AND seeds the window (its poll fills the Notifications card shortly
        // after open); a `--permissions` launch, which takes no snapshot here, probes
        // fully at seed time below.
        #[cfg(target_os = "macos")]
        crate::util::timing_mark("App::init -> permissions::probe_now_fast (begin)");
        #[cfg(target_os = "macos")]
        let route_probe = (!settings_only && !preview_mode && !permissions_only)
            .then(permissions::probe_now_fast);
        #[cfg(target_os = "macos")]
        crate::util::timing_mark("App::init <- permissions::probe_now_fast (done)");
        // Route to the checker when a real capture launch still has an UNADDRESSED
        // permission: Screen Recording missing (required), or an optional grant never
        // prompted (mic / notifications NotDetermined). Denied optionals don't nag —
        // `should_auto_open` owns that policy.
        #[cfg(target_os = "macos")]
        let route_to_permissions = route_probe
            .as_ref()
            .is_some_and(permissions::should_auto_open_probe);
        #[cfg(not(target_os = "macos"))]
        let route_to_permissions = false;
        // Open the permission window either on an explicit `--permissions`, or when a
        // capture launch is missing the Screen Recording grant.
        let open_permissions = permissions_only || route_to_permissions;
        let radius = window_radius();
        // Frosted-glass config (DRAGON-217), read once — theme is fixed for this
        // one-shot session. `None` off COSMIC / when frosted windows are off, which
        // keeps every window fully opaque (byte-identical, incl. macOS).
        let glass = theme::glass_config();
        // Per-session capture-scene acquisition (spawn the background window
        // pre-capture + grab the frozen full-output snapshots). A reusable free fn;
        // see `acquire_scene`'s doc for the once-per-process vs per-session split.
        // `active` mirrors the `!settings_only && !preview_mode` guard both blocks
        // shared — those launches don't capture. A permission-only / permission-
        // routed launch likewise captures nothing (the grant it needs is missing).
        // macOS startup latency (DRAGON): kick the tiling-WM (AeroSpace) pause NOW, on a
        // detached thread, so its 1-3s `enable off` overlaps the synchronous scene grab
        // below instead of running serially after init returns. `seed_outputs_mac` waits
        // on this latch before minting overlays, so they still land into a paused WM. Only
        // for real capture launches (the same guard the scene grab and seed use); a
        // no-AeroSpace machine returns fast and the latch signals immediately.
        #[cfg(target_os = "macos")]
        if !settings_only && !preview_mode && !open_permissions {
            crate::util::timing_mark("App::init -> early_pause_tiling_wm (detached, overlaps scene grab)");
            crate::platform::mac::window::early_pause_tiling_wm();
        }
        crate::util::timing_mark("App::init -> acquire_scene (begin)");
        let scene_active = !settings_only && !preview_mode && !open_permissions;
        // The launch capture mode, resolved from the CLI overrides (Scanner forces
        // Region). Computed HERE (before the App struct) so `acquire_scene` can gate the
        // ~1s window pre-capture on a WINDOW-mode launch (DRAGON-204) — every other mode
        // defers it to the first switch into window mode.
        let launch_mode = if startup.kind == Some(Kind::Scanner) {
            Mode::Region
        } else {
            startup.mode.unwrap_or(Mode::Region)
        };
        let (precapture, frozen, frozen_slot, wallpaper_slot, cursor_slot) = acquire_scene(
            scene_active,
            launch_mode,
            persisted.capture_cursor,
            persisted.freeze,
            wallpaper_path(),
            radius,
        );
        // DRAGON-204: the window-picker loading poll starts armed ONLY when the launch
        // pre-capture is actually running (a window-mode launch). A region / monitor /
        // scan launch defers the pre-capture, so it starts UNARMED — the poll is armed
        // lazily by `SetMode(Window)` when it kicks the deferred grab. A settings /
        // preview launch never captures, so it's unarmed too.
        let windows_loading = launch_precapture_runs(scene_active, launch_mode);
        // DRAGON-212: the flats grab is DEFERRED on BOTH platforms now (empty `frozen`
        // here); it lands via `FrozenReady`, so mark it pending whenever the scene is active
        // so the drain poll runs.
        let frozen_pending = scene_active;
        // macOS defers the per-output picker wallpaper too (DRAGON-200): it lands via
        // `WallpaperReady`, so arm its drain poll. Linux resolves it inline (precapture
        // tuple), so nothing is pending there.
        let wallpaper_pending = cfg!(target_os = "macos") && scene_active;
        // DRAGON-213: the dedicated launch cursor grab is in flight whenever the scene is
        // active AND "Preserve mouse cursor" is on (both platforms); its drain poll runs
        // until `CursorReady` lands it.
        let cursor_pending = scene_active && persisted.capture_cursor;
        crate::util::timing_mark("App::init <- acquire_scene (init-thread scene work done)");
        // Encoder probe is DEFERRED (DRAGON-201): resolving the usable encoders spawns
        // `ffmpeg -encoders`, a cost every launch used to pay here synchronously — even a
        // region/window/scan screenshot that never encodes. It now runs lazily the first
        // time the encoder list / preferred encoder is actually read (entering the
        // recording UI, the settings video/Health pages, or starting a recording), via
        // the `EncoderResolve` holder below. The resolution itself (probe the list,
        // dropping hardware under CCK_HEALTH_FORCE_WARN; keep the saved choice when
        // available else pick+persist the best; map record_hardware=off to software) is
        // unchanged — only WHEN it runs moved off the init critical path.
        let encoders = EncoderResolve::default();
        // The freeze DISPLAY still gates on this persisted flag (the snapshot
        // itself is always grabbed above by `acquire_scene`, so freeze + the
        // QR/OCR scanners can be toggled on/off live).
        let freeze = persisted.freeze;
        // QR/barcode + OCR both scan the current region (not the whole screen) when
        // it settles — see MarksPoll. Empty result slots until a pass finishes.
        let code_scan: std::sync::Arc<std::sync::Mutex<Option<Vec<crate::detect::Mark>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let text_scan: std::sync::Arc<std::sync::Mutex<Option<Vec<crate::detect::TextWord>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        // Settings window state. When launched via `--settings`, open the window
        // immediately (and remember to exit when it closes).
        let mut settings = SettingsState::new();
        settings.only = settings_only;
        // Appearance (DRAGON-139): seed the custom-accent picker with the persisted
        // override so opening the sidebar shows the live colour, and — when the app is
        // NOT following the system — apply the override theme now, before the first
        // view, so every surface (settings, overlay chrome, preview) launches themed.
        if let Some([r, g, b]) = persisted.appearance_accent {
            settings.accent_picker = cosmic::widget::ColorPickerModel::new(
                "Hex",
                "RGB",
                None,
                Some(cosmic::iced::Color::from_rgb(r, g, b)),
            );
        }
        // UNCONDITIONAL (DRAGON-144): the use-system path must also be applied
        // explicitly — libcosmic's own default follows cosmic-config's ThemeMode,
        // which doesn't exist off COSMIC (macOS: always dark, ignoring the real
        // system appearance). `apply_appearance` routes use-system through the
        // portable `system_is_dark` seam; on COSMIC the result is identical to
        // libcosmic's default, so Linux behavior is unchanged.
        let appearance_task = Some(theme::apply_appearance::<Msg>(
            persisted.appearance_use_system,
            persisted.appearance_mode.min(2),
            persisted.appearance_accent,
            persisted.appearance_roundness.min(2),
            persisted.appearance_contrast_boost,
        ));
        // Post-update relaunch (DRAGON-177): the installer's swap helper relaunches
        // the app bare; consuming the marker turns THIS launch into a settings
        // launch that lands on About, so the new version's notes are front and
        // center. (With the resident on, the daemon consumes the marker instead and
        // spawns a --settings child; this path covers non-resident relaunches.)
        let post_update = crate::update::take_post_update_marker();
        let settings_only = settings_only || post_update;
        // The launch settings window is minted a message-drain LATER (via
        // `OpenSettingsAtStartup` below), AFTER the appearance `set_theme` has been
        // applied, so its first paint carries the resolved accent with no flash
        // (DRAGON-268 follow-up). We only decide HERE that a settings window is wanted;
        // the id is minted in the deferred handler.
        let open_settings_at_startup = settings_only;
        // Permission-checker window state. Opened when `--permissions` was passed, or
        // when a capture launch is missing the Screen Recording grant (`open_permissions`).
        // `only` (like settings) makes closing it end the instance. The window is only
        // ever minted on macOS; `open_permissions` is always false elsewhere.
        let mut permissions = permissions::PermissionsState::default();
        let permissions_task = if open_permissions {
            permissions.only = true;
            // Seed the live-status snapshot BEFORE the first frame so the window opens
            // already showing real statuses (the poll then keeps them fresh). macOS-only.
            // Reuse the capture-launch probe when it was taken (routed open); that fast
            // probe carries no notification status (DRAGON-201), but the window's 1s poll
            // fills the Notifications card in right after open. A `--permissions` launch
            // took no snapshot here, so probe FULLY now (it can afford the block: the
            // launch is explicit and opens straight into the window).
            #[cfg(target_os = "macos")]
            {
                permissions.probe = route_probe.unwrap_or_else(permissions::probe_now);
            }
            let (id, task) = permissions::open_permissions_window();
            permissions.window = Some(id);
            Some(task)
        } else {
            None
        };
        // Live keyboard map = defaults with the persisted overrides applied.
        let mut keymap = crate::shortcuts::Keymap::defaults();
        keymap.apply_overrides(&persisted.shortcuts);
        let ffmpeg_available = crate::encode::ffmpeg_available();
        // Benchmark monitor dropdown (DRAGON-163): enumerate connected monitors + their
        // TRUE capture footprint once, only for the settings window (the sole place the
        // benchmark lives; a capture launch never needs this and must not pay the probe).
        // Default the selection to the LARGEST monitor — the one most likely to stress the
        // encoder (the case DRAGON-162 was about). Session-only; not persisted.
        let bench_monitors = if settings_only {
            crate::screenshot::bench_monitors()
        } else {
            Vec::new()
        };
        let bench_monitor_idx = bench_monitors
            .iter()
            .enumerate()
            .max_by_key(|(_, m)| m.px_w as u64 * m.px_h as u64)
            .map(|(i, _)| i)
            .unwrap_or(0);
        crate::util::timing_mark("App::init returning (App struct built, tasks batched)");
        (
            App {
                core,
                outputs: Vec::new(),
                // CLI overrides (`--region`/`--window`/`--monitor`, `--image`/
                // `--video`/`--scan`, `--countdown`) fall back to the defaults. A
                // Scanner kind forces Region mode (its capture invariant).
                kind: startup.kind.unwrap_or(Kind::Image),
                mode: launch_mode,
                // A CLI `--countdown` seconds value overrides the delay exactly (see
                // countdown_override); delay_idx still points at the nearest preset so
                // the delay menu is sensible if the override is later cleared.
                delay_idx: startup
                    .countdown_secs
                    .map(super::countdown_index)
                    .unwrap_or(persisted.delay_idx)
                    .min(DELAYS.len() - 1),
                countdown_override: startup.countdown_secs,
                region: persisted.region.map(GlobalRect::from_tuple),
                region_dragging: false,
                toolbar_offset: HashMap::new(),
                windows: HashMap::new(),
                // Armed only for a window-mode launch (its pre-capture is running);
                // deferred launches arm it lazily on the first switch to window mode
                // (DRAGON-204).
                windows_loading,
                window_warmup: 0,
                precapture,
                // Kicked at launch only for a window-mode launch; a deferred launch
                // kicks it lazily on the first switch into window mode (DRAGON-204).
                window_precapture_started: windows_loading,
                // Random per launch (no rng dep): seed from the clock's subsec nanos.
                loading_msg: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() as usize)
                    .unwrap_or(0)
                    % overlay::LOADING_MESSAGES.len(),
                hover: Hover::None,
                hovered_output: None,
                delay_menu_open: false,
                countdown: None,
                pending: None,
                capturing: None,
                window_spinner_neutral: false,
                windowed_swap_pending: false,
                grab_overlay_closing: None,
                #[cfg(target_os = "macos")]
                mac_preview_preopen: false,
                #[cfg(windows)]
                settings_shown_confirmed: false,
                #[cfg(windows)]
                preview_shown_confirmed: None,
                settings,
                permissions,
                keymap,
                preview_after_capture: persisted.preview_after_capture,
                copy_selection_pending: false,
                // `--preview` (windowed unless `--overlay`) overrides the persisted setting.
                preview_windowed: startup.preview_windowed.unwrap_or(persisted.preview_windowed),
                auto_close_preview: persisted.auto_close_preview,
                preview_float_cosmic: persisted.preview_float_cosmic,
                mute_others_during_preview: persisted.mute_others_during_preview,
                duck_system_audio: persisted.duck_system_audio,
                appearance_use_system: persisted.appearance_use_system,
                appearance_mode: persisted.appearance_mode.min(2),
                appearance_accent: persisted.appearance_accent,
                appearance_roundness: persisted.appearance_roundness.min(2),
                appearance_contrast_boost: persisted.appearance_contrast_boost,
                selection_box_thickness: persisted.selection_box_thickness.clamp(1, 8),
                preview: None,
                preview_duck: None,
                preview_output: None,
                preview_output_scale: 1.0,
                startup_preview,
                preview_mode,
                settings_size: persisted.settings_size,
                ffmpeg_available,
                ffprobe_available: crate::encode::ffprobe_available(),
                tesseract_langs: std::cell::OnceCell::new(),
                pactl_available: crate::audio::devices::pactl_available(),
                capture_cursor: persisted.capture_cursor,
                capture_transparency: persisted.capture_transparency,
                capture_wallpaper: !persisted.no_wallpaper,
                active_border_color: persisted.active_border_color,
                active_border_width: persisted.active_border_width.min(10),
                inactive_border_color: persisted.inactive_border_color,
                inactive_border_width: persisted.inactive_border_width.min(10),
                window_drop_shadow: persisted.window_drop_shadow,
                window_single_active: persisted.window_single_active,
                window_transparency_multiplier: persisted.window_transparency_multiplier.clamp(0.0, 1.0),
                window_padding: persisted.window_padding,
                window_padding_px: NumField::new(persisted.window_padding_px),
                allow_multiple: persisted.allow_multiple,
                resident: persisted.resident,
                capture_hotkey: persisted.capture_hotkey.clone(),
                #[cfg(target_os = "macos")]
                aerospace_guard: None,
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                passthrough_active: false,
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                passthrough_solid: None,
                region_overlay_opacity: persisted.region_overlay_opacity,
                active_overlay_opacity: persisted.active_overlay_opacity,
                preview_overlay_opacity: persisted.preview_overlay_opacity,
                record_fps: NumField::new(persisted.record_fps.clamp(1, 240)),
                record_bitrate_kbps: NumField::new(persisted.record_bitrate_kbps.clamp(100, 500_000)),
                record_res_preset: persisted.record_res_preset.min(RES_CUSTOM as u8),
                record_max_width: NumField::new(persisted.record_max_width.clamp(2, 8192)),
                record_max_height: NumField::new(persisted.record_max_height.clamp(2, 8192)),
                nvenc_preset: persisted.nvenc_preset,
                x264_preset: persisted.x264_preset,
                vaapi_compression_level: persisted.vaapi_compression_level.clamp(-1, 6),
                record_zero_copy: persisted.record_zero_copy,
                record_codec: persisted.record_codec,
                audio_sync_offset_ms: NumField::new(persisted.audio_sync_offset_ms.clamp(-1000, 1000)),
                audio_sync_auto: persisted.audio_sync_auto,
                av_calibration_base_ms: persisted.av_calibration_base_ms.clamp(-2000, 2000),
                record_dir: persisted.record_dir,
                encoders,
                // Resolved here (off the view; the probe underneath is cached) so the
                // Health page reads a plain bool.
                nvenc_driver_mismatch: crate::encode::nvenc_driver_mismatch(),
                bench: None,
                bench_monitors,
                bench_monitor_idx,
                scan_codes: persisted.scan_codes,
                scan_text: persisted.scan_text,
                text_confidence: persisted.text_confidence.clamp(0.0, 60.0),
                tesseract_available: crate::detect::tesseract_available(),
                code_scan,
                text_scan,
                code_busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                ocr_busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                last_code_region: None,
                last_ocr_region: None,
                code_marks: Vec::new(),
                marks: Vec::new(),
                hovered_mark: None,
                text_words: Vec::new(),
                hovered_word: None,
                text_sel: std::collections::BTreeSet::new(),
                text_menu: None,
                code_menu: None,
                text_drag: None,
                recording: None,
                recording_started: None,
                recording_path: None,
                recording_paused_at: None,
                recording_paused_accum: std::time::Duration::ZERO,
                recording_cancelled: false,
                hide_toolbar_fullscreen: persisted.hide_toolbar_fullscreen,
                tray: None,
                tray_hides_toolbar: false,
                push_to_talk: persisted.push_to_talk,
                ptt_held: false,
                hotkeys: None,
                screenshot_dir: persisted.screenshot_dir,
                copy_to_clipboard: persisted.copy_to_clipboard,
                clipboard_max_mb: NumField::new(persisted.clipboard_max_mb.max(1)),
                record_mic: persisted.record_mic,
                record_system_audio: persisted.record_system_audio,
                covermark_text: persisted.covermark_text,
                covermark_zoom: persisted.covermark_zoom,
                covermark_opacity: persisted.covermark_opacity,
                covermark_prefs: persisted
                    .covermark_prefs
                    .into_iter()
                    .map(|p| (p.key, (p.zoom, p.opacity)))
                    .collect(),
                mic_level: 0.0,
                sys_level: 0.0,
                sens_level: 0.0,
                mic_chain: None,
                sys_meter: None,
                #[cfg(any(target_os = "macos", windows))]
                sys_idle_meter: None,
                noise_reduction: persisted.noise_reduction,
                mic_device: persisted.mic_device.clone(),
                mic_devices: Vec::new(),
                mic_device_labels: vec!["System (automatic)".to_string()],
                echo_cancellation: persisted.echo_cancellation,
                speaker_device: persisted.speaker_device.clone(),
                speaker_devices: Vec::new(),
                speaker_device_labels: vec!["System (automatic)".to_string()],
                input_sensitivity_auto: persisted.input_sensitivity_auto,
                input_sensitivity: persisted.input_sensitivity,
                auto_gain: persisted.auto_gain,
                advanced_vad: persisted.advanced_vad,
                mic_test: None,
                mic_test_modal_open: false,
                window_radius: radius,
                glass,
                wallpaper_handles: HashMap::new(),
                origin_window: None,
                frozen_win_px: HashMap::new(),
                frozen_toplevels: Vec::new(),
                active_win_px: HashMap::new(),
                frozen_cursor: None,
                frozen_cursor_handle: None,
                freeze,
                frozen,
                frozen_slot,
                frozen_pending,
                wallpaper_slot,
                wallpaper_pending,
                cursor_slot,
                cursor_pending,
                capture_live: false,
                record_backend: persisted.record_backend.clone(),
                screenshot_backend: persisted.screenshot_backend.clone(),
                // Portal availability is still unknown (the probe is async); the
                // probe handler rebuilds these once it lands.
                screenshot_methods: crate::platform::backend::method_choices(
                    false,
                    ffmpeg_available,
                    |c| c.screenshot,
                ),
                record_methods: crate::platform::backend::method_choices(
                    false,
                    ffmpeg_available,
                    |c| c.record,
                ),
                first_launch,
                pipewire_available: false,
                pipewire_source_types: 0,
                toast: None,
                pw_restore_token: persisted.pw_restore_token,
                pw_pending: None,
                pw_slot: std::sync::Arc::new(std::sync::Mutex::new(None)),
                pw_held: None,
                update_status: crate::update::UpdateStatus::Unknown,
                update_installing: false,
                notify_updates: persisted.notify_updates,
                update_dialog: None,
                update_dialog_decided: false,
                update_notes: None,
            },
            {
                let mut tasks = vec![
                    dummy,
                    // Probe the ScreenCast portal in the background; the result fills
                    // in the recording-tab indicator and gates the PipeWire path.
                    Task::perform(probe_pipewire(), |(ok, types)| {
                        cosmic::Action::App(Msg::Capture(CaptureMsg::PipewireProbed(ok, types)))
                    }),
                ];
                // Appearance override (DRAGON-139): when not following the system, apply
                // the persisted theme before the first frame so the app launches themed.
                if let Some(t) = appearance_task {
                    tasks.push(t);
                }
                // `--settings`: open the settings window as part of startup — but a
                // message-drain AFTER `appearance_task` above, so the window's FIRST paint
                // already carries the launch-resolved accent instead of flashing
                // libcosmic's default for a frame (DRAGON-268 follow-up). This
                // `Task::done` and the `set_theme` from `appearance_task` are both queued
                // here from `init`; a single `update` drain processes them in ORDER — the
                // theme's global mutation lands first, then `OpenSettingsAtStartup`'s
                // handler returns the window-open task, so `WindowCreated` reads the
                // correct global theme when it builds the window's state. The update-cache
                // seed / check / About routing move INTO that handler (they were batched
                // alongside the eager open before; ordering vs the window paint is
                // unchanged, they just ride the same deferred open).
                if open_settings_at_startup {
                    // Land on About after an update install (marker consumed above, or
                    // the daemon spawned us with CCK_SETTINGS_TAB=about).
                    let about = post_update
                        || std::env::var(crate::update::SETTINGS_TAB_ENV)
                            .map(|v| v.eq_ignore_ascii_case("about"))
                            .unwrap_or(false);
                    tasks.push(Task::done(cosmic::Action::App(Msg::WindowChrome(
                        super::WindowChromeMsg::OpenSettingsAtStartup { about },
                    ))));
                }
                // `--permissions` / missing-grant routing: open the permission window.
                if let Some(t) = permissions_task {
                    tasks.push(t);
                }
                // macOS/Windows have no Wayland output-event subscription, so seed the
                // per-display capture overlays (or the `--preview` window) once here —
                // the PlainWindows equivalent of the `on_output` hotplug path. The
                // handler applies the same `--settings`/`--preview` guards `on_output` does.
                #[cfg(not(target_os = "linux"))]
                tasks.push(Task::done(cosmic::Action::App(Msg::WindowChrome(
                    super::WindowChromeMsg::SeedOutputs,
                ))));
                // No per-session idle icon anymore (DRAGON-182): the app's own status
                // icon exists ONLY while a recording is live (`begin_recording_tray`);
                // a resident, when enabled, owns the one always-present tray icon.
                Task::batch(tasks)
            },
        )
    }

    fn view(&self) -> Element<'_, Msg> {
        unimplemented!()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Msg> {
        // The settings window is its own toplevel, rendered independently of the
        // per-output capture overlays.
        if Some(id) == self.settings.window {
            return self.config_window_view();
        }
        if Some(id) == self.permissions.window {
            return self.permissions_window_view();
        }
        // DRAGON-216 (Linux windowed): the pre-opened neutral overlay whose preview was just
        // swapped to a real window keeps painting its loading cover until its close lands, so
        // the window maps under it with no desktop flash.
        if self.grab_overlay_closing == Some(id) {
            return self.grab_cover_view();
        }
        if self.is_preview_window(id) {
            return self.preview_view();
        }
        if let Some(o) = self.outputs.iter().find(|o| o.id == id) {
            if self.recording.is_some() {
                self.recording_view(o)
            } else if self.countdown.is_some() {
                self.countdown_view(o)
            } else {
                self.overlay_view(o)
            }
        } else {
            widget::space::Space::new()
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    }

    fn update(&mut self, message: Msg) -> Task<cosmic::Action<Msg>> {
        match message {
            Msg::Capture(message) => self.update_capture(message),
            Msg::Recording(message) => self.update_recording(message),
            Msg::Detect(message) => self.update_detect(message),
            Msg::Settings(message) => self.update_settings(message),
            Msg::Permissions(message) => self.update_permissions(message),
            Msg::WindowChrome(message) => self.update_window_chrome(message),
            Msg::Preview(message) => self.update_preview(message),
        }
    }

    fn subscription(&self) -> Subscription<Msg> {
        // Body lives in subscriptions.rs as small named `sub_*` helpers, one per
        // trigger condition (see that file for what drives each tick and why).
        self.subscriptions()
    }
}
