use super::*;

/// Push-to-talk is HIDDEN for now (DRAGON-109): its hold hotkey can't be
/// delivered while our surfaces are unfocused on COSMIC (no GlobalShortcuts
/// portal yet), which left the mic seeded muted with no working un-mute. The
/// settings row is gone and the behavior inert — the persisted setting is
/// KEPT, so flipping this back on restores everything once hold delivery
/// exists (the portal bind in `platform::global_shortcuts`, or a successor).
pub(super) const PTT_AVAILABLE: bool = false;

/// DRAGON-659: how long a promoted-but-not-yet-warm recording sits before the warming
/// spinner is REVEALED, when nothing else was covering the warmup. A warmup that finishes
/// inside this window never shows a spinner at all, which is the point: the DRAGON-657
/// bring-up is usually a few hundred milliseconds, and a glyph that appears and vanishes
/// inside one blink reads as a glitch rather than as progress.
///
/// DRAGON-673: every promotion uses it, countdown or not. A countdown promotion used to use
/// `ZERO`, because the countdown had already covered the warmup; media 0 is now gated on the
/// promotion itself, so the worker settles just AFTER it and a zero delay would flash the
/// spinner every time. See [`App::warming_spinner`].
pub(super) const WARM_SPINNER_REVEAL_DELAY: std::time::Duration =
    std::time::Duration::from_millis(200);

/// DRAGON-659: once revealed, the minimum time the warming spinner stays on screen, so a
/// warmup that lands just past the reveal cannot flash the glyph on and off.
///
/// Both numbers are borrowed from the colour picker's `PICKER_SPINNER_REVEAL_MS` /
/// `PICKER_SPINNER_MIN_MS` (`app::overlay`) purely so busy states across the app FEEL the
/// same; nothing about the recording warmup derives them, and they are free to be tuned.
pub(super) const WARM_SPINNER_MIN_HOLD: std::time::Duration =
    std::time::Duration::from_millis(300);

/// DRAGON-659: how long [`App::abandon_warming`]'s detached reaper waits for the abandoned
/// worker to finish unwinding before it deletes whatever that worker produced. Matches the
/// stop tail's own reap bound (`record::wait_or_kill`'s 30s), which is the longest a worker
/// asked to stop can legitimately take. DRAGON-118: the wait is bounded, not open-ended.
const ABANDON_REAP_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

