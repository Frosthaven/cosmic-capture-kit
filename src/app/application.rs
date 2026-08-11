use super::*;

/// The two launch marks that bracket the span `App::init` hands BACK to the toolkit.
///
/// Between `App::init returning` and the first thing this app does again, control belongs
/// entirely to iced/libcosmic/winit/wgpu: the returned `Task` batch is spawned, the
/// bootstrap surface is created, and the wgpu compositor (instance, adapter, device,
/// surface, first pipelines) is built lazily around it. Until these marks existed that was
/// one unlabelled hole in the timeline, and it is the largest single block in a macOS
/// capture launch: the last mark on a wedged launch was `App::init returning`, which cannot
/// distinguish "the toolkit never came back" from "our own first update never finished".
///
/// Two marks split it, because the toolkit hands control back at two different seams and
/// which one comes first is itself the diagnosis:
///
/// * **first `view_window`** — the compositor exists and is asking us for a scene. Time
///   spent BEFORE this is toolkit/GPU bring-up and is not ours.
/// * **first `update`** — the message loop is draining. `init`'s batched `SeedOutputs` is
///   normally the message that lands here, so this is when the overlay minting can start.
///
/// **What they measured on macOS**, warm, region capture, so the next person does not have
/// to re-derive it: ~65ms from `init` returning to the first `view_window` (wgpu compositor
/// bring-up), ~9ms more to the first `update`, then a further **~59ms before the SECOND
/// message drain**, and only then can the overlays be minted. That 59ms is NOT work we hand
/// the toolkit, and two A/B runs proved it. Dropping `set_theme` out of `init`'s task batch
/// moved it by 0.1ms (59.8 → 59.7). Moving `SeedOutputs` to the FRONT of the batch drained
/// it 59ms earlier and changed first paint by nothing (335.6 → 340.8ms): the follow-up
/// `SeedOverlays` simply inherited the same stall. In both runs the queue unblocked at the
/// exact moment the bootstrap `window::open` reported back, which is what the stall is —
/// the message loop blocked behind the 1x1 bootstrap window's creation and first present,
/// a window libcosmic's `no_main_window(true)` requires us to open. Do not spend another
/// afternoon re-testing task ORDER; the order is not the variable.
///
/// One-shot on purpose: `update` and `view_window` run on every frame and every message, and
/// a mark per call would bury the launch timeline in its own noise. `Once::call_once` is a
/// single relaxed atomic load on the (overwhelmingly common) already-called path, which is
/// the same order of cost the marks themselves already pay when logging is off.
fn mark_first_update() {
    static FIRST: std::sync::Once = std::sync::Once::new();
    FIRST.call_once(|| {
        crate::util::timing_mark("App::update FIRST call (toolkit message loop draining)");
    });
}

/// The view half of [`mark_first_update`]; see that doc for why both exist.
fn mark_first_view() {
    static FIRST: std::sync::Once = std::sync::Once::new();
    FIRST.call_once(|| {
        crate::util::timing_mark("App::view_window FIRST call (wgpu compositor up, asking for a scene)");
    });
}

impl cosmic::Application for App {
    type Executor = cosmic::executor::Default;
    /// How the app was launched (`--settings`, `--preview <file>`, or a normal capture).
    type Flags = super::Startup;
    type Message = Msg;
    const APP_ID: &'static str = "dev.thedragon.CosmicCaptureKit";

    fn core(&self) -> &app::Core {
        &self.core
    }
    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn init(mut core: app::Core, startup: super::Startup) -> (Self, Task<cosmic::Action<Msg>>) {
        crate::util::timing_mark("App::init entry (iced runtime + winit + wgpu preinit done)");
        // DRAGON-602: opt every surface OUT of the toolkit's AUTOMATIC backdrop blur,
        // before any surface is created. libcosmic enrolls each new surface from the
        // user's frosted-windows theme preference alone, which for a capture tool means
        // the compositor frosts the live desktop behind our fullscreen overlay and the
        // user is shown a picture of their screen that is not their screen. The rule and
        // the whole mechanism are documented on `toolkit_auto_glass`. Glass we ask for
        // ourselves is untouched: the colour picker and permission windows still pass
        // `window::Settings.blur`, and `crate::glass` still reproduces frost in captures.
        core.set_auto_blur(crate::app::theme::toolkit_auto_glass());
        // DRAGON-467 review, major 3: bound the disk-backed transient folder by AGE. A
        // recording the user never saved has to outlive this process (the Linux clipboard
        // worker serves it as a `file://` URI), so it cannot be cleaned up on close; a sweep
        // at launch is what keeps the folder from growing forever. Detached because it is a
        // directory walk and nothing here waits on it, on a plain OS thread like every other
        // "this touches the disk, get it off the loop" worker in the app.
        std::thread::spawn(crate::util::sweep_transient_recordings);
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
        // DRAGON-427 (Windows 10): a `--preview-handoff` launch is a preview launch too —
        // same suppression of every capture surface, just fed from the wire fields instead
        // of a bare path.
        let startup_handoff = startup.preview_handoff.clone();
        let preview_mode = startup_preview.is_some() || startup_handoff.is_some();
        let dummy_id = window::Id::unique();
        let dummy = super::shell::bootstrap_surface(dummy_id);
        let persisted = crate::state::load();
        // DRAGON-309: snapshot the TRIGGER display's NAME NOW, at the very top of init — before
        // the picker overlay is shown, before the user moves the cursor to the target monitor to
        // draw a region / pick a window, and before our layer-shell overlay grabs focus. This is
        // the monitor active when the capture was initiated; sampling it at capture COMMIT would
        // read the TARGET monitor instead. The daemon press-time env (CCK_ACTIVE_DISPLAY) wins
        // off Linux; on Linux it's the focused toplevel's output. Resolved to a rect at commit.
        let trigger_display = crate::platform::snapshot_trigger_display_name();
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
        // DRAGON-440: this snapshot is normally ALREADY TAKEN — `app::run`'s macOS preamble
        // needs it before the boot-time activation policy is set, and hands it over on
        // `Startup::route_probe`. Consuming it here rather than re-probing is what keeps the
        // policy decision and the routing decision reading ONE snapshot, with no window
        // between them for a grant to change under us. The `or_else` is belt-and-braces for
        // a caller that did not fill it in, and reproduces the original guard exactly
        // (`opens_overlays()` IS `!settings_only && !preview_mode && !permissions_only`).
        #[cfg(target_os = "macos")]
        crate::util::timing_mark("App::init -> permissions probe (normally already taken in app::run)");
        // DRAGON-443: `boot_post_update` mirrors `run`'s gate exactly. A post-update relaunch
        // becomes a settings launch below, so it must not probe here either — this fallback
        // would otherwise re-introduce, on the one path `run` deliberately skips, precisely
        // the routed-permission open that would hide the release notes.
        //
        // Read into a local BEFORE the `or_else`: that expression moves `route_probe` out of
        // `startup`, and reading another field from inside the closure afterwards would lean
        // on closure capture-disjointness to stay legal. One `bool` copy costs nothing and
        // owes the borrow checker no argument.
        #[cfg(target_os = "macos")]
        let boot_post_update = startup.post_update;
        #[cfg(target_os = "macos")]
        let route_probe = startup.route_probe.or_else(|| {
            (!settings_only && !preview_mode && !permissions_only && !boot_post_update)
                .then(permissions::probe_now_fast)
        });
        #[cfg(target_os = "macos")]
        crate::util::timing_mark("App::init <- permissions probe (done)");
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
        // DRAGON-431 BELT ONE (floor lowered to 13.0 by DRAGON-444): can this macOS capture at
        // all?
        //
        // We install on 13 (`LSMinimumSystemVersion` is 13.0 and stays there). DRAGON-444
        // added the pre-14 capture path (a one-shot `SCStream` grab plus CoreGraphics sizing in
        // place of `SCScreenshotManager` / `SCShareableContent.infoForFilter:`, the two 14.0+
        // selectors — the latter HARD-ABORTS the process on 13 with no exception handler), so
        // capture now works down to 13.0. This guard only stops macOS 12 and older, where the
        // audio-capture selectors the recording pipeline needs do not exist; such a launch
        // must say why instead of vanishing.
        //
        // Decided HERE, at the top of init, because everything below it is what fires those
        // calls: the AeroSpace pause, then `acquire_scene`'s window pre-capture / frozen
        // flats / wallpaper threads, then `seed_outputs_mac`'s `output_descs`. Suppressing
        // the scene the way a permission-routed launch does keeps every one of them from
        // starting; `seed_outputs_mac` then reports the failure and shows the dialog (it has
        // the `&mut self` and the `Task` this does not). `mac::shareable_content_uncached`
        // is belt two.
        //
        // `os_version` is safe to ask: `NSProcessInfo.operatingSystemVersion` is 10.10+, so
        // the probe itself can never be the thing that aborts. `capture_supported` is the ONE
        // seam all three guards ask, so the `CCK_TEST_FORCE_UNSUPPORTED_OS` review aid cannot
        // be honoured here and forgotten somewhere else.
        #[cfg(target_os = "macos")]
        let unsupported_os = !crate::platform::mac::capture_supported();
        #[cfg(not(target_os = "macos"))]
        let unsupported_os = false;
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
        if !settings_only && !preview_mode && !open_permissions && !unsupported_os {
            crate::util::timing_mark("App::init -> early_pause_tiling_wm (detached, overlaps scene grab)");
            crate::platform::mac::window::early_pause_tiling_wm();
        }
        crate::util::timing_mark("App::init -> acquire_scene (begin)");
        // DRAGON-431: `unsupported_os` joins the same suppression a permission-routed launch
        // uses. Nothing this gates is optional on macOS 13 — every one of the scene threads
        // reaches ScreenCaptureKit, and reaching it there aborts the process.
        let scene_active =
            !settings_only && !preview_mode && !open_permissions && !unsupported_os;
        // The launch capture mode, resolved from the CLI overrides (Scanner forces
        // Region). Computed HERE (before the App struct) so `acquire_scene` can gate the
        // ~1s window pre-capture on a WINDOW-mode launch (DRAGON-204) — every other mode
        // defers it to the first switch into window mode.
        let launch_mode = if startup.kind == Some(Kind::Scanner) {
            Mode::Region
        } else {
            // DRAGON-295: an immediate capture pins the pipeline mode to match its target
            // (active-window → Window so the window-capture path runs; active-monitor →
            // Monitor) even though no overlay is shown. Otherwise the explicit `--mode` /
            // the default Region applies.
            match startup.immediate {
                Some(ImmediateCapture::ActiveWindow) => Mode::Window,
                Some(ImmediateCapture::ActiveMonitor) => Mode::Monitor,
                _ => startup.mode.unwrap_or(Mode::Region),
            }
        };
        // DRAGON-336: the launch KIND gates the frozen-flats grab. Bound here rather than
        // inline in the call because DRAGON-600 needs the same answer twice: once to gate
        // the grab, once to decide whether a tray-menu launch holds it.
        let want_flats = launch_flats_needed(
            scene_active,
            persisted.freeze,
            startup.kind.unwrap_or(Kind::Image),
            startup.color_picker,
        );
        let (precapture, frozen, frozen_slot, wallpaper_slot, cursor_slot) = acquire_scene(
            scene_active,
            launch_mode,
            // DRAGON-582: a colour-picker launch draws no captured cursor, so it does not
            // pay for the launch cursor grab even when "Preserve mouse cursor" is on.
            launch_cursor_needed(persisted.capture_cursor, startup.color_picker),
            persisted.freeze,
            // DRAGON-336: the launch KIND gates the frozen-flats grab — the QR/OCR
            // scanners are a non-freeze reader of the flats, so a `--scan` launch must
            // grab them even with freeze off. DRAGON-582 added the third reader: the
            // colour picker samples them for every pointer move, so it always needs them
            // (`app::color_picker`'s `PixelSource`).
            want_flats,
            wallpaper_path(),
            radius,
        );
        // DRAGON-204: the window-picker loading poll starts armed ONLY when the launch
        // pre-capture is actually running (a window-mode launch). A region / monitor /
        // scan launch defers the pre-capture, so it starts UNARMED — the poll is armed
        // lazily by `SetMode(Window)` when it kicks the deferred grab. A settings /
        // preview launch never captures, so it's unarmed too.
        let precapture_running = launch_precapture_runs(scene_active, launch_mode);
        // DRAGON-212: the flats grab is DEFERRED on BOTH platforms now (empty `frozen`
        // here); it lands via `FrozenReady`, so mark it pending whenever the scene is active
        // so the drain poll runs.
        let frozen_pending = scene_active;
        // DRAGON-600 (Linux, tray-menu launches only): the grab above was HELD, because
        // the dropdown that launched us is still on screen and only this process taking
        // keyboard focus will retire it. Arm the hold so the overlay paints nothing and
        // the tick releases the grab once that focus lands (or the budget expires).
        let menu_hold = menu_flats_held(want_flats).then(|| crate::app::MenuFlatsHold {
            want_cursor: launch_cursor_needed(persisted.capture_cursor, startup.color_picker),
            armed: std::time::Instant::now(),
            focused: None,
        });
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
        // time the encoder list / displayed encoder is actually read (entering the
        // recording UI or the settings video/Health pages), via the `EncoderResolve`
        // holder below. Since DRAGON-571 a recording start does not read the list at
        // all: it carries the persisted intent ("auto" or the user's pick) plus the
        // last-known-good hint, and the worker's hint-first ladder resolves it. The
        // display resolution is never persisted; only a real picker click writes
        // `preferred_encoder` (the legacy record_hardware=off still maps to software).
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
        // the app bare; the marker turns THIS launch into a settings launch that lands
        // on About, so the new version's notes are front and center. (With the resident
        // on, the daemon consumes the marker instead and spawns a --settings child; this
        // path covers non-resident relaunches.)
        //
        // DRAGON-443: on macOS the marker was already CONSUMED in `app::run`'s preamble,
        // which needed the answer before the boot-time activation policy was set, and handed
        // it down here. Reading the field rather than taking again is what keeps the marker
        // single-shot AND keeps this decision identical to the one the policy was based on.
        // Off macOS there is no preamble, so it is taken right here, exactly as before.
        #[cfg(target_os = "macos")]
        let post_update = boot_post_update;
        #[cfg(not(target_os = "macos"))]
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
        // DRAGON-322: is another instance already recording? A cheap runtime-dir scan at
        // launch; drives the video-kind disable + the initial kind coercion below. A
        // settings/preview launch never shows the capture kind toggle, so the value is
        // simply unused there.
        let external_recording = crate::instance::any_other_recording();
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
        // DRAGON-559: the audio arms this launch starts with — a `--audio <channels>`
        // override when given, else the persisted pair (pure decision, unit-tested in
        // `recording_ui`). In-memory only: nothing here writes an override back, and only
        // a user's own later toggle persists, as any toggle does.
        let launch_arms = crate::recording_ui::launch_audio_arms(
            startup.audio_arms,
            (persisted.record_mic, persisted.record_system_audio),
        );
        crate::util::timing_mark("App::init returning (App struct built, tasks batched)");
        (
            App {
                core,
                outputs: Vec::new(),
                // CLI overrides (`--region`/`--window`/`--monitor`, `--image`/
                // `--video`/`--scan`, `--countdown`) fall back to the defaults. A
                // Scanner kind forces Region mode (its capture invariant). DRAGON-322:
                // a video kind is coerced to image when another instance is already
                // recording (only one recording at a time), so a `--video` launch (or the
                // persisted default) can't start a second recording alongside one.
                kind: {
                    let k = startup.kind.unwrap_or(Kind::Image);
                    if k == Kind::Video && external_recording {
                        Kind::Image
                    } else {
                        k
                    }
                },
                external_recording,
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
                // (DRAGON-204). QUIET rather than spinner-up: the spinner is not drawn
                // unless the pre-capture outlasts the reveal threshold (DRAGON-645), so a
                // fast window-mode launch goes straight to the picker.
                picker_load: if precapture_running {
                    overlay::PickerLoad::Quiet { ticks: 0 }
                } else {
                    overlay::PickerLoad::Idle
                },
                // Nothing is on screen yet, which is the whole point: the reveal threshold
                // does not begin until the picker has actually drawn (DRAGON-645).
                picker_painted: std::cell::Cell::new(false),
                precapture,
                // Kicked at launch only for a window-mode launch; a deferred launch
                // kicks it lazily on the first switch into window mode (DRAGON-204).
                window_precapture_started: precapture_running,
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
                win_preview_preopen: false,
                #[cfg(windows)]
                settings_shown_confirmed: false,
                #[cfg(windows)]
                settings_size_ready: false,
                #[cfg(windows)]
                preview_shown_confirmed: None,
                #[cfg(windows)]
                overlay_finalize_pending: false,
                #[cfg(windows)]
                overlay_finalize_deadline: None,
                settings,
                permissions,
                // DRAGON-582: the colour picker's state. `active` is what makes this
                // process the picker; the recents come off the persisted list, dropping
                // any entry that no longer parses rather than failing the whole load.
                color_picker: color_picker::ColorPickerState {
                    active: startup.color_picker,
                    recents: persisted
                        .recent_colors
                        .iter()
                        .filter_map(|s| crate::color::ColorFormat::Hex.parse(s))
                        .take(color_picker::geom::RECENTS_CAP)
                        .collect(),
                    // DRAGON-615: the lens opens where the user last left it, clamped into
                    // this build's bounds rather than applied as stored. This is the launch
                    // path a picker actually takes, so the read has to happen HERE and not
                    // only in `apply_persisted`, which runs for a reset or a reload.
                    zoom: color_picker::geom::zoom_from_persisted(persisted.color_picker_zoom),
                    // DRAGON-630: the remembered value mode, same launch-path reasoning.
                    // Junk resolves to hex, the tool's historical copy. The value row's
                    // split-vs-whole layout is remembered the same way.
                    mode: crate::color::ColorFormat::from_id(&persisted.color_picker_mode)
                        .unwrap_or(crate::color::ColorFormat::Hex),
                    split_inputs: persisted.color_picker_split_inputs,
                    ..Default::default()
                },
                keymap,
                // DRAGON-428: `--no-editor`, carried through from the launch flags.
                no_editor: startup.no_editor,
                // DRAGON-479: armed only by the in-overlay region-copy chord, never at launch.
                copy_selection_pending: false,
                // `--preview` (windowed unless `--overlay`) overrides the persisted setting;
                // DRAGON-427 then has the last word on Windows 10 (see `effective_preview_windowed`).
                preview_windowed: super::effective_preview_windowed(
                    startup.preview_windowed.unwrap_or(persisted.preview_windowed),
                    startup.opens_overlays(),
                    crate::platform::overlay_preview_available(),
                    crate::platform::software_overlays(),
                ),
                preview_toolbar_labels: persisted.preview_toolbar_labels,
                // DRAGON-419: the mirrored setting only. `diag::init` already resolved the
                // SINK from this same key back in `main` (before this window, before the
                // daemon branch), so there is nothing to start here.
                debug_logging: persisted.debug_logging,
                preview_copy_on_exit: persisted.preview_copy_on_exit,
                preview_save_originals: persisted.preview_save_originals,
                preview_ask_to_save: persisted.preview_ask_to_save,
                preview_video_copy_on_exit: persisted.preview_video_copy_on_exit,
                preview_video_save_originals: persisted.preview_video_save_originals,
                preview_video_ask_to_save: persisted.preview_video_ask_to_save,
                mute_others_during_preview: persisted.mute_others_during_preview,
                duck_system_audio: persisted.duck_system_audio,
                appearance_use_system: persisted.appearance_use_system,
                appearance_mode: persisted.appearance_mode.min(2),
                appearance_accent: persisted.appearance_accent,
                appearance_roundness: persisted.appearance_roundness.min(2),
                appearance_contrast_boost: persisted.appearance_contrast_boost,
                selection_box_thickness: persisted.selection_box_thickness.clamp(1, 8),
                previews: Vec::new(),
                focused_preview: None,
                focused_window: None,
                #[cfg(target_os = "linux")]
                portal_origin_output: None,
                capture_preview: None,
                preview_duck: None,
                preview_duck_refs: Default::default(),
                // Bound lazily, at the first preview mint (`preview_surface_for`) — a
                // capture that never opens a preview never hosts.
                #[cfg(any(unix, windows))]
                handoff_host: None,
                // Bound lazily too, at record start (`begin_recording_tray`): a session
                // that never records is never reachable by a recording command.
                #[cfg(target_os = "linux")]
                record_control: None,
                trigger_display,
                preview_output: None,
                #[cfg(target_os = "linux")]
                preview_output_name: None,
                #[cfg(target_os = "linux")]
                capture_pointer_output: None,
                preview_output_scale: 1.0,
                preview_open_size: None,
                startup_preview,
                startup_handoff,
                preview_mode,
                startup_immediate: startup.immediate,
                #[cfg(target_os = "linux")]
                immediate_kicked: false,
                settings_size: persisted.settings_size,
                ffmpeg_available,
                ffprobe_available: crate::encode::ffprobe_available(),
                tesseract_langs: std::cell::OnceCell::new(),
                pactl_available: crate::audio::devices::pactl_available(),
                capture_cursor: persisted.capture_cursor,
                capture_transparency: persisted.capture_transparency,
                capture_wallpaper: !persisted.no_wallpaper,
                window_recompositing: persisted.window_recompositing,
                active_border_color: persisted.active_border_color,
                active_border_width: persisted.active_border_width.min(10),
                inactive_border_color: persisted.inactive_border_color,
                inactive_border_width: persisted.inactive_border_width.min(10),
                window_drop_shadow: persisted.window_drop_shadow,
                window_single_active: persisted.window_single_active,
                window_transparency_multiplier: persisted.window_transparency_multiplier.clamp(0.0, 1.0),
                window_padding: persisted.window_padding,
                window_padding_px: NumField::new(persisted.window_padding_px),
                resident: persisted.resident,
                autostart_on_login: persisted.autostart_on_login,
                // DRAGON-628: UNOBSERVED, deliberately, rather than probed here. Every launch
                // is a one-shot capture child and almost none of them ever shows the settings
                // row, so the probe (a file read, a registry query, an `SMAppService` call)
                // belongs on the path that can display it. `autostart_settings_opened` takes
                // it at both settings-window mints, before the window is ever painted, and
                // until then the row falls back to the preference exactly as it always did.
                #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
                autostart_registered: None,
                // DRAGON-618: nothing is in flight at launch, and this is never restored from
                // disk (see the field's doc).
                #[cfg(target_os = "linux")]
                autostart_pending: false,
                // DRAGON-625: no attempt has failed yet this run, and this is never
                // restored from disk (see the field's doc).
                #[cfg(target_os = "linux")]
                autostart_notice: None,
                capture_hotkey: persisted.capture_hotkey.clone(),
                capture_active_window_hotkey: persisted.capture_active_window_hotkey.clone(),
                capture_active_monitor_hotkey: persisted.capture_active_monitor_hotkey.clone(),
                // DRAGON-428: the three "(no editor)" twins.
                capture_no_editor_hotkey: persisted.capture_no_editor_hotkey.clone(),
                capture_active_window_no_editor_hotkey: persisted
                    .capture_active_window_no_editor_hotkey
                    .clone(),
                capture_active_monitor_no_editor_hotkey: persisted
                    .capture_active_monitor_no_editor_hotkey
                    .clone(),
                color_picker_hotkey: persisted.color_picker_hotkey.clone(),
                #[cfg(target_os = "macos")]
                aerospace_guard: None,
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                passthrough_active: false,
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                passthrough_solid: None,
                region_overlay_opacity: persisted.region_overlay_opacity,
                active_overlay_opacity: persisted.active_overlay_opacity,
                preview_overlay_opacity: persisted.preview_overlay_opacity,
                color_picker_overlay_opacity: persisted.color_picker_overlay_opacity,
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
                ocr_language: persisted.ocr_language.clone(),
                ocr_langs: Vec::new(),
                ocr_lang_labels: Vec::new(),
                ocr_lang_dir: None,
                tesseract_available: crate::detect::tesseract_available(),
                code_scan,
                text_scan,
                code_busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                ocr_busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                last_code_region: None,
                last_ocr_region: None,
                scan_shot: ScanShot::Idle,
                scan_shot_slot: std::sync::Arc::new(std::sync::Mutex::new(None)),
                scan_spin: 0.0,
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
                recording_out_path: None,
                recording_stopping: false,
                recording_progress: None,
                recording_paused_at: None,
                recording_paused_accum: std::time::Duration::ZERO,
                recording_cancelled: false,
                hide_toolbar_fullscreen: persisted.hide_toolbar_fullscreen,
                tray: None,
                countdown_tray: None,
                tray_hides_toolbar: false,
                push_to_talk: persisted.push_to_talk,
                ptt_held: false,
                hotkeys: None,
                screenshot_dir: persisted.screenshot_dir,
                // DRAGON-559: `launch_arms` above — the `--audio` override, or the
                // persisted pair when the flag is absent.
                record_mic: launch_arms.0,
                record_system_audio: launch_arms.1,
                covermark_text: persisted.covermark_text,
                covermark_zoom: persisted.covermark_zoom,
                covermark_opacity: persisted.covermark_opacity,
                covermark_prefs: persisted
                    .covermark_prefs
                    .into_iter()
                    .map(|p| (p.key, (p.zoom, p.opacity)))
                    .collect(),
                annot_color: persisted.annot_color,
                annot_tool: persisted
                    .annot_tool
                    .as_deref()
                    .and_then(crate::widgets::annotation_canvas::Tool::from_str),
                annot_stroke_w: persisted.annot_stroke_w,
                annot_badge_size: persisted.annot_badge_size,
                annot_text_size: persisted.annot_text_size,
                annot_text_font: persisted
                    .annot_text_font
                    .as_deref()
                    .and_then(crate::app::preview::text_annot::TextFont::from_str)
                    .unwrap_or_default(),
                annot_recent_colors: persisted.annot_recent_colors,
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
                #[cfg(target_os = "macos")]
                settings_fullscreen: false,
                #[cfg(target_os = "macos")]
                preview_fullscreen: false,
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
                menu_hold,
                // DRAGON-606: no dim until the frozen-flats grab has landed. On a launch
                // that grabs nothing this clears on the first drain tick; on one that does,
                // it clears when the grab posts, which is the ordering the fade exists to
                // respect.
                dim_fade: std::cell::Cell::new(crate::app::overlay::DimFade::Waiting),
                // DRAGON-601: no directional key is held at launch, so the first press is a
                // fresh tap and always moves.
                nudge_hold: None,
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
                pipewire_available: false,
                pipewire_source_types: 0,
                toast: None,
                // DRAGON-612: no accept is being held. Only a colour-picker launch can ever
                // leave this state, and only for the moment before its first pixel lands.
                accept_pending: None,
                pw_restore_token: super::portal::RestoreTokens {
                    monitor: persisted.pw_restore_token_monitor,
                    window: persisted.pw_restore_token_window,
                },
                pw_pending: None,
                pw_slot: std::sync::Arc::new(std::sync::Mutex::new(None)),
                pw_held: None,
                #[cfg(target_os = "linux")]
                fallback_seed_kicked: false,
                // DRAGON-604: no seed frame exists yet, so there is nothing to compare a
                // kind change against and nothing to replace.
                #[cfg(target_os = "linux")]
                fallback_seed_cursor: None,
                #[cfg(target_os = "linux")]
                fallback_reseeding: false,
                #[cfg(target_os = "linux")]
                fallback_window: None,
                #[cfg(target_os = "linux")]
                fallback_grant: None,
                #[cfg(target_os = "linux")]
                fallback_frame_slot: std::sync::Arc::new(std::sync::Mutex::new(None)),
                update_status: crate::update::UpdateStatus::Unknown,
                update_installing: false,
                notify_updates: persisted.notify_updates,
                cloud_last_account: persisted.cloud_last_account.clone(),
                cloud_auto_share: persisted.cloud_auto_share,
                update_dialog: None,
                update_dialog_decided: false,
                update_notes: None,
                release_notes_fetched: false,
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
                // The two embedded annotation faces (Excalifont / Inter) are registered with the
                // GLOBAL cosmic-text font system SYNCHRONOUSLY in `super::run`, before the
                // compositor exists — NOT here. An async `iced::font::load` Task dispatched from
                // `init` races the compositor's lazy creation and is silently dropped (see the
                // long note in `run`), which is why the font-preview dropdown labels (DRAGON-354
                // item 19) never resolved. Nothing to do at this seam.
                // DRAGON-601 launched a pointer probe here to seed the colour picker's loupe,
                // because on a Wayland layer surface the toolkit reported no cursor until the
                // pointer moved. DRAGON-609 removed it, and the reason is worth keeping: the
                // seed never once fired on a clean run. The probe minted a layer surface per
                // output and waited for a `wl_pointer.enter`, which is the very thing
                // cosmic-comp fails to terminate with the mandatory `wl_pointer.frame`, so the
                // probe was defeated by the same bug it existed to work around. It cost ~350ms
                // of full-screen surfaces mapping over the picker overlay and answered nothing.
                // The picker now learns its position from the enter itself, earlier and more
                // accurately, via the dispatch fix in our iced fork.
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
        mark_first_view();
        // The settings window is its own toplevel, rendered independently of the
        // per-output capture overlays.
        if Some(id) == self.settings.window {
            return self.config_window_view();
        }
        if Some(id) == self.permissions.window {
            return self.permissions_window_view();
        }
        // DRAGON-582: the colour picker's result window is its own toplevel, like
        // settings and the permission checker.
        if Some(id) == self.color_picker.window {
            return self.color_picker_window_view();
        }
        // DRAGON-216 (Linux windowed): the pre-opened neutral overlay whose preview was just
        // swapped to a real window keeps painting its loading cover until its close lands, so
        // the window maps under it with no desktop flash.
        if self.grab_overlay_closing == Some(id) {
            return self.grab_cover_view();
        }
        if self.is_preview_window(id) {
            return self.preview_view(id);
        }
        if let Some(o) = self.outputs.iter().find(|o| o.id == id) {
            // DRAGON-600: this overlay exists right now for ONE reason, to take keyboard
            // focus so the tray dropdown that launched us goes away, and the flats grab
            // that follows would photograph anything we drew. Paint NOTHING until the
            // hold releases: the surface is transparent, so it composites to nothing.
            // Same trick DRAGON-456 used for the scan re-read, and the same reason it is
            // ONE gate here rather than per layer, so no layer can be forgotten. Nothing
            // of the user's is hidden by it: at launch there is no selection yet, which
            // is exactly why the blank DRAGON-460 removed could not live here.
            if self.menu_hold.is_some() {
                return widget::space::Space::new()
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into();
            }
            // DRAGON-456: a scan refresh is re-reading the screen, and our own overlay is
            // part of what a screen grab sees. Paint NOTHING (the surface is transparent,
            // so it composites to nothing) until the new flats land — chrome, marks, dim
            // wash and, most importantly, the frozen backdrop, which would otherwise make
            // the re-grab a photograph of the still it is meant to replace. ONE place, so
            // no layer can be forgotten. The overlay is untouched otherwise: same surface,
            // same input, same state — it just skips a couple of frames.
            // DRAGON-460 removed the blank that stood here.
            //
            // DRAGON-456 needed it because a scan re-read the WHOLE screen, so everything we
            // draw had to come off first. Reading only the selection removes the reason: the
            // region interior is the one part of the overlay we never paint, so a shot of it
            // is clean while the chrome stays up. The only thing hidden now is the marks
            // layer, cleared in `begin_scan_shot`, because that IS inside the crop.
            //
            // Do not reintroduce a blank here. It was also the mechanism by which a refresh
            // "hid the region selection", which is the behaviour this ticket removes.
            if self.color_picking() {
                // DRAGON-582: a colour-picker launch mints the SAME per-output overlays
                // a capture does and only draws something else on them.
                self.color_picker_view(o)
            } else if self.recording.is_some() {
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
        mark_first_update();
        let task = match message {
            Msg::Capture(message) => self.update_capture(message),
            Msg::Recording(message) => self.update_recording(message),
            Msg::Detect(message) => self.update_detect(message),
            Msg::Settings(message) => self.update_settings(message),
            Msg::Permissions(message) => self.update_permissions(message),
            Msg::WindowChrome(message) => self.update_window_chrome(message),
            Msg::ColorPicker(message) => self.update_color_picker(message),
            Msg::Preview(id, message) => self.update_preview(id, message),
        };
        // DRAGON-413: publish what this child currently has on screen to the startup
        // self-exit guard. Here — after the dispatch, on EVERY message — rather than at
        // each surface's open/close site: the status is recomputed from live `App` state
        // (`startup_presence`), so no new field is introduced and no mutation site can be
        // forgotten. One relaxed atomic store. macOS-only (the guard is only armed
        // there), so Linux/Windows compile exactly as before.
        #[cfg(target_os = "macos")]
        crate::startup_guard::report(self.startup_presence());
        task
    }

    fn subscription(&self) -> Subscription<Msg> {
        // Body lives in subscriptions.rs as small named `sub_*` helpers, one per
        // trigger condition (see that file for what drives each tick and why).
        self.subscriptions()
    }
}

/// Whether a DRAGON-415 capture-failure alert is on screen (macOS; always false
/// elsewhere, where nothing is ever presented). Split out so `startup_presence` below
/// stays one expression on both platforms.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn alert_is_showing() -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::platform::mac::alert::is_showing()
    }
    #[cfg(not(target_os = "macos"))]
    false
}

// `test` as well as macOS: compiling this on the Linux dev box keeps the field list
// below TYPE-CHECKED by the local suite, which is the only gate this repo has — the
// macOS build is not run here (see CLAUDE.md). Dead there, hence the allow.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl App {
    /// This child's [`crate::startup_guard::Presence`], read from the surfaces it
    /// currently owns (DRAGON-413). The ONE place that maps app state onto the guard's
    /// three cases; `update` calls it after every dispatch.
    ///
    /// The "visible work" list is deliberately broad — every state in which the user can
    /// see (or is about to see) this session, including the picker overlay, a running
    /// countdown, a pixel capture in flight, a live recording (an immediate
    /// `--active-monitor --video` launch mints NO overlay and can legitimately run for
    /// hours), an open preview editor, and an in-app settings window. Any one of them
    /// disarms the guard for the rest of the process, so an overlay later torn down for a
    /// preview handoff can never re-arm it.
    ///
    /// The permission checker is the one surface that SUSPENDS instead of disarming: the
    /// user may sit on it for as long as they like, and reading it must never accumulate
    /// toward a kill. It is read straight off `permissions.window` — the existing state
    /// that already drives `sub_permission_poll` and `seed_outputs_mac` — so nothing new
    /// is coupled to the permission flow (DRAGON-412 owns that module).
    ///
    /// DRAGON-439: the overlay counts once it is PLACED, not once it is MINTED. `outputs`
    /// gains its entries in `seed_overlays`, at winit-window creation, and on macOS the
    /// view deliberately draws a transparent `Space` until `configure_overlay` lands
    /// (`OutputState::user_visible`). Latching Presented at the mint disarmed the guard
    /// over a stretch in which the user still sees nothing at all — an iced/wgpu runtime
    /// that wedged there produced exactly the invisible immortal child DRAGON-413 exists
    /// to bound. That stretch now BURNS budget like the rest of a slow start. It cannot
    /// make the guard trigger-happy on a working launch: placement normally lands within a
    /// frame or two, and `configure_overlay` forces `placed = true` when its 30 × 40ms
    /// (~1.2s) title-match retries run out, so even an overlay whose NSWindow is never
    /// matched still latches. Only a runtime that stops answering keeps burning.
    fn startup_presence(&self) -> crate::startup_guard::Presence {
        crate::startup_guard::classify(&crate::startup_guard::Surfaces {
            overlay_placed: self.outputs.iter().any(|o| o.user_visible()),
            preview_open: !self.previews.is_empty(),
            countdown: self.countdown.is_some(),
            capturing: self.capturing.is_some(),
            recording: self.recording.is_some(),
            settings_open: self.settings.window.is_some(),
            permission_window: self.permissions.window.is_some(),
            // DRAGON-415: a capture-failure alert is the OTHER window the user must act on,
            // and reading it must no more cost this child its life than reading the
            // permission checker does. `report_failure` publishes `AwaitingUser` before the
            // modal takes the run loop; this keeps the two agreeing on any dispatch that
            // happens to run while one is up.
            alert: alert_is_showing(),
        })
    }
}