// DRAGON-673 deleted `WARM_SPAWN_AT_REMAINING_SECS` and `warm_spawn_at_countdown_start`.
// They existed to trade warmup cover against recording the countdown: the worker captures
// continuously once warm, and media 0 used to move with the spawn, so every extra second of
// runway was another second of countdown in the file. That trade is gone. Media 0 is the
// settled pipeline (DRAGON-672) gated on the app's own promotion (DRAGON-673,
// `RecordSettings::start_gate`), so the worker is spawned at countdown START and the file
// still begins where the UI starts claiming to record. Nothing has to estimate how long
// warmup takes any more, which is the point: the constant had just been re-tuned 1 -> 2 for
// Windows and would have needed re-tuning for the next slow audio stack.
//
// Nor does anything estimate WHEN the countdown ends. The gate carried the predicted instant
// of zero first, and the prediction lost to the tick schedule by ~1s; see
// `record::owned::media_zero`.
//
// The measurement that justified the old rule is worth keeping: on a 5 second countdown
// the pre-flight (media 0 back then) began at +1.92s, so 4.6 of the 5 seconds landed in
// the file. That is what "defeats what a countdown is FOR on video" meant, and it is now
// impossible by construction rather than by choosing a spawn tick.

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
    ///
    /// DRAGON-595: asks the SELECTED backend which one it is, instead of re-deriving
    /// the choice from the saved ids. The two agree by construction, because
    /// `active_*_backend` resolves a portal object on exactly the
    /// `uses_portal() && pipewire_available` pairing this used to test inline. Off
    /// Linux the selected backend is SCK or WGC, so the label stays "cosmic" as it
    /// always did (`pipewire_available` is false there).
    pub(super) fn source_label(&self) -> &'static str {
        let backend = match self.kind {
            Kind::Video => self.active_record_backend(),
            Kind::Image | Kind::Scanner => self.active_screenshot_backend(),
        };
        if backend.id() == crate::platform::backend::PORTAL_ID {
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

    /// The DISPLAYED preferred-encoder id (what the settings picker shows), resolved
    /// lazily on first read (DRAGON-201). A display resolution only: it is never
    /// persisted (DRAGON-571); saves write `encoder_requested` instead.
    pub(super) fn preferred_encoder(&self) -> String {
        self.encoders.preferred()
    }

    /// The persisted encoder INTENT ("auto" or the user's pick): what `save_state`
    /// writes back to `preferred_encoder`, never a resolution (DRAGON-571).
    pub(super) fn encoder_requested(&self) -> String {
        self.encoders.requested()
    }

    /// The `(requested, hint)` pair a recording start hands to `RecordSettings`: see
    /// `EncoderResolve::request` (DRAGON-571). "auto" travels as "auto" so the
    /// worker's hint-first ladder re-resolves it every session.
    pub(super) fn encoder_request(&self) -> (String, Option<String>) {
        self.encoders.request()
    }

    /// Overwrite the live preferred-encoder INTENT (user pick / persist apply).
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
        // DRAGON-499: detached, not `spawn_blocking`. This is the OTHER background job a
        // settings window starts, and the probe runs `ffmpeg -encoders` plus real hardware
        // probe-encodes, so it is seconds long; on the blocking pool those seconds are added
        // to closing the window (see `app::background`). A probe nobody is waiting for any
        // more is simply abandoned.
        Task::perform(
            off_thread(crate::app::EncoderResolve::probe_list),
            |list| {
                cosmic::Action::App(Msg::Settings(SettingsMsg::EncodersProbed(
                    list.unwrap_or_default(),
                )))
            },
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
    /// show and, since DRAGON-674, whether the codec row may offer HEVC.
    ///
    /// `preferred_encoder` is already probe-backed: it resolves the persisted intent
    /// through the PROBED list (`display_encoder_choice`), and the last-known-good auto
    /// hint can only reorder that list, never add an encoder to it. What this match
    /// adds is the pass-through of ids that are real tiers, so a value which is not an
    /// encoder at all can never be reported as one.
    ///
    /// DRAGON-674 completed that list. `amf`, `qsv` and `videotoolbox` were missing, so
    /// on a Windows or mac box where the user had picked a LOWER-ranked tier this
    /// answered with the TOP of the probed list instead of their pick: the wrong preset
    /// row, and, now that the codec row reads the tier's HEVC probe, the wrong codec
    /// offer too. Linux is byte-identical — its ladder can only ever produce
    /// nvenc / vaapi / software, so the three added arms are unreachable there.
    pub(super) fn effective_encoder(&self) -> String {
        match self.preferred_encoder().as_str() {
            id @ ("nvenc" | "vaapi" | "amf" | "qsv" | "videotoolbox" | "software") => {
                id.to_string()
            }
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

    /// Promote a recording worker into the app's LIVE recording. This is the one instant
    /// `self.recording` becomes `Some`, for both the countdown and the no-countdown path,
    /// exactly as it always was.
    ///
    /// DRAGON-659: the worker itself may already have been running for the whole countdown
    /// ([`App::spawn_record_worker`], parked in `self.warming`); when it has not, it is
    /// spawned here, synchronously, at the same call boundary as before. Everything in the
    /// prelude below stays HERE on purpose rather than moving to the early spawn: none of it
    /// is a device-contention or config hazard, and binding the global stop hotkey at
    /// countdown start would make "stop recording" live seconds before there is a recording
    /// to stop.
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
        // Close any other instances so only this overlay records. DRAGON-322: a preview
        // / recording sibling is spared, so this recording can coexist with a concurrent
        // capture (record a tutorial of the tool in use).
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
        // DRAGON-659: reuse the worker the countdown started, or start one now. Either way
        // the spinner's reveal delay gives a warmup that lands inside it room to finish
        // invisibly (see `warming_spinner`).
        let warm = match self.warming.take() {
            Some(warm) => warm,
            None => self.spawn_record_worker(&sel),
        };
        let WarmSpawn { handle, out_path } = warm;
        self.recording = Some(handle);
        self.recording_promoted_at = Some(std::time::Instant::now());
        self.recording_paused_at = None;
        self.recording_paused_accum = std::time::Duration::ZERO;
        // The live size readout tracks the temp capture as it grows; the final file
        // (after the finalize pass) is what `RecordingPoll` reports on `done`.
        self.arm_session_bound(&out_path);
        // lab/flatpak: the fallback path has no layer shell, so `recreate_active_overlays`
        // (below) tears the selection window and `self.outputs` down the moment recording
        // starts. Resolve the finished recording's editor anchor NOW, while the outputs
        // are still known; `stop_recording` prefers a fresh stop-time answer and only
        // falls back to this one. Protocol-keyed (`overlay_fallback_active`), and native
        // sessions keep their surfaces up and never take this branch, so they stay
        // byte-identical.
        if self.overlay_fallback_active() {
            self.snapshot_preview_anchor(&sel);
        }
        // Keep the ORIGINAL selection for the overlay/border + toolbar anchor.
        self.pending = Some(sel);
        // DRAGON-673: SAY GO. This is the instant the app begins claiming to record, so it is
        // the instant a countdown's prewarmed worker is released to stamp media 0 — the file
        // and the claim then start together by construction, with nothing predicting either.
        // Raised here rather than at the countdown's zero-tick because the tick only decides
        // to promote; everything above (the hotkey binds, the sibling sweep, the worker
        // handoff) still has to happen, and the file must not begin before it has.
        self.open_countdown_gate();
        // DRAGON-659: adopt a worker that is ALREADY settled here rather than up to one 100ms
        // poll later, so the tray and the elapsed anchor never lag a recording that is
        // already live. It also runs `begin_recording_tray` BEFORE the overlay rebuild below,
        // so `tray_hides_toolbar` is settled when the input regions are cut.
        //
        // DRAGON-673 made that the RARE case rather than the countdown's normal one: the gate
        // was opened one statement ago and the worker settles a few milliseconds after it, so
        // a countdown session usually adopts on the next `RecordingPoll`, which handles the
        // tray rebuild it owes. Nothing here waits for the worker either way.
        self.adopt_warm_start();
        // Recreate the overlay click-through except the toolbar's region so the
        // recorded apps stay usable. (A no-op rebuild from the countdown, which is
        // already active, but cheap.)
        self.recreate_active_overlays()
    }

    /// DRAGON-673: OPEN the countdown's start gate, releasing a prewarmed worker to stamp
    /// media 0. Idempotent, and a no-op for a session that never ran a countdown (nothing
    /// was minted, and a worker with no gate never holds).
    ///
    /// Called from exactly two places, and both are "this countdown is over": the promotion
    /// in [`App::start_recording`], where the file must begin, and [`App::abandon_warming`],
    /// where there will be no file — a worker being thrown away must not sit out the gate's
    /// whole 30s cap first, with ffmpeg spawned and idle, before it can notice it was
    /// stopped.
    ///
    /// `Relaxed` like every other flag the record path shares (`stop`, `paused`): the only
    /// thing published is the flag itself, and the worker re-reads it on its own poll.
    pub(super) fn open_countdown_gate(&self) {
        if let Some(gate) = &self.countdown_gate {
            gate.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// DRAGON-659: start a countdown's recording worker EARLY, parked in `self.warming`
    /// until the countdown's zero-tick promotes it. THE one entry point for the early
    /// spawn, and since DRAGON-673 there is only one caller: [`App::enter_countdown`],
    /// for EVERY countdown, at countdown START.
    ///
    /// It used to be called from a countdown TICK near the end instead, because the worker
    /// captures continuously and media 0 moved with the spawn, so every extra second of
    /// warmup was another second of countdown in the file. Media 0 is now gated on the
    /// promotion (`RecordSettings::start_gate`), so warming for the whole countdown puts
    /// nothing in the file and needs no estimate of how long warmup takes.
    ///
    /// The gate mirrors `begin`'s recording gate exactly: any other kind, or a session with
    /// no ffmpeg (which `begin` answers with the install notice, not a recording), must
    /// behave precisely as it did before this ticket. A second call is a no-op, so the two
    /// call sites can never double-spawn.
    ///
    /// `self.recording` deliberately stays `None` for the whole countdown; only
    /// `self.warming` is filled. See its doc for the two if/else-if chains that would
    /// otherwise take the wrong branch.
    pub(super) fn arm_warm_spawn(&mut self, sel: &Selection) {
        if self.warming.is_some() || self.kind != Kind::Video || !self.ffmpeg_available {
            return;
        }
        self.warming = Some(self.spawn_record_worker(sel));
    }

    /// Start the recording WORKER for `sel` and hand back its handle plus the output path
    /// computed for it. THE one place a recording worker is spawned: either early, from
    /// [`App::arm_warm_spawn`], so the WHOLE countdown covers the DRAGON-657 warmup
    /// (DRAGON-673; it used to be only the last second or two), or immediately, from
    /// [`App::start_recording`], when there is no countdown to hide behind.
    ///
    /// Everything below is order-sensitive against the worker's own thread, which is why
    /// none of it may be left behind at promotion:
    ///
    /// * The device releases (a live mic test's capture, the armed-idle system meter) have
    ///   to happen before the worker opens the same devices.
    /// * The mic-cleanup and ducking GLOBALS are read by the worker's audio pre-flight, and
    ///   since DRAGON-657 that pre-flight is gated on the first real captured frame, an
    ///   instant with no relationship to the countdown timer. A worker whose stream produces
    ///   a frame quickly reads them while the countdown is still ticking, so setting them at
    ///   promotion would silently record with whatever the globals last held.
    /// * `record_output_path` embeds a wall-clock timestamp, so it is computed ONCE, here,
    ///   and carried forward; recomputing it at promotion would name a second file the
    ///   worker never wrote a byte to.
    pub(super) fn spawn_record_worker(&mut self, sel: &Selection) -> WarmSpawn {
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
        let out_path = self.record_output_path(sel);
        // DRAGON-421: reap whatever a crashed session left behind BEFORE this one creates
        // anything of its own — an orphaned muxer still holding a temp, its stale FIFOs,
        // an abandoned take. Only provably-dead wreckage is touched; a concurrent
        // recording's files are spared (DRAGON-322/351).
        crate::record::recover::sweep_wreckage(out_path.parent());
        // A portal stream granted at commit (held across any countdown) → record it;
        // otherwise direct screencopy.
        let handle = match self.pw_held.take() {
            Some(held) => self.spawn_held_pipewire_worker(held, &out_path),
            None => self.spawn_screencopy_worker(sel, &out_path),
        };
        // DRAGON-322: advertise the live recording cross-process so a fresh capture
        // overlay disables its video kind and the sibling sweep spares us. Set at SPAWN
        // rather than at promotion since DRAGON-659: the capture is genuinely running from
        // here, so a sibling that starts during the countdown must already see it.
        crate::instance::set_recording_marker(true);
        WarmSpawn { handle, out_path }
    }

    /// Spawn the worker for the already-granted portal stream `held`.
    fn spawn_held_pipewire_worker(
        &mut self,
        held: HeldStream,
        out_path: &std::path::Path,
    ) -> crate::record::RecordHandle {
        let (max_w, max_h) = self.res_limit();
        // DRAGON-571: the request travels verbatim ("auto" stays "auto", with its
        // last-known-good hint), so the worker's ladder resolves auto fresh each
        // session instead of this process pinning a one-time answer.
        let (requested_encoder, encoder_hint) = self.encoder_request();
        crate::record::start_pipewire_recording(crate::record::PipewireRecordParams {
            fd: held.fd,
            node_id: held.node_id,
            crop: held.crop,
            settings: crate::record::RecordSettings {
                fps: self.record_fps.value,
                preferred_encoder: requested_encoder,
                encoder_hint,
                presets: self.presets(),
                zero_copy: self.record_zero_copy,
                mic: self.mic_armed(),
                system_audio: self.record_system_audio,
                bitrate_kbps: self.record_bitrate_kbps.value,
                audio_offset_ms: self.audio_sync_offset_ms.value,
                // DRAGON-673: the countdown gate, so a prewarmed session still
                // begins its file exactly where the app promotes the recording.
                start_gate: self.countdown_gate.clone(),
                // Auto mode probes + folds in the live device latency (system channel
                // only); manual mode keeps the offset above exactly as set (DRAGON-119).
                auto_device_compensation: self.audio_sync_auto,
                max_res: (max_w, max_h),
                metadata: self.recording_metadata(),
                out_path: out_path.to_path_buf(),
            },
        })
    }

    /// Spawn the worker for a direct cosmic-screencopy recording (the default path, and
    /// the fallback when the portal/PipeWire is unavailable).
    fn spawn_screencopy_worker(
        &mut self,
        sel: &Selection,
        out_path: &std::path::Path,
    ) -> crate::record::RecordHandle {
        // Record only what's inside the visible line: a region is inset by the
        // line width (the outline, drawn on the original rect, then sits just
        // outside the recorded crop); window/monitor record the full target.
        let rec = inset_region(sel.clone());
        let (max_w, max_h) = self.res_limit();
        // DRAGON-571: the request travels verbatim ("auto" stays "auto", with its
        // last-known-good hint); the worker's ladder resolves auto fresh each session.
        let (requested_encoder, encoder_hint) = self.encoder_request();
        crate::record::start_region_recording(crate::record::RegionRecordParams {
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
                preferred_encoder: requested_encoder.clone(),
                encoder_hint,
                presets: self.presets(),
                // GPU zero-copy applies to a full output (Monitor mode, no crop) with a
                // hardware encoder; Region/Window crop, so they take the CPU path. An
                // "auto" request passes this gate (it may resolve to hardware); on a
                // GPU-less box the zero-copy attempt declines on its own and the CPU
                // fallback takes over, exactly like any other zero-copy decline.
                zero_copy: self.mode == Mode::Monitor
                    && self.record_zero_copy
                    && requested_encoder != "software",
                mic: self.mic_armed(),
                system_audio: self.record_system_audio,
                bitrate_kbps: self.record_bitrate_kbps.value,
                audio_offset_ms: self.audio_sync_offset_ms.value,
                // DRAGON-673: the countdown gate, so a prewarmed session still
                // begins its file exactly where the app promotes the recording.
                start_gate: self.countdown_gate.clone(),
                // Auto mode probes + folds in the live device latency (system channel
                // only); manual mode keeps the offset above exactly as set (DRAGON-119).
                auto_device_compensation: self.audio_sync_auto,
                max_res: (max_w, max_h),
                metadata: self.recording_metadata(),
                out_path: out_path.to_path_buf(),
            },
        })
    }

    /// DRAGON-659/661: adopt the worker's SETTLED signal, once per recording. Returns whether
    /// this call is the one that made the recording LIVE.
    ///
    /// `settled_at` — the end of the worker's opening phase — answers BOTH questions since
    /// DRAGON-672/673, because it is MEDIA 0:
    ///
    /// * it is the LIVE DECLARATION: the tray's "Recording" state, the same claim the record
    ///   chip's STOP face makes. DRAGON-661 moved that here from `warm_at`, which fires at the
    ///   START of a bring-up with the audio pre-flight, the ffmpeg spawn and the opening
    ///   catch-up still to come, so an icon saying "Recording" through 600ms of setup was
    ///   telling the user something not yet true.
    /// * it is also the ELAPSED ANCHOR, where the file begins. That half used to read
    ///   `warm_at`, on the premise that the confirmed first frame is where the file's content
    ///   starts. It no longer is: media 0 is the settled pipeline (DRAGON-672), and with the
    ///   worker spawned at countdown START (DRAGON-673) the first frame precedes the file by
    ///   the WHOLE countdown. The owner saw it immediately: a 10s countdown's recording armed
    ///   its readout at about 0:10 and he cancelled the take.
    ///
    /// The anchor is still the WORKER's own timestamp, never a fresh `Instant::now()`, so the
    /// readout cannot drift by a poll's cadence.
    ///
    /// Each half keeps its own latch and neither waits for the other's. `recording_started`
    /// latches the anchor. `recording_live_declared` latches the tray, and it has to be its
    /// own field rather than `self.tray`: a Linux session with no SNI host leaves that
    /// `None` even though the tray was raised as far as it can be, and keying on it would
    /// re-run this every 100ms for the whole recording.
    pub(super) fn adopt_warm_start(&mut self) -> bool {
        let (anchor, declare_live) = warm_adoption(
            self.recording_started.is_some(),
            self.recording_live_declared,
            self.settled_instant(),
        );
        // Both marks land on the SAME timeline as the workers' own (`rec/sck: first frame`,
        // `rec/sck: opening covered ...`), so one debug log shows the whole start: when the
        // file's content begins, and when the UI began claiming to be live. That pair is
        // what DRAGON-661 was diagnosed from, and it is what a re-check needs.
        if let Some(at) = anchor {
            self.recording_started = Some(at);
            crate::util::timing_mark("ui: elapsed anchor adopted (media 0)");
            // The ONE line that says how much bring-up this recording paid for. `warm_at` is
            // not what anything counts from any more, but it is still the worker's own
            // "capture became real" instant, and the span from there to media 0 IS the
            // opening: the audio pre-flight, the ffmpeg spawn, the opening ticks and, on a
            // countdown, the hold at the start gate. It is the first number to check a "my
            // recording is missing the start" report against, and pairing it with
            // `media_zero`'s own dropped-audio line gives the whole picture from one log.
            if let Some(warm) = self.warm_instant() {
                log::debug!(
                    "recording bring-up: capture was confirmed {:.0}ms before media 0, which \
                     the countdown or the warming spinner covered; the file begins at media 0",
                    at.saturating_duration_since(warm).as_secs_f64() * 1000.0,
                );
            }
        }
        if declare_live {
            self.recording_live_declared = true;
            crate::util::timing_mark("ui: recording declared live (settled)");
            self.begin_recording_tray();
        }
        declare_live
    }

    /// The live recording worker's confirmed first-frame instant (DRAGON-657's
    /// `RecordHandle::warm_at`), or `None` while it is still warming up. Nothing the UI shows
    /// is derived from it any more (see [`Self::adopt_warm_start`]); it is read once, for the
    /// bring-up line in the debug log.
    fn warm_instant(&self) -> Option<std::time::Instant> {
        self.recording
            .as_ref()
            .and_then(|r| r.warm_at.lock().ok().and_then(|g| *g))
    }

    /// The live recording worker's SETTLED instant (DRAGON-661's
    /// `RecordHandle::settled_at`): the end of its opening phase, and so the first moment
    /// the pipeline is genuinely steady. `None` until then, which is strictly later than
    /// [`Self::warm_instant`] on every worker.
    fn settled_instant(&self) -> Option<std::time::Instant> {
        self.recording
            .as_ref()
            .and_then(|r| r.settled_at.lock().ok().and_then(|g| *g))
    }

    /// DRAGON-659: whether the record chip should be wearing its WARMING face right now.
    /// The effectful half of [`warm_spinner_visible`], which holds the whole rule.
    ///
    /// DRAGON-661: the thing being waited for is the SETTLED instant, not the first frame.
    ///
    /// DRAGON-673: ONE reveal delay for every promotion, countdown or not. A countdown
    /// promotion used to pass `ZERO`, on the reasoning that the countdown had already been
    /// the warmup's cover so anything still pending at zero was late. Gating media 0 on the
    /// promotion inverted that: the worker now holds AT the gate and settles a few
    /// milliseconds AFTER the promotion, every time, so a zero delay would flash the spinner
    /// on every single countdown recording — the exact "appears and vanishes inside one
    /// blink" this delay exists to prevent. A worker genuinely late at zero has not reached
    /// the gate at all and is still tens or hundreds of milliseconds out, so the delay shows
    /// it just the same.
    pub(super) fn warming_spinner(&self) -> bool {
        let Some(promoted_at) = self.recording_promoted_at else {
            return false;
        };
        if self.recording.is_none() {
            return false;
        }
        warm_spinner_visible(
            promoted_at,
            self.settled_instant(),
            WARM_SPINNER_REVEAL_DELAY,
            WARM_SPINNER_MIN_HOLD,
            std::time::Instant::now(),
        )
    }

    /// DRAGON-659: which face the record chip is wearing right now. The effectful half of
    /// [`chip_face`], which holds the whole rule; the view matches on the answer rather than
    /// re-deriving it from three separate questions.
    ///
    /// DRAGON-661: the STOP face keys on the SETTLED instant, not the first frame.
    pub(super) fn record_chip_face(&self) -> ChipFace {
        chip_face(
            self.recording.is_some(),
            self.settled_instant().is_some(),
            self.warming_spinner(),
        )
    }

    /// DRAGON-659: throw away a worker the countdown started but nothing will promote:
    /// the user cancelled the timer, or the session is ending. Idempotent, and a no-op
    /// when no countdown ever spawned one (every kind but Video, and every session with no
    /// ffmpeg).
    ///
    /// The files are reaped on a short detached thread rather than here, because a worker
    /// asked to stop still runs its stop tail: it drains, finalizes and WRITES the output
    /// file. Deleting before that lands would leave the finished recording behind in the
    /// user's folder, which is the opposite of cancelling. The wait is bounded (DRAGON-118:
    /// nothing in the record path waits unboundedly) and the thread is detached, so a
    /// session that exits first simply dies with it, leaving only the temp that
    /// `recover::sweep_wreckage` already reaps at the next recording's start.
    pub(super) fn abandon_warming(&mut self) {
        let Some(warm) = self.warming.take() else {
            return;
        };
        log::debug!("the countdown's warming recording worker was abandoned before promotion");
        warm.handle
            .stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // DRAGON-673: release the start gate too. A worker parked waiting for a promotion
        // that is never coming would otherwise hold until the gate's own 30s cap before it
        // could see the stop above, and only then begin the unwind this function's reaper is
        // already waiting on.
        self.open_countdown_gate();
        // DRAGON-322: nothing is recording here after all, so drop the cross-process marker
        // the spawn raised, before any sibling reads it.
        crate::instance::set_recording_marker(false);
        let done = warm.handle.done.clone();
        let out_path = warm.out_path;
        std::thread::spawn(move || {
            let temp = crate::record::recording_temp_path(&out_path);
            let deadline = std::time::Instant::now() + ABANDON_REAP_BUDGET;
            while std::time::Instant::now() < deadline {
                if done.lock().ok().and_then(|g| g.clone()).is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = std::fs::remove_file(&temp);
            let _ = std::fs::remove_file(&out_path);
        });
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
        // DRAGON-583: open this recording to control commands from other processes, so a
        // desktop global hotkey bound to `--toggle-mic` / `--pause-recording` /
        // `--finish-recording` / `--cancel-recording` / `--toggle-system-audio` can reach
        // it. Bound BEFORE the tray, and deliberately independent of it: a sandboxed child
        // often fails to register a tray item at all, and that is exactly the session where
        // these commands are the only control the user has left. A bind failure costs only
        // the commands, never the recording, so it logs and carries on.
        #[cfg(target_os = "linux")]
        {
            self.record_control = match crate::daemon_ipc::start_control_inlet() {
                Ok(inlet) => Some(inlet),
                Err(e) => {
                    log::warn!("recording control commands unavailable ({e})");
                    None
                }
            };
        }
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
                // DRAGON-574: the live countdown preset, which the (disabled while
                // recording) Countdown Timer submenu titles itself from.
                self.delay_idx,
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
        // DRAGON-583: the control inlet has the same lifetime as the icon it sits beside:
        // there is no recording left to command. Dropping it unlinks the socket, so a
        // command sent a moment later reports "no recording is in progress" instead of
        // vanishing into a socket nobody reads.
        #[cfg(target_os = "linux")]
        {
            self.record_control = None;
        }
    }

    /// Tear the session status icon down entirely (DRAGON-174) — the end of the whole
    /// capture session (`finish_session`). Drops any own icon (removing it from the
    /// menu bar / tray) or relay.
    pub(super) fn drop_session_icon(&mut self) {
        self.tray = None;
        self.tray_hides_toolbar = false;
        // DRAGON-583: and the control inlet, on the same terms. `finish_session` exits
        // through `iced::exit`, which runs no destructor, so the socket file this misses is
        // the one `instance::sweep_stale_markers` clears on the next launch, exactly like
        // the preview host's.
        #[cfg(target_os = "linux")]
        {
            self.record_control = None;
        }
        // DRAGON-563: the countdown digits item comes down with the session too, so no
        // ending can leave it on the panel past the process.
        self.countdown_tray = None;
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
                // Same dedup as the keyboard path, carried in the guards: only the first
                // press of a held span un-mutes, and only a press that actually happened
                // re-mutes. A repeat of either falls to the catch-all below and does
                // nothing, exactly as the inner `if` it replaces did.
                Ev::PttPressed if self.ptt_active() && !self.ptt_held => {
                    self.ptt_held = true;
                    if self.recording.is_some() {
                        self.log_audio_toggle_at(at, crate::record::AudioChannel::Mic, true);
                    }
                }
                Ev::PttReleased if self.ptt_active() && self.ptt_held => {
                    self.ptt_held = false;
                    self.log_audio_toggle_at(at, crate::record::AudioChannel::Mic, false);
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
        // The toolbar overlaps the capture on the output it sits on → no outside room.
        self.outputs.iter().any(|o| {
            let Some((tb, _)) = self.toolbar_layout(o) else {
                return false;
            };
            // `toolbar_layout` returns output-local POINTS while the selection is CAPTURE
            // space, so bring the SELECTION into this output's point space to compare
            // (DRAGON-448 — the old shift-the-toolbar-to-global direction compared a point
            // rect against a physical one on a scaled Windows monitor).
            let units = o.units();
            let (cx, cy) = units.to_point((sel.x, sel.y));
            // The pair form of `len_to_point`; on the letterbox fallback bridge
            // (`lab/flatpak`) `to_point` above already carried the bar offsets, and the
            // extent scales uniformly. Identical arithmetic everywhere else.
            let (cw, ch) = units.size_f_to_point((sel.width as f32, sel.height as f32));
            let overlap_x = tb.x < cx + cw && tb.x + tb.width > cx;
            let overlap_y = tb.y < cy + ch && tb.y + tb.height > cy;
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

    /// Arm the SESSION-level bound for a recording just started to `out_path` (DRAGON-423),
    /// and note the two files it watches: the temp the muxer writes (which the live size
    /// readout also tracks) and the finished file finalize will produce from it.
    ///
    /// Shared by both entry points so no recording can be started without a bound on it —
    /// the reason this is a helper rather than four lines copied twice.
    fn arm_session_bound(&mut self, out_path: &std::path::Path) {
        self.recording_path = Some(crate::record::recording_temp_path(out_path));
        self.recording_out_path = Some(out_path.to_path_buf());
        self.recording_stopping = false;
        self.recording_progress =
            Some(crate::record::progress::SessionProgress::new(std::time::Instant::now()));
    }

    /// Drop every trace of a live recording from the app's own state — the worker handle,
    /// the cross-process marker, the elapsed/pause bookkeeping, the session-level bound, the
    /// tray and the meters.
    ///
    /// The ONE place that says what "no longer recording" means, shared by the ordinary end
    /// (`RecordingPoll` seeing a result) and by giving up on a wedged one, so the two cannot
    /// drift. `recording_path` is deliberately NOT cleared here: what happens to the temp
    /// differs between them (deleted after a normal finalize, already salvaged after a
    /// wedge), and that difference is exactly what must not be laundered.
    pub(super) fn clear_recording_state(&mut self) {
        self.recording = None;
        // DRAGON-322: the recording ended (this process lives on into the video preview) —
        // drop the cross-process marker now so other overlays re-enable their video kind
        // promptly.
        crate::instance::set_recording_marker(false);
        self.recording_started = None;
        // DRAGON-659: the promotion instant goes with the recording it timed. Nothing reads
        // it once `self.recording` is `None` (`warming_spinner` tests that first), but this
        // is the ONE place that says what "no longer recording" means, so it says all of it.
        self.recording_promoted_at = None;
        // DRAGON-661: the live-declaration latch belongs to the recording that raised it, so
        // the next one declares itself from scratch.
        self.recording_live_declared = false;
        self.recording_paused_at = None;
        self.recording_paused_accum = std::time::Duration::ZERO;
        self.recording_stopping = false;
        self.recording_progress = None;
        self.recording_out_path = None;
        self.end_recording_tray();
        self.mic_level = 0.0;
        self.sys_level = 0.0;
    }

    /// Feed the session-level bound one observation (DRAGON-423), from the recording poll.
    /// `Some` means this recording has gone the whole budget without progress.
    pub(super) fn observe_recording_progress(
        &mut self,
    ) -> Option<crate::record::progress::Stall> {
        let phase = self.recording_phase();
        let sample = crate::record::progress::Sample::read(
            self.recording_path.as_deref(),
            self.recording_out_path.as_deref(),
        );
        self.recording_progress
            .as_mut()?
            .observe(std::time::Instant::now(), phase, sample)
    }

    /// Give up on a recording that stopped making progress (DRAGON-423).
    ///
    /// Everything the session started goes — its muxers, its FIFOs, its temp — through
    /// `recover::abandon_session`, which is the same salvage path a crashed session's
    /// wreckage takes: a take with real content in it is RENAMED to `<stamp>-recovered.mkv`,
    /// never deleted. Then the user is told, through the two channels that already exist
    /// (DRAGON-419's log and DRAGON-415's alert), and the one-shot session ends.
    ///
    /// The worker thread is not joined and does not need to be: killing its muxer makes its
    /// writes fail, and `fail_session` ends this process, which is what the DRAGON-421
    /// tether is waiting for to reap anything still breathing.
    pub(super) fn abandon_wedged_recording(
        &mut self,
        stall: crate::record::progress::Stall,
    ) -> Task<cosmic::Action<Msg>> {
        // Ask the worker to stop too. It may be past caring, but a worker that is merely
        // slow gets to unwind through its own path rather than being cut off mid-write.
        if let Some(rec) = &self.recording {
            rec.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let swept = crate::record::recover::abandon_session();
        let cancelled = self.recording_cancelled;
        self.clear_recording_state();
        self.recording_cancelled = false;
        // The temp is NOT deleted here (the ordinary end does delete it): `abandon_session`
        // has already salvaged it into a recovered file, and removing our reference is all
        // that is left to do.
        self.recording_path = None;
        crate::diag::note_failure(
            crate::diag::Failure::RecordingWedged,
            &crate::record::progress::wedge_detail(&stall, &swept.recovered),
        );
        if cancelled {
            // The user threw this take away before it wedged, and a wedge does not undo
            // that: salvaging would hand them the file they said no to, and an alert would
            // report a failure they had already ended themselves. The log still carries the
            // full diagnosis (the note above, plus what `abandon_session` reported), so
            // nothing is hidden from US — only from a user who has moved on.
            for p in &swept.recovered {
                let _ = std::fs::remove_file(p);
            }
            return self.finish_session();
        }
        self.fail_session()
    }

    /// What the live recording has been asked to do right now — the input the session-level
    /// bound judges progress against. A stop the user has asked for outranks a pause: the
    /// worker's `stop` wins over `paused` too, so the session really is stopping.
    fn recording_phase(&self) -> crate::record::progress::Phase {
        use crate::record::progress::Phase;
        if self.recording_stopping {
            Phase::Stopping
        } else if self.recording_paused_at.is_some() {
            Phase::Paused
        } else {
            Phase::Running
        }
    }

    /// Record-start snapshot of the finished recording's editor anchor: the trigger
    /// display, else the selection's output, plus that output's backing scale. Taken on
    /// the portal-fallback path only (`lab/flatpak`), where `recreate_active_overlays`
    /// tears the selection window and `self.outputs` down the moment recording starts,
    /// so a stop-time resolution can no longer see any output. That empty answer is what
    /// the daemon-tray "Finish & Save did nothing" report was: `preview_output` stayed
    /// `None`, the preview spinner refused to open, and `present_capture` delivered the
    /// file through the editor-less `finish_share` with no editor ever appearing.
    ///
    /// `pub(super)` since DRAGON-563: a TRAY countdown tears the same surfaces down at
    /// COUNTDOWN start, so `enter_countdown` takes the same snapshot for the delayed
    /// still/recording it is counting toward (`keep_countdown_anchor` reads it back).
    pub(super) fn snapshot_preview_anchor(&mut self, sel: &Selection) {
        self.preview_output =
            self.active_trigger_display().or_else(|| self.output_for_selection(sel));
        self.preview_output_scale = self.scale_for_selection(sel);
    }

    /// Stop the recording: signal the worker (it finalizes the file) and clear the
    /// overlay, opening the video preview overlay (a spinner) right away to cover the
    /// finalize wait. `RecordingPoll` fills in the poster once the file is ready.
    pub(super) fn stop_recording(&mut self) -> Task<cosmic::Action<Msg>> {
        self.end_recording_tray();
        // DRAGON-423: remember that the USER asked, independently of the flag below — a
        // worker is free to clear that one, and one that did is what left a recording
        // running behind a spinner nobody could dismiss.
        self.recording_stopping = true;
        if let Some(rec) = &self.recording {
            rec.stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // DRAGON-309: open the finalized-recording preview on the TRIGGER display (the monitor
        // active when the recording was initiated), NOT the recorded region's monitor, matching
        // the still-capture path. Fall back to the selection's output when the trigger can't be
        // resolved. Captured before the overlay (and `self.outputs`) tears down; the finalize
        // pass is a file op (no live-screen read), so the preview overlay is safe to show.
        //
        // lab/flatpak: on the portal-fallback path the overlays (and `self.outputs`) went
        // down at record START, so the fresh resolution below always comes back empty. The
        // anchor snapshotted then (`snapshot_preview_anchor`) stands in, with the scale it
        // stored riding along. Overwriting the pair with the empty stop-time answer is what
        // left the finished recording with no output to open its editor on, which the user
        // saw as "Finish & Save from the daemon tray does nothing".
        if let Some(sel) = self.pending.clone() {
            let fresh = self
                .active_trigger_display()
                .or_else(|| self.output_for_selection(&sel))
                .map(|a| (a, self.scale_for_selection(&sel)));
            let snapshot = self.preview_output.take().map(|a| (a, self.preview_output_scale));
            if let Some((anchor, scale)) = stop_preview_anchor(fresh, snapshot) {
                self.preview_output = Some(anchor);
                self.preview_output_scale = scale;
            }
            // DRAGON-317 regression fix: the windowed-preview re-home target is the RELIABLE
            // capture-origin monitor ONLY — the pointer's output from the capture overlay's
            // first pointer-enter (`capture_pointer_output`), not the focused-toplevel guess;
            // None suppresses the move so cosmic-comp's native pointer-output placement stands.
            // Cached before `destroy_surfaces` (below) clears `self.outputs`. Mirrors the
            // still-capture path in `capture_flow.rs`.
            #[cfg(target_os = "linux")]
            {
                self.preview_output_name = self.capture_pointer_output.clone();
            }
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
        self.recording_stopping = true; // DRAGON-423 — see `stop_recording`
        if let Some(rec) = &self.recording {
            rec.stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Task::batch(self.destroy_surfaces())
    }

    /// Destination file for a new recording of `sel`: `<dir>/<stem>.mp4` (same
    /// `<timestamp>[-<descriptor>]` naming as screenshots).
    ///
    /// DRAGON-467: `<dir>` is the configured `record_dir`, or the session runtime directory
    /// when the Video Editor's "Automatically save originals" is off — the same fork stills
    /// take (`App::capture_write_dir`). The recording TEMP (`record::recording_temp_path`)
    /// is derived from this path, so it follows the recording rather than being stranded in
    /// a folder the finished file never reaches.
    pub(super) fn record_output_path(&self, sel: &Selection) -> std::path::PathBuf {
        self.capture_write_dir(true)
            .join(super::capture_flow::recording_save_name(&self.capture_stem(sel)))
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

/// Whether the record chip wears its WARMING face at `now` (DRAGON-659). Pure,
/// unit-tested.
///
/// * `promoted_at`: when `self.recording` became `Some`.
/// * `ready_at`: when the thing being waited for happened, or `None` while it still has
///   not. Since DRAGON-661 that is the worker's SETTLED instant (`RecordHandle::settled_at`,
///   the end of its opening phase), not its first captured frame: the frame is the START of
///   a bring-up that still has the audio pre-flight, the ffmpeg spawn and the opening
///   catch-up to go, and a spinner that clears there clears mid-setup.
/// * `reveal_delay`: how long to wait before showing anything (`ZERO` after a countdown,
///   [`WARM_SPINNER_REVEAL_DELAY`] otherwise).
/// * `min_hold`: how long a revealed spinner stays, [`WARM_SPINNER_MIN_HOLD`].
///
/// The rule in one line: a warmup that beat the reveal delay is never shown at all, and one
/// that did not is shown for at least `min_hold` past the reveal, whether or not it has
/// landed by then. So the glyph either never appears or is legible, and never blinks.
///
/// The boundary is deliberately `<=`, not `<`: a warmup that lands exactly ON the reveal
/// instant has nothing left to report, and the countdown case (where `reveal_delay` is
/// `ZERO` and `ready_at` precedes `promoted_at`, saturating to a zero offset) rides that
/// same comparison rather than needing an arm of its own.
fn warm_spinner_visible(
    promoted_at: std::time::Instant,
    ready_at: Option<std::time::Instant>,
    reveal_delay: std::time::Duration,
    min_hold: std::time::Duration,
    now: std::time::Instant,
) -> bool {
    match ready_at {
        // Still warming: shown from the reveal onward, with no upper bound. A worker that
        // never settles is a worker that is failing, and its failure ends the recording
        // through `RecordingPoll`; until then the spinner is the honest face.
        None => now.saturating_duration_since(promoted_at) >= reveal_delay,
        Some(ready_at) => {
            let warm_offset = ready_at.saturating_duration_since(promoted_at);
            if warm_offset <= reveal_delay {
                false
            } else {
                // Both bounds, not just the upper one. Live, only the upper bound can ever
                // decide anything (before the reveal, `ready_at` is still in the future, so
                // the `None` arm above is what answers). Stating the lower bound anyway
                // keeps the function TOTAL: it gives the right answer for any pair a test
                // hands it, instead of being right only for the pairs a clock can produce.
                let reveal = promoted_at + reveal_delay;
                now >= reveal && now < reveal + min_hold
            }
        }
    }
}

/// What one recording poll adopts from the worker's SETTLED signal (DRAGON-661, rewritten
/// by DRAGON-673). Pure, unit-tested.
///
/// * `anchor_set` / `live_declared`: the two once-per-recording latches, already taken.
/// * `settled_at`: the end of the worker's opening phase (`RecordHandle::settled_at`), which
///   is MEDIA 0 — where the file begins — or `None` while it is still coming up.
///
/// Returns the elapsed anchor to adopt (`None` = leave it alone) and whether THIS poll is
/// the one that declares the recording live.
///
/// ONE signal, for both, and that is the fix. It used to take `warm_at` as well and anchor
/// the elapsed readout there, on the premise that the confirmed first frame is "where the
/// file's real content begins". DRAGON-672 made that premise false (media 0 became the
/// settled pipeline, so everything before it is dropped) and DRAGON-673 made it visible: the
/// worker is spawned at countdown START, so `warm_at` lands a whole countdown early and a 10s
/// countdown opened its recording at 0:10. The readout must count the file, and the file
/// begins where the app declares itself live, so the two are the same instant by
/// construction.
///
/// The old guard ("never declared live without an anchor to count from") is now structural
/// rather than tested: both halves read the same `Option`, so live cannot be declared with no
/// anchor even in principle.
fn warm_adoption(
    anchor_set: bool,
    live_declared: bool,
    settled_at: Option<std::time::Instant>,
) -> (Option<std::time::Instant>, bool) {
    let anchor = if anchor_set { None } else { settled_at };
    let declare_live = !live_declared && settled_at.is_some();
    (anchor, declare_live)
}

/// Which face the record chip wears. See [`chip_face`] for the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChipFace {
    /// No recording: the countdown's remaining seconds, or the idle delay readout.
    Idle,
    /// Promoted, but the worker's pipeline has not settled yet. `spinning` is
    /// whether the wait has run long enough to be worth REVEALING
    /// ([`warm_spinner_visible`]): revealed, the face is the turning loader; before that it
    /// is a still record dot, which says "starting" without either claiming to be live or
    /// flashing loading chrome on a warmup that beats the reveal.
    Warming { spinning: bool },
    /// Live: the STOP glyph plus the elapsed readout, and the only face that takes a press.
    /// Reached ONLY through a settled pipeline.
    Live,
}

/// Which face the record chip wears, from the three facts that decide it. Pure, unit-tested.
///
/// * `recording` — a worker is PROMOTED (`self.recording` is `Some`).
/// * `settled` — that worker's opening phase is over (`RecordHandle::settled_at`), so the
///   pipeline is genuinely steady.
/// * `spinner` — the warming spinner is past its reveal and inside its hold
///   ([`warm_spinner_visible`]).
///
/// The rule this exists to enforce is the first line of the body: `Live`, the STOP face, is
/// reachable only through `settled`. The view used to ask `recording` instead, and
/// `recording` is true from PROMOTION, which is a whole warmup before there is anything to
/// stop. That read correctly whenever the spinner was up, and wrongly in the gap BEFORE the
/// spinner is revealed, so a fresh no-countdown recording showed STOP, then the spinner,
/// then STOP again. Only the no-countdown path could reach it: a countdown promotion reveals
/// at zero delay, so it has no gap.
///
/// DRAGON-661 moved this second fact from the first captured frame to the settled instant.
/// The frame only proves the capture stream works; the audio pre-flight, the ffmpeg spawn
/// and the opening catch-up all follow it (600ms on one measured session), and a STOP face
/// during that span points straight at the part of the recording still being papered over.
///
/// `spinner` is tested before `settled` because it outlives the warmup by design:
/// [`WARM_SPINNER_MIN_HOLD`] keeps a revealed spinner up for a moment after the pipeline
/// settles, so the glyph cannot blink off the instant it appeared.
fn chip_face(recording: bool, settled: bool, spinner: bool) -> ChipFace {
    if !recording {
        ChipFace::Idle
    } else if spinner {
        ChipFace::Warming { spinning: true }
    } else if settled {
        ChipFace::Live
    } else {
        ChipFace::Warming { spinning: false }
    }
}

/// Which preview anchor a stopping recording keeps. Pure, unit-tested (lab/flatpak).
///
/// The STOP-TIME resolution wins whenever it produced one: on the native path the
/// capture surfaces are still up at stop, so the fresh answer reflects the pointer and
/// outputs as they are NOW (the DRAGON-309 trigger-display rule). When it produced
/// nothing, the RECORD-START snapshot stands: the portal-fallback path tears the
/// selection window and the output list down at record start, so its stop-time
/// resolution is always empty, and erasing the snapshot with that empty answer is what
/// made "Finish & Save" from the daemon tray end a recording with no editor. Both
/// absent stays absent: the editor-less `finish_share` delivery is then the honest
/// route, exactly as before.
fn stop_preview_anchor<T>(fresh: Option<T>, start_snapshot: Option<T>) -> Option<T> {
    fresh.or(start_snapshot)
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
mod stop_preview_anchor_tests {
    use super::stop_preview_anchor;

    // The lab/flatpak live-test bug, distilled: the daemon tray's "Finish & Save"
    // reached the child and the file was written, but the finished recording opened no
    // editor, because the stop-time anchor resolution ran against an output list the
    // portal-fallback path had torn down at record START, and its empty answer erased
    // the record-start snapshot.

    #[test]
    fn a_fresh_resolution_wins() {
        // The native path: the surfaces are still up at stop, so the fresh answer is
        // the better one and must shadow any record-start snapshot.
        assert_eq!(stop_preview_anchor(Some("fresh"), Some("start")), Some("fresh"));
        assert_eq!(stop_preview_anchor(Some("fresh"), None), Some("fresh"));
    }

    #[test]
    fn an_empty_resolution_keeps_the_start_snapshot() {
        // The portal-fallback path: the outputs went down at record start, so the
        // stop-time resolution is empty and the snapshot must stand instead of being
        // erased. This is the "Finish & Save did nothing" fix.
        assert_eq!(stop_preview_anchor(None, Some("start")), Some("start"));
    }

    #[test]
    fn nothing_resolvable_stays_none() {
        // No anchor from either moment: the editor-less delivery is then the honest
        // route, exactly as before this decision existed.
        assert_eq!(stop_preview_anchor::<&str>(None, None), None);
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

/// DRAGON-659: the warming spinner's reveal + hold rule, as a table. The four cases below
/// are the whole behavior the ticket asks for, and the reason the rule is a pure function
/// rather than three conditions inlined in a view.
#[cfg(test)]
mod warm_spinner_visible_tests {
    use super::{warm_spinner_visible, WARM_SPINNER_MIN_HOLD, WARM_SPINNER_REVEAL_DELAY};
    use std::time::{Duration, Instant};

    /// The happy no-countdown case: a warmup faster than the reveal delay never shows a
    /// spinner, at any point afterwards.
    #[test]
    fn a_warmup_that_beats_the_reveal_is_never_shown() {
        let promoted = Instant::now();
        let warm = promoted + Duration::from_millis(120);
        for after in [0, 50, 120, 200, 400, 5_000] {
            assert!(
                !warm_spinner_visible(
                    promoted,
                    Some(warm),
                    WARM_SPINNER_REVEAL_DELAY,
                    WARM_SPINNER_MIN_HOLD,
                    promoted + Duration::from_millis(after),
                ),
                "a 120ms warmup must stay invisible, and it did not at {after}ms"
            );
        }
    }

    /// A slow no-countdown warmup: nothing before the reveal, the spinner from there, and
    /// it is HELD past the moment the warmup actually landed rather than blinking off with
    /// it.
    #[test]
    fn a_slow_warmup_is_revealed_then_held() {
        let promoted = Instant::now();
        let warm = promoted + Duration::from_millis(250);
        let at = |ms: u64| {
            warm_spinner_visible(
                promoted,
                Some(warm),
                WARM_SPINNER_REVEAL_DELAY,
                WARM_SPINNER_MIN_HOLD,
                promoted + Duration::from_millis(ms),
            )
        };
        assert!(!at(0), "nothing shows before the reveal delay");
        assert!(!at(199), "nothing shows before the reveal delay");
        assert!(at(200), "the spinner is revealed at the delay");
        assert!(at(250), "and stays through the instant the warmup landed");
        assert!(at(499), "and is held for the full minimum");
        assert!(!at(500), "then it is gone for good");
    }

    /// Still warming (`settled_at` unset): shown from the reveal onward. This is also the
    /// state a worker that never settles sits in, and the spinner is the honest face for it
    /// until `RecordingPoll` ends the recording.
    #[test]
    fn a_recording_that_has_not_warmed_yet_shows_from_the_reveal() {
        let promoted = Instant::now();
        let at = |ms: u64| {
            warm_spinner_visible(
                promoted,
                None,
                WARM_SPINNER_REVEAL_DELAY,
                WARM_SPINNER_MIN_HOLD,
                promoted + Duration::from_millis(ms),
            )
        };
        assert!(!at(0));
        assert!(!at(199));
        assert!(at(200));
        assert!(at(10_000), "a worker that never warms keeps the spinner");
    }

    /// The countdown case (DRAGON-673), which now uses the SAME reveal delay as any other
    /// promotion. A prewarmed worker holds at the start gate and settles a few milliseconds
    /// after the promotion opens it, so the settle sits well inside the delay and shows
    /// nothing. This is why the countdown's old `ZERO` delay had to go: with the gate, that
    /// zero would flash the spinner on every countdown recording.
    #[test]
    fn a_gated_countdown_settle_lands_inside_the_reveal_and_shows_nothing() {
        let promoted = Instant::now();
        let gated = promoted + Duration::from_millis(8);
        for after in [0, 8, 50, 200, 400, 5_000] {
            assert!(
                !warm_spinner_visible(
                    promoted,
                    Some(gated),
                    WARM_SPINNER_REVEAL_DELAY,
                    WARM_SPINNER_MIN_HOLD,
                    promoted + Duration::from_millis(after),
                ),
                "the gated settle must stay invisible, and it did not at {after}ms"
            );
        }
    }

    /// The other half of the countdown case: a worker that had NOT reached the gate at zero
    /// is genuinely late, and is shown from the reveal exactly like any other slow warmup.
    #[test]
    fn a_countdown_worker_that_missed_the_gate_is_still_revealed() {
        let promoted = Instant::now();
        let at = |ms: u64| {
            warm_spinner_visible(
                promoted,
                None,
                WARM_SPINNER_REVEAL_DELAY,
                WARM_SPINNER_MIN_HOLD,
                promoted + Duration::from_millis(ms),
            )
        };
        assert!(!at(199), "nothing shows before the reveal delay");
        assert!(at(200), "a worker still warming at countdown zero shows from the reveal");
    }

    /// The exact boundary, pinned because it is the one place a `<` would change the
    /// answer: a warmup landing ON the reveal instant has nothing left to report.
    #[test]
    fn a_warmup_landing_exactly_on_the_reveal_is_not_shown() {
        let promoted = Instant::now();
        let warm = promoted + WARM_SPINNER_REVEAL_DELAY;
        assert!(!warm_spinner_visible(
            promoted,
            Some(warm),
            WARM_SPINNER_REVEAL_DELAY,
            WARM_SPINNER_MIN_HOLD,
            promoted + WARM_SPINNER_REVEAL_DELAY,
        ));
        // One microsecond later it IS shown, so the edge is exactly where it says it is.
        let warm = promoted + WARM_SPINNER_REVEAL_DELAY + Duration::from_micros(1);
        assert!(warm_spinner_visible(
            promoted,
            Some(warm),
            WARM_SPINNER_REVEAL_DELAY,
            WARM_SPINNER_MIN_HOLD,
            promoted + WARM_SPINNER_REVEAL_DELAY,
        ));
    }
}

/// DRAGON-659: the record chip's face, as a table over the three facts. The first test is
/// the invariant the whole function exists for; the rest are the states a real session walks
/// through, in order.
#[cfg(test)]
mod chip_face_tests {
    use super::{chip_face, ChipFace};

    /// THE rule: the STOP face may never show unless the worker's pipeline has SETTLED.
    /// Every combination that is not settled has to answer something else, whatever the
    /// other two facts say.
    #[test]
    fn the_stop_face_needs_a_settled_pipeline() {
        for recording in [false, true] {
            for spinner in [false, true] {
                assert_ne!(
                    chip_face(recording, false, spinner),
                    ChipFace::Live,
                    "not settled must never read as live (recording={recording}, \
                     spinner={spinner})"
                );
            }
        }
    }

    /// A fresh no-countdown recording, tick by tick: promoted with the pipeline still coming
    /// up and nothing revealed yet (the gap this fix is about), then the revealed spinner,
    /// then live. The middle step is the one that used to answer `Live`.
    #[test]
    fn a_no_countdown_recording_walks_gap_then_spinner_then_live() {
        assert_eq!(chip_face(true, false, false), ChipFace::Warming { spinning: false });
        assert_eq!(chip_face(true, false, true), ChipFace::Warming { spinning: true });
        assert_eq!(chip_face(true, true, false), ChipFace::Live);
    }

    /// The minimum-hold window: the pipeline has settled but the revealed spinner is still
    /// being held, so the chip keeps spinning rather than snapping to STOP mid-turn. This is
    /// why the spinner is asked about before `settled`.
    #[test]
    fn a_held_spinner_outlives_the_warmup() {
        assert_eq!(chip_face(true, true, true), ChipFace::Warming { spinning: true });
    }

    /// No worker, no chip: the countdown digits and the idle delay readout share this face,
    /// and neither cares what a stale settled flag says.
    #[test]
    fn no_recording_is_always_the_idle_face() {
        assert_eq!(chip_face(false, false, false), ChipFace::Idle);
        assert_eq!(chip_face(false, true, false), ChipFace::Idle);
    }
}

/// DRAGON-661: what a poll adopts from the worker's two startup signals. The table is the
/// whole point of the ticket: the elapsed anchor and the live declaration key on DIFFERENT
/// signals, so a session walks through a state where one has been taken and the other has
/// not.
#[cfg(test)]
mod warm_adoption_tests {
    use super::warm_adoption;
    use std::time::{Duration, Instant};

    /// The startup, in order: a worker still coming up gives the poll nothing, and the one
    /// poll that sees `settled_at` takes BOTH halves — the anchor and the live declaration —
    /// because they are the same instant.
    #[test]
    fn a_session_adopts_nothing_until_it_settles_then_takes_both() {
        let settled = Instant::now();

        assert_eq!(warm_adoption(false, false, None), (None, false));
        assert_eq!(warm_adoption(false, false, Some(settled)), (Some(settled), true));
    }

    /// The DRAGON-673 case, and the reason the anchor moved. A countdown's worker confirms
    /// its first frame at countdown START and settles a whole countdown later, so anchoring
    /// on the frame opened a 10s countdown's recording at 0:10. The anchor is the SETTLED
    /// instant, which is media 0 — the elapsed readout therefore reads 0:00 exactly when the
    /// file begins.
    #[test]
    fn the_anchor_is_media_zero_not_the_first_frame_a_countdown_ago() {
        let first_frame = Instant::now();
        let settled = first_frame + Duration::from_secs(10);
        let (anchor, live) = warm_adoption(false, false, Some(settled));
        assert_eq!(anchor, Some(settled), "the readout counts the FILE, not the warmup");
        assert!(live);
        assert!(
            anchor.expect("anchored") > first_frame,
            "a countdown's first frame is long past by the time the file starts"
        );
    }

    /// Both latches are once-per-recording: every later poll of a live recording adopts
    /// nothing, which is what keeps the tray from being re-raised every 100ms.
    #[test]
    fn an_already_live_recording_adopts_nothing_further() {
        let settled = Instant::now();
        assert_eq!(warm_adoption(true, true, Some(settled)), (None, false));
    }

    /// The halves still latch INDEPENDENTLY, so a poll that already anchored can still be the
    /// one that declares live. Unreachable while both read one `Option`, and pinned so a
    /// future signal split cannot silently re-raise the tray or lose the declaration.
    #[test]
    fn an_anchored_but_undeclared_recording_still_declares_live() {
        let settled = Instant::now();
        assert_eq!(warm_adoption(true, false, Some(settled)), (None, true));
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
