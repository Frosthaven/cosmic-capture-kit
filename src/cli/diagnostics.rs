/// Dispatch the hidden test/diagnostic harnesses behind a single `--test <name> [args]`
/// flag. These are developer tools (benchmarks, capture/encode/audio probes), never part
/// of the normal capture flow. `rest` is the arguments following the subcommand name.
pub fn run_test(name: &str, rest: &[String]) {
    // The portal source-type + PipeWire/record diagnostics are Linux-only (ashpd).
    #[cfg(target_os = "linux")]
    let source_type = |rest: &[String]| {
        if rest.iter().any(|a| a == "window") {
            ashpd::desktop::screencast::SourceType::Window
        } else {
            ashpd::desktop::screencast::SourceType::Monitor
        }
    };
    let arg = |n: usize| rest.get(n).map(String::as_str).unwrap_or("");
    match name {
        "selftest" => selftest(),
        "audio" => audio_input_test(),
        "mic-rec" => mic_rec_test(),
        "scan" => scan_test(arg(0)),
        "ocr-bench" => ocr_bench(std::path::Path::new(if arg(0).is_empty() { "." } else { arg(0) })),
        // A/B harness: run `<in.raw>` (f32le mono 48k) through WebRTC NS, RNNoise, and
        // the WebRTC->RNNoise cascade; write `<prefix>.{webrtc,rnnoise,cascade}.raw`.
        "denoise" => denoise_test(arg(0), if arg(1).is_empty() { "/tmp/dn" } else { arg(1) }),
        "monitor-latency" => monitor_latency_test(),
        "capture-relay" => capture_relay_test(),
        "bench-capture" => crate::screenshot::bench_window_capture(),
        "cursor-capture" => cursor_capture_test(),
        "backend" => backend_test(),
        #[cfg(target_os = "macos")]
        "mac-shot" => mac_shot_test(arg(0)),
        #[cfg(target_os = "macos")]
        "mac-active-shot" => mac_active_shot_test(),
        #[cfg(target_os = "macos")]
        "mac-daemon-repro" => mac_daemon_repro_test(arg(0), arg(1)),
        #[cfg(target_os = "macos")]
        "mac-grab-id" => mac_grab_id_test(arg(0), arg(1)),
        #[cfg(target_os = "macos")]
        "mac-focus-shot" => mac_focus_shot_test(arg(0)),
        #[cfg(target_os = "macos")]
        "mac-rec-bench" => mac_rec_bench(rest),
        #[cfg(target_os = "macos")]
        "mac-wallpaper" => mac_wallpaper_test(arg(0)),
        #[cfg(target_os = "macos")]
        "mac-window-composite" => mac_window_composite_test(arg(0)),
        #[cfg(target_os = "macos")]
        "mac-list-windows" => {
            for line in crate::platform::mac::dump_windows() {
                println!("{line}");
            }
        }
        // DRAGON-229 M1 Windows capture checkpoints (W1-SPEC §7). Each dispatches into
        // the Windows plugin primitives (`crate::screenshot` / `platform::windows`) and
        // does portable PNG/print glue — the mac-route pattern.
        #[cfg(target_os = "windows")]
        "windows-monitors" => windows_monitors_test(),
        #[cfg(target_os = "windows")]
        "windows-shot-output" => windows_shot_output_test(arg(0)),
        #[cfg(target_os = "windows")]
        "windows-list" => windows_list_test(),
        #[cfg(target_os = "windows")]
        "windows-shot-window" => windows_shot_window_test(arg(0)),
        #[cfg(target_os = "windows")]
        "windows-thumbs" => windows_thumbs_test(),
        #[cfg(target_os = "windows")]
        "windows-cursor" => windows_cursor_test(),
        #[cfg(target_os = "windows")]
        "windows-wallpaper" => windows_wallpaper_test(arg(0)),
        #[cfg(target_os = "windows")]
        "windows-window-composite" => windows_window_composite_test(arg(0), arg(1)),
        #[cfg(target_os = "windows")]
        "windows-shot-region" => {
            windows_shot_region_test(arg(0), arg(1), arg(2), arg(3))
        }
        // DRAGON-229 M3 recording checkpoints (W3-SPEC §8). S1: one WGC frame → PNG. S2/S4:
        // a real recording (monitor mode) through the owned media-clock pipeline.
        #[cfg(target_os = "windows")]
        "windows-wgc-frame" => windows_wgc_frame_test(arg(0), arg(1), arg(2), arg(3), arg(4)),
        #[cfg(target_os = "windows")]
        "windows-record" => windows_record_test(rest),
        // DRAGON-282: the ACTIVE WASAPI render endpoints (id + friendly name) the Output-device
        // picker offers and the recorder loops back — the enumeration proof.
        #[cfg(target_os = "windows")]
        "windows-audio-endpoints" => windows_audio_endpoints_test(),
        "bench-encoders" => {
            let w: u32 = arg(0).parse().unwrap_or(3840);
            let h: u32 = arg(1).parse().unwrap_or(2160);
            let cap = arg(2) == "capture" || arg(2) == "pipeline";
            eprintln!("bench-encoders at {w}x{h} ({})", if cap { "full pipeline (capture+encode)" } else { "encoder-only" });
            for e in crate::encode::available_encoders() {
                let s = crate::encode::bench_encoder_pipeline(
                    &e.id, w, h, 8000, &crate::encode::Presets::default(), 1.5, cap,
                );
                eprintln!("{:<52} => {:.0} fps, ~{:.1} CPU cores", e.label, s.fps, s.cores);
            }
        }
        #[cfg(target_os = "linux")]
        "bench-record" => {
            let secs = rest.first().and_then(|s| s.parse().ok()).unwrap_or(3);
            let fps = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(60);
            crate::record::bench_record(secs, fps, if arg(2).is_empty() { "gpu" } else { arg(2) });
        }
        #[cfg(target_os = "linux")]
        "pw" => pw_test(source_type(rest)),
        #[cfg(target_os = "linux")]
        "linux-focus-probe" => linux_focus_probe(arg(0)),
        #[cfg(target_os = "linux")]
        "glass-shot" => glass_shot(arg(0)),
        #[cfg(feature = "zero-copy")]
        "dmabuf" => dmabuf_test(source_type(rest)),
        #[cfg(feature = "zero-copy")]
        "screencopy-dmabuf" => println!("{}", crate::record::screencopy_dmabuf_test()),
        "" | "help" | "list" => print_test_help(),
        other => {
            eprintln!("unknown test '{other}'\nbackend                           probe every capture backend (caps + outputs + windows + cursor)\n");
            print_test_help();
        }
    }
}

/// `--test linux-focus-probe [id|title-substring]` (DRAGON-194): the empirical proof that
/// driving a toplevel's focus state changes its captured client-side decorations. Lists the
/// toplevels + their `activated` state, grabs the target window's pixels (as-is), activates
/// it via the cosmic toplevel manager (`compositor::activate`), re-lists, then re-grabs — so
/// the before/after PNGs + activated states show that activation flips the `activated` bit
/// and the re-grabbed pixels change ONLY in the titlebar region. With no arg, targets the
/// first NON-active toplevel (grabs it inactive, then active). Linux only (cosmic toplevel
/// manager); needs the screencopy (ext-image-copy-capture) capability to grab.
#[cfg(target_os = "linux")]
fn linux_focus_probe(arg0: &str) {
    use crate::platform::compositor;
    let groups = compositor::list_toplevels();
    let all: Vec<_> = groups.values().flatten().cloned().collect();
    println!("== toplevels (before) ==");
    for t in &all {
        println!("  active={} id={} title={:?} rect={:?}", t.active, t.id, t.title, t.rect);
    }
    let target = if !arg0.is_empty() {
        all.iter().find(|t| t.id == arg0 || t.title.contains(arg0)).cloned()
    } else {
        all.iter().find(|t| !t.active).cloned()
    };
    let Some(t) = target else {
        eprintln!("linux-focus-probe: no target (pass an id/title substring, or open a second window)");
        return;
    };
    println!("target: id={} title={:?} active={}", t.id, t.title, t.active);

    let before = crate::screenshot::windows(std::slice::from_ref(&t.id));
    if let Some(img) = before.get(&t.id) {
        let p = std::env::temp_dir().join("cck-linux-focus-before.png");
        let _ = img.save(&p);
        println!("BEFORE grab {}x{} -> {}", img.width(), img.height(), p.display());
    } else {
        println!("BEFORE grab: none");
    }

    // The capture path's VERIFIED activation (DRAGON-194 follow-up): poll-and-reissue
    // until the state sticks, so the probe exercises exactly what a real capture does.
    let confirmed = compositor::activate_until(&t.id, &t.id, true);
    println!("activate_until confirmed: {confirmed}");
    std::thread::sleep(std::time::Duration::from_millis(200));

    let groups2 = compositor::list_toplevels();
    println!("== toplevels (after activate) ==");
    for t2 in groups2.values().flatten() {
        let marker = if t2.id == t.id { " <== target" } else { "" };
        println!("  active={} id={} title={:?}{marker}", t2.active, t2.id, t2.title);
    }
    let after = crate::screenshot::windows(std::slice::from_ref(&t.id));
    if let Some(img) = after.get(&t.id) {
        let p = std::env::temp_dir().join("cck-linux-focus-after.png");
        let _ = img.save(&p);
        println!("AFTER grab {}x{} -> {}", img.width(), img.height(), p.display());
    } else {
        println!("AFTER grab: none");
    }

    // DEFOCUS leg (DRAGON-194's Inactive appearance): the target is active from the
    // leg above — now activate a DIFFERENT toplevel (the protocol has no deactivate
    // request), verify the target dropped `Activated`, and re-grab it. The dump
    // should render the INACTIVE decorations (dim title / gray controls).
    let other = compositor::list_toplevels()
        .values()
        .flatten()
        .find(|o| o.id != t.id)
        .cloned();
    let Some(other) = other else {
        println!("DEFOCUS leg skipped: no second toplevel to hand focus to");
        return;
    };
    let confirmed = compositor::activate_until(&other.id, &t.id, false);
    println!("defocus via {:?}: activate_until confirmed: {confirmed}", other.title);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let defocused = crate::screenshot::windows(std::slice::from_ref(&t.id));
    if let Some(img) = defocused.get(&t.id) {
        let p = std::env::temp_dir().join("cck-linux-focus-defocused.png");
        let _ = img.save(&p);
        println!("DEFOCUSED grab {}x{} -> {}", img.width(), img.height(), p.display());
    } else {
        println!("DEFOCUSED grab: none");
    }
}

/// `--test glass-shot [id|title-substring]` (DRAGON-218): the empirical proof that the
/// frosted-glass reproduction reaches a REAL single-window capture. Runs the actual
/// `WindowCaptureJob` (transparency + wallpaper-behind on, live grab) against a live
/// toplevel TWICE — once with the glass reader live (blur + grain within the window's
/// rounded footprint) and once with `CCK_NO_GLASS=1` (the sharp historical composite) —
/// and dumps both PNGs so the pair shows blurred vs sharp wallpaper through translucent
/// regions. Prints the resolved glass config so a "no visible difference" run over an
/// opaque window (no alpha to see through) is diagnosable. With no arg, targets the
/// first non-active toplevel (grabbing our own picker would be pointless).
#[cfg(target_os = "linux")]
fn glass_shot(arg0: &str) {
    use crate::platform::compositor;
    let all: Vec<_> = compositor::list_toplevels().values().flatten().cloned().collect();
    let target = if !arg0.is_empty() {
        all.iter().find(|t| t.id == arg0 || t.title.contains(arg0)).cloned()
    } else {
        all.iter().find(|t| !t.active).cloned().or_else(|| all.first().cloned())
    };
    let Some(t) = target else {
        eprintln!("glass-shot: no toplevel (pass an id/title substring, or open a window)");
        return;
    };
    println!("target: id={} title={:?} rect={:?}", t.id, t.title, t.rect);
    match crate::app::theme::glass_config() {
        Some(g) => println!(
            "glass config: strength_ordinal={} alpha={} frosted_windows={}",
            g.strength_ordinal, g.alpha, g.frosted_windows
        ),
        None => println!("glass config: None (off COSMIC, v2 theme unreadable, or CCK_NO_GLASS=1)"),
    }
    // Output geometry for the wallpaper composite (what a real capture snapshots at launch).
    let Some((_conn, _queue, data)) = crate::screencopy::connect(false) else {
        eprintln!("glass-shot: screencopy unavailable");
        return;
    };
    let conn_geom: Vec<crate::screenshot::OutputGeom> = crate::screencopy::outputs(&data)
        .into_iter()
        .map(|(_, name, pos, size)| (name, pos, size))
        .collect();
    if conn_geom.is_empty() {
        eprintln!("glass-shot: no outputs enumerated");
        return;
    }
    println!("outputs: {conn_geom:?}");
    let (wx, wy, ww, wh) = t.rect;
    let sel = crate::selection::Selection {
        x: wx,
        y: wy,
        width: ww.max(1) as u32,
        height: wh.max(1) as u32,
        output: None,
        window_id: Some(t.id.clone()),
    };
    // The job a real single-window capture builds (capture_flow.rs), pinned to the
    // glass-relevant combination: transparency + wallpaper ON, shadow + padding on
    // (their margins show the sharp-outside/frosted-inside boundary), no border
    // (the ring would cover the footprint edge under inspection).
    let job = |label: &str| {
        let out = crate::screenshot::WindowCaptureJob {
            id: t.id.clone(),
            cursor: false,
            sel: sel.clone(),
            capture_transparency: true,
            capture_wallpaper: true,
            window_radius: crate::app::theme::window_radius(),
            border: crate::decoration::BorderSpec { width: 0, color: [0, 0, 0, 0] },
            window_shadow: true,
            pad_logical: 32.0,
            dark: crate::app::theme::theme_is_dark(),
            frozen_geom: conn_geom.clone(),
            frozen_px: None,
            cursor_overlay: None,
        }
        .run();
        match out {
            Some(img) => {
                let p = std::env::temp_dir().join(format!("cck-glass-shot-{label}.png"));
                let _ = img.save(&p);
                println!("{label} composite {}x{} -> {}", img.width(), img.height(), p.display());
            }
            None => println!("{label} composite: grab failed"),
        }
    };
    job("glass-on"); // glass per the live theme (a no-op pair on an unfrosted theme)
    // A/B leg: same job with the reader killed — the historical sharp composite.
    // Single-threaded CLI process; set_var's concurrent-getenv caveat doesn't apply.
    unsafe { std::env::set_var("CCK_NO_GLASS", "1") };
    job("glass-off");
    unsafe { std::env::remove_var("CCK_NO_GLASS") };
}

/// `--test monitor-latency`: point the DRAGON-119 device-latency probe at the current
/// default sink's monitor and print its signed record-stream latency — the value
/// auto mode folds into a recording's SYSTEM channel. On a real hardware sink this is
/// ~the device output latency; on a virtual / null / suspended sink it is 0 (no device
/// buffer — fail-open). Never persisted; sampled live per recording.
fn monitor_latency_test() {
    let Some(probe) = crate::audio::MonitorLatencyProbe::start() else {
        eprintln!("monitor-latency: probe unavailable on this platform");
        return;
    };
    println!("monitor-latency: sampling the default sink's monitor for ~1.5s…");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let ms = probe.stop();
    println!("monitor-latency: median device latency = {ms:.1} ms");
    println!(
        "  (0.0 = a virtual/suspended sink with no device buffer, or no reachable pulse \
         server; real hardware reports its output latency here)"
    );
}

/// `--test capture-relay`: the permanent empirical probe for the DRAGON-126 class of
/// bug — start a [`crate::audio::capture::MonitorCapture`] on the default sink's monitor,
/// consume its chunks for ~3s, and report what the server actually negotiated: chunk
/// cadence (chunks/sec + mean chunk duration), delivery lag (mean + max — wall since the
/// first chunk minus `frames/48000`; the value that back-dates the relay's `w0` anchor
/// when the server buffers stale), the first-chunk arrival delay after start, and the
/// run's device latency. With the buffer-attr fix the lag stays near zero and chunks are
/// ~25ms; the pre-fix default ~2s record latency shows here as a ~1.9s lag.
fn capture_relay_test() {
    use std::time::{Duration, Instant};
    let Some((capture, rx)) = crate::audio::capture::MonitorCapture::start(None, None) else {
        eprintln!("capture-relay: monitor capture unavailable on this platform / no pulse server");
        return;
    };
    println!("capture-relay: consuming the default sink's monitor for ~3s…");
    let start = Instant::now();
    let deadline = start + Duration::from_secs(3);
    let mut chunks = 0u64;
    let mut frames_total = 0u64;
    let mut first_arrival: Option<Instant> = None;
    let mut last_arrival = start;
    let (mut lag_sum, mut lag_max, mut lag_n) = (0f64, f64::MIN, 0u64);
    while Instant::now() < deadline {
        let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        let now = Instant::now();
        let first = *first_arrival.get_or_insert(now);
        last_arrival = now;
        chunks += 1;
        frames_total += (chunk.samples.len() / 2) as u64;
        // Same lag the capture thread's backlog guard measures: how far the delivered
        // audio has fallen behind wall time since the first chunk.
        let lag = now.duration_since(first).as_secs_f64() - frames_total as f64 / 48000.0;
        lag_sum += lag;
        lag_max = lag_max.max(lag);
        lag_n += 1;
    }
    let stats = capture.stop();
    if chunks == 0 {
        println!("capture-relay: NO chunks delivered in 3s (suspended/virtual sink, or no server)");
        return;
    }
    let audio_secs = frames_total as f64 / 48000.0;
    let window_secs = last_arrival.duration_since(first_arrival.unwrap_or(start)).as_secs_f64();
    let mean_chunk_ms = audio_secs / chunks as f64 * 1000.0;
    let chunks_per_sec = if window_secs > 0.0 { chunks as f64 / window_secs } else { 0.0 };
    let first_delay_ms =
        first_arrival.map(|f| f.duration_since(start).as_secs_f64() * 1000.0).unwrap_or(0.0);
    println!("  chunks:            {chunks} ({:.1}/s over {window_secs:.2}s of arrivals)", chunks_per_sec);
    println!("  mean chunk:        {mean_chunk_ms:.1} ms ({audio_secs:.2}s audio total)");
    println!(
        "  delivery lag:      mean {:.1} ms, max {:.1} ms (peak in stats {:.1} ms)",
        lag_sum / lag_n as f64 * 1000.0,
        lag_max * 1000.0,
        stats.peak_lag_secs * 1000.0
    );
    println!("  first-chunk delay: {first_delay_ms:.1} ms after start");
    println!("  device latency:    {:.1} ms (dropped chunks: {})", stats.device_latency_ms, stats.dropped_chunks);
    println!(
        "  (healthy: ~25 ms chunks, lag near 0; a ~1.9s lag is the DRAGON-126 default-buffering bug)"
    );
}

/// `--test backend`: exercise the CaptureBackend seam end-to-end — print every
/// backend's capabilities, and for the capable ones prove the pixel methods
/// (outputs, a screenshot, the window list + one window grab, the cursor). The
/// trait's live consumer until capture dispatch moves behind it (DRAGON-93).
/// The portal probe needs the GUI runtime's session plumbing, so it reports as
/// unavailable here.
fn backend_test() {
    let ffmpeg = crate::encode::ffmpeg_available();
    for b in crate::platform::backend::backends(false, ffmpeg) {
        let c = b.caps();
        println!(
            "{}: screenshot={} record={} window_list={} window_capture={} cursor={} layer_overlay={} wallpaper={}",
            c.name,
            c.screenshot,
            c.record,
            c.window_list,
            c.window_capture,
            c.cursor_session,
            c.layer_overlay,
            c.wallpaper_path,
        );
        if c.screenshot {
            let outs = b.outputs();
            for o in &outs {
                println!(
                    "  output {} {}x{} at {},{}",
                    o.name, o.logical_size.0, o.logical_size.1, o.logical_pos.0, o.logical_pos.1
                );
            }
            if let Some(first) = outs.first() {
                match b.screenshot_output(&first.name) {
                    Some(img) => println!("  screenshot({}): {}x{}", first.name, img.width(), img.height()),
                    None => println!("  screenshot({}): FAILED", first.name),
                }
            }
        }
        if c.window_list {
            let wins = b.list_windows();
            println!("  windows: {}", wins.len());
            if c.window_capture
                && let Some(w) = wins.first()
            {
                match b.screenshot_window(&w.id) {
                    Some(img) => println!("  window({:?}): {}x{}", w.title, img.width(), img.height()),
                    None => println!("  window({:?}): FAILED", w.title),
                }
            }
        }
        if c.cursor_session {
            match b.cursor() {
                // `..` tolerates the macOS `CursorSprite`'s trailing sprite-scale
                // element (DRAGON-156); Linux's is a 3-tuple.
                Some((img, pos, hot, ..)) => println!(
                    "  cursor: {}x{} at {},{} hotspot {},{}",
                    img.width(), img.height(), pos.0, pos.1, hot.0, hot.1
                ),
                None => println!("  cursor: none (pointer off every monitor?)"),
            }
        }
    }
}

/// `--test mac-shot [display-name]`: prove the ScreenCaptureKit still path end to
/// end (DRAGON-94 phase 2). Enumerates displays, captures the named one (or the
/// first), writes a PNG to a temp file, and prints its dimensions. Also lists
/// windows + the cursor. Requires the Screen Recording TCC grant — if capture
/// returns empty, that's a permission state, not a code bug (the message says so).
#[cfg(target_os = "macos")]
fn mac_shot_test(name: &str) {
    let descs = crate::platform::mac::output_descs();
    if descs.is_empty() {
        eprintln!(
            "mac-shot: no displays returned. Either Screen Recording permission is not \
             granted (System Settings > Privacy & Security > Screen Recording: enable this \
             binary/terminal, then RESTART it), or SCK returned nothing."
        );
        return;
    }
    println!("displays ({}):", descs.len());
    for d in &descs {
        println!(
            "  {} {}x{} at {},{}",
            d.name, d.logical_size.0, d.logical_size.1, d.logical_pos.0, d.logical_pos.1
        );
    }
    let target = if name.is_empty() { descs[0].name.clone() } else { name.to_string() };
    match crate::screenshot::output(&target, None) {
        Some(img) => {
            let path = std::env::temp_dir().join("cosmic-capture-kit-mac-shot.png");
            match img.save(&path) {
                Ok(()) => println!(
                    "captured {}: {}x{} -> {}",
                    target,
                    img.width(),
                    img.height(),
                    path.display()
                ),
                Err(e) => eprintln!("mac-shot: capture ok ({}x{}) but save failed: {e}", img.width(), img.height()),
            }
        }
        None => eprintln!(
            "mac-shot: capture({target}) returned empty, likely because the Screen Recording TCC \
             grant is missing (grant + restart), not a code fault."
        ),
    }
    let wins = crate::platform::mac::list_windows();
    println!("windows: {}", wins.len());
    if let Some(w) = wins.first() {
        match crate::screenshot::window(&w.id, false) {
            Some(img) => println!("  window({:?}): {}x{}", w.title, img.width(), img.height()),
            None => println!("  window({:?}): FAILED", w.title),
        }
    }
    match crate::screenshot::capture_cursor() {
        // `..` tolerates the macOS `CursorSprite`'s trailing sprite-scale element.
        Some((img, pos, hot, ..)) => println!(
            "cursor: {}x{} at {},{} hotspot {},{}",
            img.width(), img.height(), pos.0, pos.1, hot.0, hot.1
        ),
        None => println!("cursor: none"),
    }
}

/// `--test mac-focus-shot [windowID]` (DRAGON-189, extended): the empirical proof for a
/// USER-PICKED window that was NOT frontmost at capture. Reproduces the failing case:
/// grab the target window as-is FIRST (it is not key, so its traffic lights are GRAY),
/// then run the real re-focus-then-grab seam (`capture_window_active`: AX-raise the exact
/// window + activate its app, WAIT until the OS confirms it frontmost, THEN grab) and grab
/// it AGAIN (now ACTIVE, colored). Scores the tight traffic-light region of both and writes
/// before/after PNGs. With no `windowID` arg, picks a titled window NOT owned by the
/// frontmost app (a different app's window — exactly the user's case). Requires the Screen
/// Recording grant; the AX-raise additionally requires the Accessibility grant (without it
/// the AFTER grab may stay gray and the run says so).
#[cfg(target_os = "macos")]
fn mac_focus_shot_test(arg0: &str) {
    use crate::platform::mac;
    const TL_W: u32 = 160;
    const TL_H: u32 = 60;

    // Warm up CoreGraphics (first bare-CLI SCK grab aborts otherwise).
    let _ = mac::output_descs();

    println!("Accessibility (AX) granted: {}", mac::focus::accessibility_granted());
    let front_pid = mac::frontmost_app_pid();
    println!("frontmost app pid: {front_pid:?}");

    // The target: an explicit windowID, else a titled window owned by a DIFFERENT app
    // than the frontmost one (the un-focused-pick case the fix targets).
    let target_id: Option<String> = if !arg0.is_empty() {
        Some(arg0.to_string())
    } else {
        mac::list_windows()
            .into_iter()
            .find(|t| {
                mac::window_owner_pid(&t.id).is_some_and(|pid| Some(pid) != front_pid)
            })
            .map(|t| t.id)
    };
    let Some(id) = target_id else {
        eprintln!(
            "mac-focus-shot: no window owned by a non-frontmost app found. Open a second \
             app's window (so a DIFFERENT app than the frontmost has a titled window) and \
             re-run, or pass an explicit windowID."
        );
        return;
    };
    let owner = mac::window_owner_pid(&id);
    println!("target window id: {id} (owner pid {owner:?})");

    // BEFORE: grab the target as-is (it is NOT key → gray traffic lights).
    let before = mac::capture_window(&id);
    let before_score = before.as_ref().map(|img| {
        let p = std::env::temp_dir().join("cck-focus-shot-before.png");
        let _ = img.save(&p);
        let s = mac::traffic_light_colorfulness(img, TL_W, TL_H);
        println!("BEFORE (no focus): colorfulness={s} -> {}", p.display());
        s
    });

    // AFTER: the real re-focus-then-grab seam.
    let after = mac::capture_window_active(&id);
    let after_score = after.as_ref().map(|img| {
        let p = std::env::temp_dir().join("cck-focus-shot-after.png");
        let _ = img.save(&p);
        let s = mac::traffic_light_colorfulness(img, TL_W, TL_H);
        println!("AFTER (focus+verify+grab): colorfulness={s} -> {}", p.display());
        s
    });

    match (before_score, after_score) {
        (Some(b), Some(a)) => {
            println!(
                "\nRESULT: before={b} ({}), after={a} ({})",
                if b < 10 { "GRAY" } else if b > 60 { "COLORED" } else { "ambiguous" },
                if a > 60 { "COLORED" } else if a < 10 { "GRAY" } else { "ambiguous" },
            );
            if b < 10 && a > 60 {
                println!("PROOF: the picked window went GRAY->COLORED via the focus step.");
            } else if a > 60 {
                println!("The after grab is COLORED (active) — the focus step worked.");
            } else {
                println!(
                    "The after grab is NOT colored. If Accessibility is not granted, grant it \
                     under System Settings > Privacy & Security > Accessibility and re-run."
                );
            }
        }
        _ => eprintln!("mac-focus-shot: a capture returned None (TCC missing?)."),
    }
}

/// `--test mac-wallpaper [display-name]` (DRAGON-291): the empirical proof for the
/// wallpaper backdrop behind a window captured from another AeroSpace workspace. For the
/// named display (or the main display if omitted) it runs BOTH backdrop sources the
/// `composite_over_wallpaper` ladder uses and reports which one yields a real wallpaper:
///
///   (a) the SCK `capture_wallpaper` grab: width/height, fraction opaque, fraction
///       near-black, per-channel min/max/mean, a coarse variance, and the composite
///       path's own black-vs-content VERDICT (`content` / `uniform-black` /
///       `transparent` / `empty`) — then saves the grab to a PNG.
///   (b) the FILE fallback: `wallpaper::detect()` -> `wallpaper_crop` to the display
///       size (the AeroSpace/SCK-independent path) — reports success + saves a PNG.
///
/// A UNIFORM near-black SCK grab (verdict `uniform-black`) is the DRAGON-291 failure the
/// fix now rejects so the composite falls through to (b). Requires the Screen Recording
/// grant for (a); (b) only needs the NSWorkspace desktop-picture URL.
#[cfg(target_os = "macos")]
fn mac_wallpaper_test(name: &str) {
    use image::RgbaImage;

    // Warm CoreGraphics (first bare-CLI SCK grab aborts otherwise) + enumerate displays.
    let descs = crate::platform::mac::output_descs();
    if descs.is_empty() {
        eprintln!(
            "mac-wallpaper: no displays returned. Screen Recording permission is likely not \
             granted (System Settings > Privacy & Security > Screen Recording: enable this \
             binary, then RESTART it)."
        );
        return;
    }
    println!("displays ({}):", descs.len());
    for d in &descs {
        println!(
            "  {} {}x{} at {},{}",
            d.name, d.logical_size.0, d.logical_size.1, d.logical_pos.0, d.logical_pos.1
        );
    }
    // Target: the requested display, else the primary (CGMainDisplayID), else the first.
    let main_name = format!("Display-{}", crate::platform::mac::main_display_id());
    let target = if !name.is_empty() {
        name.to_string()
    } else if descs.iter().any(|d| d.name == main_name) {
        main_name
    } else {
        descs[0].name.clone()
    };
    let (log_w, log_h) = descs
        .iter()
        .find(|d| d.name == target)
        .map(|d| d.logical_size)
        .unwrap_or((0, 0));
    println!("\ntarget display: {target} ({log_w}x{log_h} logical)");

    // Per-channel pixel stats over a full-frame scan (this is a diagnostic; cost is fine).
    fn stats(img: &RgbaImage) {
        let n = (img.width() as u64) * (img.height() as u64);
        if n == 0 {
            println!("  (empty image)");
            return;
        }
        let (mut opaque, mut near_black) = (0u64, 0u64);
        let mut lo = [255u8; 3];
        let mut hi = [0u8; 3];
        let mut sum = [0u64; 3];
        for p in img.pixels() {
            if p[3] != 0 {
                opaque += 1;
            }
            let max_c = p[0].max(p[1]).max(p[2]);
            if p[3] != 0 && max_c <= 8 {
                near_black += 1;
            }
            for c in 0..3 {
                lo[c] = lo[c].min(p[c]);
                hi[c] = hi[c].max(p[c]);
                sum[c] += p[c] as u64;
            }
        }
        let mean = [sum[0] / n, sum[1] / n, sum[2] / n];
        // Coarse variance proxy: the largest per-channel (max - min) spread.
        let spread = (0..3).map(|c| hi[c].saturating_sub(lo[c])).max().unwrap_or(0);
        println!(
            "  pixels={n} opaque={:.4} near_black={:.4}",
            opaque as f64 / n as f64,
            near_black as f64 / n as f64
        );
        println!("  channel min={lo:?} max={hi:?} mean={mean:?} spread(max-min)={spread}");
    }

    // (a) The SCK capture_wallpaper grab.
    println!("\n(a) SCK capture_wallpaper({target}):");
    match crate::platform::mac::capture_wallpaper(&target) {
        Some(wp) => {
            println!("  grab: {}x{}", wp.width(), wp.height());
            stats(&wp);
            let verdict = crate::screenshot::wallpaper_grab_verdict(&wp);
            println!("  composite verdict: {verdict}  (content => used as backdrop; anything else => file fallback)");
            let path = std::env::temp_dir().join("cosmic-capture-kit-wallpaper-sck.png");
            match wp.save(&path) {
                Ok(()) => println!("  saved -> {}", path.display()),
                Err(e) => eprintln!("  save failed: {e}"),
            }
        }
        None => println!("  capture_wallpaper returned None (Screen Recording grant missing?)"),
    }

    // (b) The FILE fallback: NSWorkspace desktop picture -> wallpaper_crop to display size.
    println!("\n(b) FILE fallback (wallpaper::detect -> wallpaper_crop):");
    match crate::wallpaper::detect() {
        Some(path) => {
            println!("  desktop-picture file: {}", path.display());
            let (pw, ph) = if log_w > 0 && log_h > 0 {
                (log_w as u32, log_h as u32)
            } else {
                (1920, 1080)
            };
            match crate::wallpaper::wallpaper_crop(&path, false, pw, ph, 0, 0, pw, ph) {
                Some(bg) => {
                    println!("  wallpaper_crop -> {}x{}", bg.width(), bg.height());
                    stats(&bg);
                    let verdict = crate::screenshot::wallpaper_grab_verdict(&bg);
                    println!("  (classifier on the FILE result: {verdict} — expect `content` for a real wallpaper)");
                    let out = std::env::temp_dir().join("cosmic-capture-kit-wallpaper-file.png");
                    match bg.save(&out) {
                        Ok(()) => println!("  saved -> {}", out.display()),
                        Err(e) => eprintln!("  save failed: {e}"),
                    }
                }
                None => println!(
                    "  wallpaper_crop returned None — the file could not be decoded/placed. \
                     If this happens the AeroSpace fix would NOT hold (no backdrop source)."
                ),
            }
        }
        None => println!(
            "  wallpaper::detect returned None — no decodable desktop-picture URL \
             (dynamic .heic / rotating folder / solid color resolve to None by design). \
             If this happens the AeroSpace fix would NOT hold."
        ),
    }

    // (c) The EXCLUDE-based SCK grab (DRAGON-186 Phase 5c) as a SECONDARY SCK backdrop
    //     source that does NOT depend on the wallpaper-window title allow-list matching,
    //     so it can recover a backdrop when (a) misses on an AeroSpace transition and (b)
    //     is unavailable. Verdict + PNG for comparison.
    println!("\n(c) EXCLUDE-based SCK grab (Phase 5c fallback source):");
    match crate::platform::mac::capture_wallpaper_excluding(&target) {
        Some(wp) => {
            println!("  grab: {}x{}", wp.width(), wp.height());
            stats(&wp);
            let verdict = crate::screenshot::wallpaper_grab_verdict(&wp);
            println!("  composite verdict: {verdict}");
            let out = std::env::temp_dir().join("cosmic-capture-kit-wallpaper-exclude.png");
            match wp.save(&out) {
                Ok(()) => println!("  saved -> {}", out.display()),
                Err(e) => eprintln!("  save failed: {e}"),
            }
        }
        None => println!("  capture_wallpaper_excluding returned None"),
    }
}

/// `--test mac-window-composite <window-id>` (DRAGON-268): the non-interactive reproduction
/// of the wallpaper-behind-window composite, so the crop/scale math can be inspected with
/// REAL numbers. Builds a `WindowCaptureJob` for the given window id with wallpaper-compose
/// ON and transparency ON, runs the real `run()`, and logs EVERY composite number: the
/// resolved display, the derived scale, out_w/out_h, the raw grab dims + backing scale, the
/// decorated (bordered+padded) dims used as the crop w/h, the crop rect (raw + nudged), and
/// whether the decorated window EXCEEDS the display (the old stretch trigger). It also saves
/// the composite PNG, a direct `capture_wallpaper` grab, and a direct footprint crop of the
/// SAME rect so the wallpaper strip can be compared against an undistorted reference.
///
/// Defaults for the job fields that a real capture would read from settings/overlay
/// (documented so the numbers are reproducible): wallpaper ON, transparency ON, shadow ON,
/// `pad_logical = 50` (the shipped default window padding), `window_radius = 12`, an Active
/// border resolved from the persisted appearance accent, `dark = true`. These mirror the
/// running app's window-capture defaults so the composite geometry matches a live capture.
#[cfg(target_os = "macos")]
fn mac_window_composite_test(id: &str) {
    use crate::platform::mac;
    if id.is_empty() {
        eprintln!("usage: --test mac-window-composite <window-id>  (ids from --test mac-list-windows)");
        return;
    }
    // Warm CoreGraphics (first bare-CLI SCK grab aborts otherwise) + enumerate displays.
    let descs = mac::output_descs();
    if descs.is_empty() {
        eprintln!(
            "mac-window-composite: no displays returned. Screen Recording permission is likely \
             not granted (grant this binary + RESTART)."
        );
        return;
    }
    let Some((wx, wy, ww, wh)) = mac::window_logical_rect(id) else {
        eprintln!("mac-window-composite: window id {id} not found (frame lookup failed)");
        return;
    };
    println!("window {id}: logical rect origin=({wx},{wy}) size=({ww}x{wh})");
    let sel = crate::selection::Selection {
        x: wx,
        y: wy,
        width: ww.max(1) as u32,
        height: wh.max(1) as u32,
        output: None,
        window_id: Some(id.to_string()),
    };

    // ── Reproduce run()'s decoration math to LOG the geometry (kept in lock-step with
    //    WindowCaptureJob::run + composite_over_wallpaper). ──────────────────────────
    let Some(raw) = crate::screenshot::window(id, false) else {
        eprintln!("mac-window-composite: raw window grab returned None (grant missing?)");
        return;
    };
    let scale = raw.width() as f32 / sel.width.max(1) as f32;
    println!(
        "raw window grab: {}x{}  derived backing scale = {} / {} = {scale}",
        raw.width(),
        raw.height(),
        raw.width(),
        sel.width
    );

    // The shipped window-capture defaults (documented above).
    let pad_logical: f32 = 50.0;
    let window_radius: f32 = 12.0;
    let dark = true;
    let active_default = crate::app::theme::resolved_appearance_accent_rgba();
    let borders =
        crate::decoration::WindowBorders::resolve(Some(active_default), 3, [65, 69, 80, 255], 1);
    let border = borders.active;
    let bw_logical = border.to_compose().map(|(w, _)| w).unwrap_or(0.0);
    let margin_logical = (pad_logical - bw_logical).max(0.0);
    let total_margin_logical = bw_logical + margin_logical;
    // Physical dims of the decorated window = window + 2*border + 2*margin (shadow uses the
    // SAME 2*margin canvas as pad_transparent), i.e. rw/rh the crop is taken at.
    let bw_px = (bw_logical * scale).round().max(1.0) as i32;
    let margin_px = (margin_logical * scale).round() as i32;
    let decorated_w = raw.width() as i32 + 2 * bw_px + 2 * margin_px;
    let decorated_h = raw.height() as i32 + 2 * bw_px + 2 * margin_px;
    println!(
        "decoration: pad_logical={pad_logical} border_logical={bw_logical} margin_logical={margin_logical} \
         total_margin_logical={total_margin_logical}"
    );
    println!(
        "decoration (physical): border={bw_px}px margin={margin_px}px -> decorated (crop rw x rh) = {decorated_w}x{decorated_h}"
    );

    // Resolve the display the window centre sits on (the composite's own containment test).
    let cx = sel.x + sel.width as i32 / 2;
    let cy = sel.y + sel.height as i32 / 2;
    let resolved = descs
        .iter()
        .find(|d| {
            let ((ox, oy), (ow, oh)) = (d.logical_pos, d.logical_size);
            cx >= ox && cx < ox + ow && cy >= oy && cy < oy + oh
        })
        .or_else(|| descs.first());
    let Some(d) = resolved else {
        eprintln!("mac-window-composite: could not resolve a display for the window centre");
        return;
    };
    let display = (d.name.clone(), d.logical_pos, d.logical_size);
    println!(
        "resolved display: {} logical pos=({},{}) size=({}x{})",
        d.name, d.logical_pos.0, d.logical_pos.1, d.logical_size.0, d.logical_size.1
    );
    let g = crate::screenshot::wallpaper_composite_geom(
        display.clone(),
        sel.x,
        sel.y,
        total_margin_logical,
        scale,
        decorated_w,
        decorated_h,
    );
    println!("── composite geometry ──");
    println!("  scale={}", g.scale);
    println!("  out_w x out_h = {} x {}", g.out_w, g.out_h);
    println!("  bnd (border_logical rounded) = {}px", g.bnd_physical);
    println!("  rw x rh (decorated crop) = {} x {}", g.rw, g.rh);
    println!("  rx_raw,ry_raw = {},{}   rx,ry (nudged) = {},{}", g.rx_raw, g.ry_raw, g.rx, g.ry);
    println!(
        "  OVERSIZE (decorated exceeds display): {}  ({}x{} vs {}x{})",
        g.oversize, g.rw, g.rh, g.out_w, g.out_h
    );

    // Save a direct wallpaper grab + the direct footprint crop of the SAME rect (the
    // undistorted reference the composite's wallpaper strip must match).
    match mac::capture_wallpaper(&d.name) {
        Some(wp) => {
            let p = std::env::temp_dir().join("cck-window-composite-wallpaper.png");
            let _ = wp.save(&p);
            println!("  capture_wallpaper: {}x{} -> {}", wp.width(), wp.height(), p.display());
            if let Some(strip) = crate::screenshot::wallpaper_footprint_ref(
                &wp, g.out_w, g.out_h, g.rx, g.ry, g.rw as u32, g.rh as u32,
            ) {
                let sp = std::env::temp_dir().join("cck-window-composite-wp-footprint.png");
                let _ = strip.save(&sp);
                println!(
                    "  direct footprint crop (undistorted reference): {}x{} -> {}",
                    strip.width(), strip.height(), sp.display()
                );
            }
        }
        None => println!("  capture_wallpaper returned None (grant missing?)"),
    }

    // ── Run the REAL composite through the job. ──────────────────────────────────────
    let frozen_geom: Vec<_> = descs
        .iter()
        .map(|o| (o.name.clone(), o.logical_pos, o.logical_size))
        .collect();
    let job = crate::screenshot::WindowCaptureJob {
        id: id.to_string(),
        cursor: false,
        sel,
        capture_transparency: true,
        capture_wallpaper: true,
        window_radius,
        border,
        window_shadow: true,
        pad_logical,
        dark,
        frozen_geom,
        frozen_px: None,
        cursor_overlay: None,
        // DRAGON-292: this diagnostic reproduces the ACTIVE-appearance composite.
        backdrop_active: true,
    };
    match job.run() {
        Some(img) => {
            let p = std::env::temp_dir().join(format!("cck-window-composite-{id}.png"));
            match img.save(&p) {
                Ok(()) => println!(
                    "composite output: {}x{} -> {}",
                    img.width(),
                    img.height(),
                    p.display()
                ),
                Err(e) => eprintln!("mac-window-composite: save failed: {e}"),
            }
        }
        None => eprintln!("mac-window-composite({id}): run() returned None (grab failed)"),
    }
}

/// `--test mac-active-shot` (DRAGON-189): the empirical traffic-light proof. Grabs the
/// window of the app that is TRULY frontmost RIGHT NOW (NSWorkspace.frontmostApplication,
/// the same notion the OS uses to color a window's traffic lights) and scores ONLY the
/// tight top-left traffic-light region. A colored score (>60) means the capture caught the
/// ACTIVE (colored-buttons) appearance; a gray score (<10) means the target was rendered
/// inactive. Writes the PNG to the temp dir for eyeball confirmation. Requires the Screen
/// Recording TCC grant. Run it with a NORMAL app frontmost (activate it first, e.g.
/// `osascript -e 'tell application "Finder" to activate'`) so its traffic lights are lit.
#[cfg(target_os = "macos")]
fn mac_active_shot_test() {
    use crate::platform::mac;
    // TIGHT traffic-light box: the buttons sit in the top-left ~78x28 LOGICAL px, so on a
    // 2x retina window they are within ~160x60 PHYSICAL px. A box this size scores ONLY the
    // buttons, never colored app chrome (a logo, an avatar, a red badge) elsewhere in the
    // title bar — the mistake a 400x160 box makes. `traffic_light_colorfulness` clamps to
    // the image, so a 1x window (smaller physical box) still works.
    const TL_W: u32 = 160;
    const TL_H: u32 = 60;

    // Warm up CoreGraphics: the first SCK display/window grab from a bare CLI process
    // aborts in `CGS_REQUIRE_INIT` unless a display-level SCK call ran first.
    let _ = mac::output_descs();

    // The app the OS considers frontmost — the ONLY app whose windows get colored traffic
    // lights. `list_windows().find(active)` uses SCK's `isActive`, which flags a window as
    // active even when a DIFFERENT app (e.g. the terminal running this CLI) is truly
    // frontmost, so its buttons are actually gray. We want the genuinely-front app instead.
    let front_pid = {
        let ws = objc2_app_kit::NSWorkspace::sharedWorkspace();
        ws.frontmostApplication().map(|a| a.processIdentifier())
    };
    println!("frontmost app pid: {front_pid:?}");

    // The frontmost app's on-screen, titled, layer-0 window (its front window). Fall back
    // to SCK's `active` window if we can't resolve the pid's window.
    let wins = mac::list_windows();
    let target = front_pid
        .and_then(|pid| mac::list_windows_owned_by(pid).into_iter().next())
        .or_else(|| wins.iter().find(|t| t.active).cloned());
    let Some(target) = target else {
        eprintln!(
            "mac-active-shot: no capturable window for the frontmost app. Activate a normal \
             app window (Finder, a browser) and re-run."
        );
        return;
    };
    println!("target window: {:?} (id {})", target.title, target.id);

    let Some(img) = mac::capture_window(&target.id) else {
        eprintln!("mac-active-shot: capture_window returned None (TCC missing?)");
        return;
    };
    let p = std::env::temp_dir().join("cck-active-shot.png");
    let _ = img.save(&p);
    let score = mac::traffic_light_colorfulness(&img, TL_W, TL_H);
    println!(
        "traffic-light colorfulness = {score}  ({}x{}) -> {}",
        img.width(),
        img.height(),
        p.display()
    );
    println!(
        "  ({})",
        if score > 60 {
            "COLORED traffic lights: the ACTIVE appearance was captured"
        } else if score < 10 {
            "GRAY traffic lights: the INACTIVE appearance was captured (the DRAGON-189 bug)"
        } else {
            "ambiguous"
        }
    );
}

/// `--test mac-grab-id <windowID> [out.png]` (DRAGON-189): grab ONE specific window by its
/// `windowID` right now (whatever the current frontmost app is) and score its traffic-light
/// region. Used to prove a target window renders GRAY when it is NOT the frontmost/key
/// window (activate a DIFFERENT windowed app first), and COLORED when it is.
#[cfg(target_os = "macos")]
fn mac_grab_id_test(id: &str, out: &str) {
    use crate::platform::mac;
    if id.is_empty() {
        eprintln!("mac-grab-id: usage: --test mac-grab-id <windowID> [out.png]");
        return;
    }
    let _ = mac::output_descs(); // CG warmup
    println!(
        "frontmost app: {:?}",
        objc2_app_kit::NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .and_then(|a| a.localizedName().map(|n| n.to_string()))
    );
    let Some(img) = mac::capture_window(id) else {
        eprintln!("mac-grab-id: capture_window({id}) returned None (bad id or TCC missing)");
        return;
    };
    let score = mac::traffic_light_colorfulness(&img, 160, 60);
    let path = if out.is_empty() {
        std::env::temp_dir().join(format!("cck-grab-{id}.png"))
    } else {
        std::path::PathBuf::from(out)
    };
    let _ = img.save(&path);
    println!(
        "window {id}: traffic_light_colorfulness = {score} ({}) {}x{} -> {}",
        if score > 60 { "COLORED" } else if score < 10 { "GRAY" } else { "ambiguous" },
        img.width(),
        img.height(),
        path.display()
    );
}

/// `--test mac-daemon-repro <AppName> [scratch_dir]` (DRAGON-189): the empirical proof of
/// the RESIDENT daemon bug AND its fix. Reproduces the real daemon->child focus churn in one
/// observable process:
///
///   1. Activate `<AppName>` so it is genuinely frontmost (the "user is working there" state
///      at hotkey time). Records the frontmost pid + its front window id.
///   2. DAEMON-TIME grab: capture that front window NOW (target still frontmost) and score
///      the traffic-light region. This is where the DRAGON-189 fix grabs — expect COLORED.
///   3. Save the daemon grab to the handoff temp PNG and simulate the CHILD receiving it:
///      load it back via the env handoff and re-score — proves the pixels survive the trip.
///   4. CHILD-TIME grab: activate OURSELVES (mimicking the spawned child stealing frontmost
///      as it boots), wait a child-boot-sized delay, then re-grab the SAME window id and
///      score it. This is the pre-fix path the child took — expect GRAY.
///
/// Prints all three scores + PNG paths so the before/after is eyeball- and number-verifiable.
/// Writes PNGs to `scratch_dir` (default: the temp dir). Requires Screen Recording TCC.
#[cfg(target_os = "macos")]
fn mac_daemon_repro_test(app: &str, scratch: &str) {
    use crate::platform::mac::{self, active_window};
    if app.is_empty() {
        eprintln!("mac-daemon-repro: usage: --test mac-daemon-repro <AppName> [scratch_dir]");
        return;
    }
    let dir = if scratch.is_empty() {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from(scratch)
    };
    let _ = std::fs::create_dir_all(&dir);
    const TL_W: u32 = 160;
    const TL_H: u32 = 60;
    let score_of = |img: &image::RgbaImage| mac::traffic_light_colorfulness(img, TL_W, TL_H);
    let verdict = |s: u8| if s > 60 { "COLORED" } else if s < 10 { "GRAY" } else { "ambiguous" };

    // Warm up CoreGraphics (first bare-CLI SCK grab aborts otherwise).
    let _ = mac::output_descs();

    // 1. Make the target genuinely frontmost (the daemon's world at hotkey time).
    let _ = std::process::Command::new("osascript")
        .args(["-e", &format!("tell application \"{app}\" to activate")])
        .status();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let front_pid = {
        let ws = objc2_app_kit::NSWorkspace::sharedWorkspace();
        ws.frontmostApplication()
            .map(|a| (a.processIdentifier(), a.localizedName().map(|n| n.to_string())))
    };
    println!("frontmost after activate {app:?}: {front_pid:?}");
    let Some((pid, _)) = front_pid else {
        eprintln!("mac-daemon-repro: no frontmost app; is {app:?} installed/running?");
        return;
    };
    let Some(front) = mac::list_windows_owned_by(pid).into_iter().next() else {
        eprintln!("mac-daemon-repro: {app:?} has no capturable front window (a titled, on-screen, layer-0 window). Open one and re-run.");
        return;
    };
    println!("target front window: {:?} (id {})", front.title, front.id);

    // 2. DAEMON-TIME grab (target still frontmost) — the fix's grab point.
    let Some(daemon_img) = mac::capture_window(&front.id) else {
        eprintln!("mac-daemon-repro: daemon-time capture_window returned None (TCC?)");
        return;
    };
    let daemon_score = score_of(&daemon_img);
    let daemon_png = dir.join("dragon189-daemon-time.png");
    let _ = daemon_img.save(&daemon_png);
    println!(
        "DAEMON-TIME  score = {daemon_score:>3}  ({}) -> {}",
        verdict(daemon_score),
        daemon_png.display()
    );

    // 3. Handoff round-trip: save to the real handoff path, set the env, load it back the
    //    way the child does, and re-score (proves the pixels survive path+env+decode).
    let handoff_png = active_window::temp_png_path(42);
    let _ = daemon_img.save(&handoff_png);
    unsafe {
        std::env::set_var(active_window::ENV_PNG, &handoff_png);
        std::env::set_var(active_window::ENV_ID, &front.id);
    }
    match active_window::load_from_env() {
        Some((id, img)) => {
            let s = score_of(&img);
            println!(
                "HANDOFF      score = {s:>3}  ({}) id={id}  [round-tripped through env + temp PNG]",
                verdict(s)
            );
        }
        None => println!("HANDOFF      FAILED to load back (the env handoff is broken)"),
    }

    // 4. CHILD-TIME grab: steal frontmost the way the spawned child does as it boots, wait a
    //    child-boot-sized delay, then re-grab the SAME window id. The pre-fix path.
    let me = objc2_app_kit::NSRunningApplication::currentApplication();
    #[allow(deprecated)]
    me.activateWithOptions(
        objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
    );
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let child_score = match mac::capture_window(&front.id) {
        Some(child_img) => {
            let s = score_of(&child_img);
            let child_png = dir.join("dragon189-child-time.png");
            let _ = child_img.save(&child_png);
            println!(
                "CHILD-TIME   score = {s:>3}  ({}) -> {}",
                verdict(s),
                child_png.display()
            );
            s
        }
        None => {
            println!("CHILD-TIME   capture_window returned None");
            0
        }
    };

    println!("\nsummary:");
    println!("  daemon-time (the fix's grab point) : {daemon_score} ({})", verdict(daemon_score));
    println!("  child-time  (the pre-fix bug path) : {child_score} ({})", verdict(child_score));
    if daemon_score > 60 && child_score < 10 {
        println!("  => REPRODUCED: daemon-time is COLORED, child-time is GRAY. The DRAGON-189 handoff fixes it.");
    } else if daemon_score > 60 && child_score > 60 {
        println!("  => both colored on this machine (no focus churn reproduced); the fix is still correct + harmless.");
    } else {
        println!("  => inconclusive on this run (target may not have had lit traffic lights; try a normal windowed app).");
    }
}

/// `--test mac-rec-bench [Display-<id>|largest] [secs] [encoder] [fps] [maxside]`: drive a
/// REAL recording of a whole display through the production SCK media-clock pipeline
/// (`start_region_recording`), while forcing on-screen motion, then measure the ACHIEVED
/// distinct-frame rate of the output — the honest full-pipeline number the DRAGON-163
/// encoder-only bench cannot give (capture + any downscale + pipe + encode all counted).
///
/// Args (all optional):
///   target    `Display-<id>` or `largest` (default: the largest connected display)
///   secs      recording duration in seconds (default 6)
///   encoder   `software` | `videotoolbox` | `auto` (default `software`)
///   fps       configured frame rate (default 60)
///   maxside   a max-resolution box side to honor (0 = no user cap; default 0)
///
/// Prints the resolved encode dims, the container fps/duration, and the distinct-content
/// frame rate (via `mpdecimate`, which drops near-duplicate frames — so distinct/duration
/// is the true motion throughput). Requires the Screen Recording TCC grant + ffmpeg. It
/// spawns an on-screen motion source on the target and KILLS it before returning.
#[cfg(target_os = "macos")]
fn mac_rec_bench(rest: &[String]) {
    use std::time::{Duration, Instant};
    let arg = |n: usize| rest.get(n).map(String::as_str).unwrap_or("");

    // Resolve the target display (largest by pixel area unless a Display-<id> is named).
    let descs = crate::platform::mac::output_descs();
    if descs.is_empty() {
        eprintln!("mac-rec-bench: no displays (Screen Recording permission missing? grant + restart)");
        return;
    }
    let want = arg(0);
    let target_desc = if want.is_empty() || want == "largest" {
        descs
            .iter()
            .max_by_key(|d| (d.logical_size.0 as i64) * (d.logical_size.1 as i64))
            .cloned()
    } else {
        descs.iter().find(|d| d.name == want).cloned()
    };
    let Some(td) = target_desc else {
        eprintln!("mac-rec-bench: no display named {want:?}; have: {:?}",
            descs.iter().map(|d| &d.name).collect::<Vec<_>>());
        return;
    };
    let secs: u64 = arg(1).parse().unwrap_or(6);
    let encoder = if arg(2).is_empty() { "software" } else { arg(2) };
    let fps: u32 = arg(3).parse().unwrap_or(60);
    let maxside: u32 = arg(4).parse().unwrap_or(0);

    // The display's global TOP-LEFT origin + logical size, so the motion source can be
    // spawned centered on it (ffplay places windows by the primary display; we just need
    // it visible somewhere on the target — a fullscreen lavfi source on the main display
    // still forces global motion the capture sees when the target is the main display, and
    // for a secondary display we position it into the target's rect).
    let (ox, oy) = td.logical_pos;
    let (lw, lh) = td.logical_size;
    println!(
        "mac-rec-bench: target {} {}x{} (logical) at {},{}  encoder={encoder} fps={fps} secs={secs} maxside={maxside}",
        td.name, lw, lh, ox, oy
    );

    // Spawn a motion source onto the target display, sized to cover most of it so the
    // captured frame changes substantially every tick (a small corner window shrinks to a
    // handful of pixels after the downscale and mpdecimate rightly reads near-static
    // frames as duplicates — an unrealistic torture test vs. real full-screen playback/
    // scrolling). `testsrc2` animates every frame. `ffplay` is a dev-only PATH tool (not
    // vendored) — bare name, resolved by the OS. `-left/-top` place the window in the
    // target's rect (global coords); the source resolution matches the window so the whole
    // window animates.
    let (win_w, win_h) = ((lw * 9 / 10).max(640), (lh * 9 / 10).max(360));
    let motion = match std::process::Command::new("ffplay")
        .args(["-loglevel", "quiet", "-noborder"])
        .args(["-x", &win_w.to_string(), "-y", &win_h.to_string()])
        .args(["-left", &(ox + 20).to_string(), "-top", &(oy + 20).to_string()])
        .args(["-f", "lavfi", "-i", &format!("testsrc2=size={win_w}x{win_h}:rate={fps}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mac-rec-bench: could not spawn ffplay motion source ({e}); \
                the achieved-fps number would be meaningless without motion — aborting");
            return;
        }
    };
    // Give ffplay a beat to appear and start animating.
    std::thread::sleep(Duration::from_millis(800));

    // Kill-the-motion guard: whatever happens below, ffplay must not leak (a stray ffplay
    // window has annoyed the user before). Runs on every return path.
    struct MotionGuard(std::process::Child);
    impl Drop for MotionGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _guard = MotionGuard(motion);

    let out = std::env::temp_dir().join(format!("cck-rec-bench-{}.mkv", std::process::id()));
    let _ = std::fs::remove_file(&out);

    let settings = crate::record::RecordSettings {
        fps,
        preferred_encoder: encoder.to_string(),
        presets: crate::encode::Presets::default(),
        zero_copy: false,
        mic: false,
        system_audio: false,
        bitrate_kbps: 8000,
        audio_offset_ms: 0,
        auto_device_compensation: false,
        max_res: (maxside, maxside),
        metadata: String::new(),
        out_path: out.clone(),
    };
    let params = crate::record::RegionRecordParams {
        x: ox,
        y: oy,
        w: lw as u32,
        h: lh as u32,
        cursor: false,
        mac_target: crate::record::MacRecordTarget::Display(td.name.clone()),
        settings,
    };
    println!("mac-rec-bench: recording {secs}s through the production SCK pipeline…");
    let handle = crate::record::start_region_recording(params);
    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(secs));
    handle.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    // Wait (bounded) for the worker to finalize.
    let deadline = Instant::now() + Duration::from_secs(60);
    let result = loop {
        if let Ok(g) = handle.done.lock()
            && let Some(r) = g.as_ref()
        {
            break r.clone();
        }
        if Instant::now() > deadline {
            break Err("timed out waiting for the recording to finalize".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let wall = started.elapsed().as_secs_f64();
    let captured_dims = handle.dims.lock().ok().and_then(|g| *g);

    match result {
        Ok(path) => {
            println!("mac-rec-bench: recording finished -> {}", path.display());
            if let Some((cw, ch)) = captured_dims {
                println!("  captured footprint (pre-cap): {cw}x{ch}");
            }
            report_achieved_fps(&path, fps, wall, secs as f64);
            if std::env::var_os("CCK_KEEP").is_some() {
                println!("  (kept: {})", path.display());
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
        Err(e) => eprintln!("mac-rec-bench: recording FAILED: {e}"),
    }
    // `_guard` drops here, killing ffplay.
}

/// Probe an output recording for its encode dims, container duration/fps, and the
/// ACHIEVED distinct-content frame rate (frames whose content differs from the previous
/// frame — real motion, past the CFR re-feed) over the container duration. This is the
/// number that must approach the configured `fps` for a healthy recording (DRAGON-168).
#[cfg(target_os = "macos")]
fn report_achieved_fps(path: &std::path::Path, configured_fps: u32, wall_secs: f64, requested_secs: f64) {
    // Encode dims + duration + nominal fps from ffprobe.
    let probe = std::process::Command::new(crate::util::ffprobe_path())
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries",
               "stream=width,height,avg_frame_rate,nb_read_frames:format=duration",
               "-count_frames", "-of", "default=noprint_wrappers=1", ])
        .arg(path)
        .output();
    let (mut w, mut h, mut dur, mut nb) = (0u32, 0u32, 0f64, 0u64);
    if let Ok(o) = probe {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            if let Some(v) = line.strip_prefix("width=") { w = v.trim().parse().unwrap_or(0); }
            if let Some(v) = line.strip_prefix("height=") { h = v.trim().parse().unwrap_or(0); }
            if let Some(v) = line.strip_prefix("duration=") { dur = v.trim().parse().unwrap_or(0.0); }
            if let Some(v) = line.strip_prefix("nb_read_frames=") { nb = v.trim().parse().unwrap_or(0); }
        }
    }
    let (distinct, total) = distinct_frame_count(path);
    let dur = if dur > 0.0 { dur } else { wall_secs };
    // The decoded frame count is the CFR total (container re-feeds hold the rate); prefer
    // it over `nb_read_frames` when the decode counted more.
    let nb = total.max(nb);
    let nominal = if dur > 0.0 { nb as f64 / dur } else { 0.0 };
    let achieved = if dur > 0.0 { distinct as f64 / dur } else { 0.0 };
    let pct = if configured_fps > 0 { 100.0 * achieved / configured_fps as f64 } else { 0.0 };
    let dur_pct = if requested_secs > 0.0 { 100.0 * dur / requested_secs } else { 0.0 };
    println!("  encode dims:        {w}x{h}");
    println!(
        "  duration:           {dur:.2}s of {requested_secs:.0}s requested = {dur_pct:.0}% (wall {wall_secs:.2}s)  {}",
        if dur_pct >= 90.0 { "OK" } else { "TRUNCATED (backlog stole capture time)" }
    );
    println!("  container frames:   {nb} ({nominal:.1} fps nominal — CFR re-fed)");
    println!("  DISTINCT frames:    {distinct} ({achieved:.1} distinct fps)");
    println!(
        "  achieved / config:  {achieved:.1} / {configured_fps} = {pct:.0}%   {}",
        if pct >= 70.0 { "OK (>=70%)" } else { "LOW (<70% — throughput bottleneck)" }
    );
}

/// Count `(distinct, total)` decoded frames of a recording. `distinct` is the number of
/// frames whose content DIFFERS from the immediately preceding frame — the CFR re-feed
/// (DRAGON-125) writes a copy of the last frame on ticks where nothing new arrived, so a
/// run of identical consecutive frames counts once. Uses ffmpeg's `framemd5` over a small
/// grayscale downscale (robust, exact, threshold-free — unlike `mpdecimate`, whose block
/// heuristic reads a downscaled synthetic pattern's small per-frame deltas as duplicates
/// and undercounts wildly). `distinct/duration` is the true achieved motion frame rate.
#[cfg(target_os = "macos")]
fn distinct_frame_count(path: &std::path::Path) -> (u64, u64) {
    let out = std::process::Command::new(crate::util::ffmpeg_path())
        .args(["-loglevel", "error", "-i"])
        .arg(path)
        // A tiny grayscale downscale makes the per-frame hash cheap while still capturing
        // any real content change; identical frames hash identically.
        .args(["-vf", "scale=160:90,format=gray", "-f", "framemd5", "-"])
        .output();
    let Ok(out) = out else { return (0, 0) };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut total = 0u64;
    let mut distinct = 0u64;
    let mut prev: Option<String> = None;
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        // framemd5 line: "<stream>, <pts>, <dts>, <duration>, <size>, <md5hash>"
        let Some(hash) = line.rsplit(',').next().map(|s| s.trim().to_string()) else { continue };
        total += 1;
        if prev.as_deref() != Some(hash.as_str()) {
            distinct += 1;
        }
        prev = Some(hash);
    }
    (distinct, total)
}

/// `--test cursor-capture`: grab the mouse cursor via the ext-image-copy-capture cursor session,
/// save the sprite (real alpha) to a PNG, and print the path + dimensions + position + hotspot.
/// Verifies the clean cursor-capture path before building the cursor-over-transparent feature.
fn cursor_capture_test() {
    // Print the enumerated outputs (name + GLOBAL LOGICAL geometry) so the resolved cursor
    // position can be eyeballed against the multi-monitor layout — DRAGON-213: the position
    // is now a GLOBAL LOGICAL coordinate (the Position event's transformed BUFFER pixels
    // divided by the output's buffer scale), so it must land inside exactly one of these
    // rects, at the visible pointer's spot, on any scale (1x / fractional / mixed).
    let descs = crate::screenshot::output_descs();
    println!("outputs ({}):", descs.len());
    for d in &descs {
        println!(
            "  {} logical {},{} {}x{}",
            d.name, d.logical_pos.0, d.logical_pos.1, d.logical_size.0, d.logical_size.1
        );
    }
    match crate::screenshot::capture_cursor() {
        // `..` tolerates the macOS `CursorSprite`'s trailing sprite-scale element.
        Some((img, pos, hotspot, ..)) => {
            let path = std::env::temp_dir().join("cosmic-capture-kit-cursor.png");
            match img.save(&path) {
                Ok(()) => println!("saved: {}", path.display()),
                Err(e) => eprintln!("cursor-capture: failed to save {}: {e}", path.display()),
            }
            println!("size: {}x{}", img.width(), img.height());
            println!("position (global logical): {},{}", pos.0, pos.1);
            println!("hotspot (sprite px): {},{}", hotspot.0, hotspot.1);
            // Which output contains the resolved position (the correctness check).
            match descs.iter().find(|d| {
                let (ox, oy) = d.logical_pos;
                let (ow, oh) = d.logical_size;
                pos.0 >= ox && pos.0 < ox + ow && pos.1 >= oy && pos.1 < oy + oh
            }) {
                Some(d) => println!("resolved onto output: {}", d.name),
                None => println!("resolved onto output: NONE (position outside every output!)"),
            }
        }
        None => eprintln!("cursor-capture: no cursor captured (is the pointer on a monitor?)"),
    }
}

/// List the available `--test` subcommands.
fn print_test_help() {
    eprint!(
        "usage: cosmic-capture-kit --test <name> [args]\n\n\
         selftest                          gather outputs + screencopy probe\n\
         audio                             mic input-processing chain on the live mic\n\
         mic-rec                           record cleaned mic + system to /tmp/cck-rectest.mp4\n\
         scan <image>                      run code + text detection on an image\n\
         ocr-bench <dir>                   OCR similarity over a labelled corpus\n\
         denoise <in.raw> [out-prefix]     A/B the denoisers on f32le mono 48k audio\n\
         monitor-latency                   probe the default sink's signed device latency (DRAGON-119)\n\
         capture-relay                     probe monitor-capture chunk cadence + delivery lag (DRAGON-126)\n\
         bench-capture                     time a window capture\n\
         cursor-capture                    grab the mouse cursor sprite (alpha + position + hotspot)\n\
         bench-encoders [w h [capture]]    fps + CPU cores per encoder (default 4K; `capture` adds the capture-thread cost)\n\
         bench-record [secs] [fps] [back]  end-to-end record benchmark\n\
         pw [window]                       PipeWire screencast probe (monitor|window)\n"
    );
    #[cfg(target_os = "linux")]
    eprintln!(
        "linux-focus-probe [id|title]      activate a toplevel + re-grab; before/after CSD focus proof (DRAGON-194)\n\
         glass-shot [id|title]             window capture with frosted-glass compositing on vs off -> PNG pair (DRAGON-218)"
    );
    #[cfg(feature = "zero-copy")]
    eprint!(
        "dmabuf [window]                   zero-copy dmabuf capture probe\n\
         screencopy-dmabuf                 wlr-screencopy dmabuf probe\n"
    );
    #[cfg(target_os = "macos")]
    eprint!(
        "mac-shot [display-name]           SCK still + window/cursor probe -> PNG in tmp\n\
         mac-active-shot                   grab the frontmost app's window + score its traffic lights (DRAGON-189)\n\
         mac-focus-shot [windowID]         focus a non-front window, verify, re-grab; before/after traffic-light score (DRAGON-189)\n\
         mac-wallpaper [display-name]      SCK wallpaper grab + FILE fallback: pixel stats, black-vs-content verdict -> PNG pair (DRAGON-291)\n"
    );
    // DRAGON-229/241: the Windows capture + recording checkpoints (W1-SPEC §7 / W3-SPEC §8),
    // one line per `windows-*` match arm above.
    #[cfg(target_os = "windows")]
    eprint!(
        "windows-monitors                  DPI awareness + each monitor's geometry + DPI\n\
         windows-shot-output [name]        a full-monitor BitBlt flat -> PNG\n\
         windows-list                      the toplevel window list (id, rect, active, title)\n\
         windows-shot-window <hwnd>        one window's pixels (PrintWindow ladder) -> PNG\n\
         windows-thumbs                    time the FULL bounded window pre-capture (picker grab); reports skips\n\
         windows-cursor                    the cursor sprite (alpha + position + hotspot + scale) -> PNG\n\
         windows-wallpaper [name]          detect() + the per-output wallpaper resolver -> PNG\n\
         windows-window-composite <hwnd> [black]  single-window-on-wallpaper composite -> PNG\n\
         windows-shot-region <x> <y> <w> <h>  a stitched region flat -> PNG\n\
         windows-wgc-frame <monitor|window|region> [args]  grab one WGC frame -> PNG\n\
         windows-record <secs> [mic] [nosys] [pause] [enc]  record via the owned media-clock pipeline -> mp4\n\
         windows-audio-endpoints           list the ACTIVE WASAPI render endpoints (id + name) the Output picker offers\n"
    );
}

/// Run code + text detection on an image and print the results (`--test scan <image>`).
fn scan_test(path: &str) {
    let path = std::path::Path::new(path);
    let Ok(img) = image::open(path) else {
        eprintln!("scan: could not open {}", path.display());
        return;
    };
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    for m in crate::detect::scan_codes(&rgba, 0, 0, w, h) {
        eprintln!(
            "Code rect={:?} poly={:?} action={:?} label={:?}",
            m.rect, m.poly, m.action, m.label
        );
    }
    let conf = std::env::var("CCK_CONF").ok().and_then(|s| s.parse().ok()).unwrap_or(25.0);
    let words = crate::detect::scan_text(&rgba, 0, 0, w, h, conf);
    for word in &words {
        eprintln!(
            "Word line={} rect={:?} poly={:?} text={:?}",
            word.line, word.rect, word.poly, word.text
        );
    }
    if std::env::var_os("CCK_JOIN").is_some() {
        eprintln!("\n--- joined ---\n{}", crate::detect::join_words(&words));
    }
}

/// OCR regression bench: run the text scanner over a corpus of `(image, expected
/// text)` cases and report a per-case + overall similarity, so OCR tuning can be
/// measured instead of eyeballed. Each case is an image (png/jpg/jpeg/webp) with the
/// expected text in a sibling `<stem>.txt`, or an `expected.txt`/`text.txt` in the
/// same folder (so `<dir>/<case>/image.png` + `<dir>/<case>/expected.txt` works too).
/// Whitespace is normalised before comparing (content over exact layout); case is
/// kept (case errors are real OCR errors). `CCK_CONF` tunes the run.
fn ocr_bench(dir: &std::path::Path) {
    let mut cases: Vec<(std::path::PathBuf, String)> = Vec::new();
    collect_ocr_cases(dir, &mut cases);
    cases.sort_by(|a, b| a.0.cmp(&b.0));
    if cases.is_empty() {
        eprintln!(
            "No OCR cases under {} (need an image + a sibling .txt or expected.txt)",
            dir.display()
        );
        return;
    }
    let conf = std::env::var("CCK_CONF").ok().and_then(|s| s.parse().ok()).unwrap_or(25.0);
    let (mut sum, mut passing) = (0f64, 0usize);
    for (img_path, expected) in &cases {
        let want = norm_ws(expected);
        let got = match image::open(img_path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = (rgba.width(), rgba.height());
                norm_ws(&crate::detect::join_words(&crate::detect::scan_text(&rgba, 0, 0, w, h, conf)))
            }
            Err(_) => {
                println!("ERR  ----  {} (could not open)", img_path.display());
                continue;
            }
        };
        let sim = similarity(&got, &want);
        sum += sim;
        if sim >= 0.90 {
            passing += 1;
        }
        let tag = if sim >= 0.90 { "ok  " } else { "DIFF" };
        let name = img_path.strip_prefix(dir).unwrap_or(img_path);
        println!("{tag} {:.3}  {}", sim, name.display());
        if sim < 0.999 {
            println!("        got:  {got:?}");
            println!("        want: {want:?}");
        }
    }
    let n = cases.len();
    println!(
        "\n{n} cases | mean similarity {:.3} | pass(>=0.90) {passing}/{n}",
        sum / n as f64
    );
}

/// Recursively collect `(image, expected-text)` OCR cases under `dir`.
fn collect_ocr_cases(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_ocr_cases(&p, out);
            continue;
        }
        let is_img = p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp"));
        if is_img && let Some(exp) = expected_text_for(&p) {
            out.push((p, exp));
        }
    }
}

/// The expected text for an image: a sibling `<stem>.txt`, else a common name
/// (`expected`/`text`/`content`/`contents`.txt), else — if the folder has exactly one
/// `.txt` — that file.
fn expected_text_for(img: &std::path::Path) -> Option<String> {
    if let Ok(s) = std::fs::read_to_string(img.with_extension("txt")) {
        return Some(s);
    }
    let dir = img.parent()?;
    if let Some(s) = ["expected.txt", "text.txt", "content.txt", "contents.txt"]
        .iter()
        .find_map(|n| std::fs::read_to_string(dir.join(n)).ok())
    {
        return Some(s);
    }
    // Fallback: the sole .txt in the folder, whatever it's named.
    let mut txts = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("txt"));
    let only = txts.next()?;
    txts.next().is_none().then(|| std::fs::read_to_string(only).ok()).flatten()
}

/// Collapse all whitespace runs to single spaces and trim (layout-insensitive compare).
fn norm_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Character-level Levenshtein similarity in `[0, 1]` (1 = identical).
fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (la, lb) = (a.len(), b.len());
    if la == 0 && lb == 0 {
        return 1.0;
    }
    if la == 0 || lb == 0 {
        return 0.0;
    }
    let mut prev: Vec<usize> = (0..=lb).collect();
    let mut cur = vec![0usize; lb + 1];
    for i in 1..=la {
        cur[0] = i;
        for j in 1..=lb {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - prev[lb] as f64 / la.max(lb) as f64
}

/// Diagnostic: request a ScreenCast session for `src` (monitor by default, or
/// `--pw-test window`), then consume its PipeWire stream for ~3s and print the
/// frames received. Validates the portal + in-process PipeWire path end to end
/// without the recording UI. The portal dialog appears — pick a source.
#[cfg(target_os = "linux")]
fn pw_test(src: ashpd::desktop::screencast::SourceType) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("pw-test: tokio runtime: {e}");
            return;
        }
    };
    match rt.block_on(crate::platform::screencast::request(src, None)) {
        Ok(session) => {
            eprintln!(
                "pw-test: granted {} stream(s), restore_token={}",
                session.streams.len(),
                session.restore_token.is_some()
            );
            let Some(stream) = session.streams.first() else {
                eprintln!("pw-test: no streams returned");
                return;
            };
            eprintln!(
                "pw-test: node={} position={:?} size={:?}",
                stream.node_id, stream.position, stream.size
            );
            let node = stream.node_id;
            let stop = Arc::new(AtomicBool::new(false));
            let stop2 = stop.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(3));
                stop2.store(true, Ordering::Relaxed);
            });
            let mut n = 0u32;
            let r = crate::platform::pipewire::consume_frames(session.fd, node, None, stop, move |rgba, w, h, pts, pw_delay| {
                n += 1;
                if n <= 3 || n.is_multiple_of(60) {
                    eprintln!(
                        "pw-test: frame {n}: {w}x{h} ({} bytes) pts={pts} pw_delay_ms={}",
                        rgba.len(),
                        pw_delay / 1_000_000
                    );
                }
            });
            eprintln!("pw-test: capture ended: {r:?}");
        }
        Err(crate::platform::screencast::CastError::Cancelled) => eprintln!("pw-test: cancelled by user"),
        Err(crate::platform::screencast::CastError::Unavailable(e)) => eprintln!("pw-test: unavailable: {e}"),
    }
}

/// Diagnostic: like `--pw-test` but negotiate **DMA-BUF** (the zero-copy path) and
/// report what the compositor actually hands out — whether dmabuf frames arrive at
/// all, their DRM format + modifier (the modifier's top byte is the *vendor*, which
/// tells us which GPU produced the buffer, and therefore whether NVENC or VAAPI
/// zero-copy can import it), plane count, and fds. Run as `--dmabuf-test [window]`.
#[cfg(feature = "zero-copy")]
fn dmabuf_test(src: ashpd::desktop::screencast::SourceType) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("dmabuf-test: tokio runtime: {e}");
            return;
        }
    };
    match rt.block_on(crate::platform::screencast::request(src, None)) {
        Ok(session) => {
            let Some(stream) = session.streams.first() else {
                eprintln!("dmabuf-test: no streams returned");
                return;
            };
            eprintln!("dmabuf-test: node={}, requesting DMA-BUF for ~5s...", stream.node_id);
            let node = stream.node_id;
            let stop = Arc::new(AtomicBool::new(false));
            let got = Arc::new(AtomicBool::new(false));
            {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    stop.store(true, Ordering::Relaxed);
                });
            }
            let got_cb = got.clone();
            let stop_cb = stop.clone();
            let mut n = 0u32;
            let r = crate::platform::pipewire::consume_dmabuf(session.fd, node, stop.clone(), move |f| {
                got_cb.store(true, Ordering::Relaxed);
                n += 1;
                if n <= 2 {
                    let cc = f.fourcc.to_le_bytes();
                    let fourcc: String = cc.iter().map(|&b| b as char).collect();
                    let vendor = if f.modifier == 0x00ff_ffff_ffff_ffff {
                        "INVALID/unfixated".to_string()
                    } else {
                        match (f.modifier >> 56) & 0xff {
                            0 => "none/linear".to_string(),
                            1 => "Intel".to_string(),
                            2 => "AMD".to_string(),
                            3 => "NVIDIA".to_string(),
                            v => format!("vendor 0x{v:02x}"),
                        }
                    };
                    let fds: Vec<i32> = f.planes.iter().map(|p| p.0).collect();
                    eprintln!(
                        "dmabuf-test: frame {n}: {}x{} fourcc={fourcc:?} modifier=0x{:016x} \
                         (source GPU: {vendor}) planes={} fds={fds:?}",
                        f.width, f.height, f.modifier, f.planes.len()
                    );
                    if n == 2 {
                        stop_cb.store(true, Ordering::Relaxed);
                    }
                }
            });
            if got.load(Ordering::Relaxed) {
                eprintln!(
                    "dmabuf-test: SUCCESS. Cosmic delivered DMA-BUF frames. Zero-copy is \
                     possible; the source GPU above decides NVENC (NVIDIA) vs VAAPI (AMD/Intel)."
                );
            } else {
                eprintln!(
                    "dmabuf-test: NO dmabuf frames in 5s. Cosmic declined DMA-BUF over the \
                     portal, so portal zero-copy isn't possible (the CPU path is used)."
                );
            }
            eprintln!("dmabuf-test: ended: {r:?}");
        }
        Err(crate::platform::screencast::CastError::Cancelled) => eprintln!("dmabuf-test: cancelled by user"),
        Err(crate::platform::screencast::CastError::Unavailable(e)) => eprintln!("dmabuf-test: unavailable: {e}"),
    }
}

/// Hidden A/B harness for evaluating the noise-reduction stages. Reads `in_path`
/// (raw f32le mono 48 kHz) and writes three processed versions next to `out_prefix`:
/// `.webrtc.raw` (sonora WebRTC NS + high-pass), `.rnnoise.raw` (nnnoiseless), and
/// `.cascade.raw` (WebRTC -> RNNoise). Each pipeline gets its own state so the runs
/// are independent. Analyzed offline to see how much the two overlap.
fn denoise_test(in_path: &str, out_prefix: &str) {
    use sonora::config::{HighPassFilter, NoiseSuppression, NoiseSuppressionLevel};
    use sonora::{AudioProcessing, Config, StreamConfig};

    let Ok(bytes) = std::fs::read(in_path) else {
        eprintln!("denoise-test: cannot read {in_path}");
        return;
    };
    let input: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    let sc = StreamConfig::new(48_000, 1);
    let nf = sc.num_frames();
    let cfg = || Config {
        high_pass_filter: Some(HighPassFilter::default()),
        noise_suppression: Some(NoiseSuppression {
            level: NoiseSuppressionLevel::High,
            analyze_linear_aec_output_when_available: false,
        }),
        ..Default::default()
    };
    let mk_apm = || {
        AudioProcessing::builder()
            .config(cfg())
            .capture_config(sc)
            .render_config(sc)
            .build()
    };
    let mut apm_w = mk_apm();
    let mut apm_c = mk_apm();
    let mut rnn_r = nnnoiseless::DenoiseState::new();
    let mut rnn_c = nnnoiseless::DenoiseState::new();

    let (mut out_w, mut out_r, mut out_c) = (Vec::new(), Vec::new(), Vec::new());
    let rnn = |st: &mut nnnoiseless::DenoiseState, frame: &[f32], dst: &mut Vec<f32>| {
        let mut i16f = [0f32; 480];
        for (o, &s) in i16f.iter_mut().zip(frame) {
            *o = s * 32768.0;
        }
        let mut o = [0f32; 480];
        st.process_frame(&mut o, &i16f);
        dst.extend(o.iter().map(|s| s / 32768.0));
    };
    for chunk in input.chunks(nf) {
        let mut fr = vec![0f32; nf];
        fr[..chunk.len()].copy_from_slice(chunk);
        let mut ow = vec![0f32; nf];
        apm_w.process_capture_f32(&[&fr], &mut [&mut ow]).unwrap();
        out_w.extend_from_slice(&ow);
        rnn(&mut rnn_r, &fr, &mut out_r);
        let mut oc = vec![0f32; nf];
        apm_c.process_capture_f32(&[&fr], &mut [&mut oc]).unwrap();
        rnn(&mut rnn_c, &oc, &mut out_c);
    }
    let write = |suffix: &str, data: &[f32]| {
        let raw: Vec<u8> = data.iter().flat_map(|s| s.to_le_bytes()).collect();
        let _ = std::fs::write(format!("{out_prefix}.{suffix}.raw"), raw);
    };
    write("webrtc", &out_w);
    write("rnnoise", &out_r);
    write("cascade", &out_c);
    eprintln!(
        "denoise-test: {} frames -> {out_prefix}.{{webrtc,rnnoise,cascade}}.raw",
        input.len() / nf
    );
}

/// Synthetic self-test of the input cleanup chain (`--audio-test`): runs a generated
/// silence/voice signal through `InputProcessor` under several configs and prints
/// per-segment metering, so the gate / AGC / noise-suppression / VAD wiring can be
/// sanity-checked without a recording. NOTE: synthetic "voice" under-drives the neural
/// VADs (they want real speech), so the RNNoise/level path is the meaningful check here;
/// real-voice accuracy is best judged by ear in the mic test.
fn audio_input_test() {
    use crate::audio::{InputConfig, InputProcessor, FRAME};
    const SR: usize = 48_000;
    const SEG: usize = SR / 2; // 0.5 s segments
    const SEGS: usize = 8; // 4 s total, alternating silence / voice
    let pi = std::f32::consts::PI;
    let mut rng: u32 = 0x1234_5678;
    let mut noise = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        (rng as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    // Build the signal + per-sample truth (true = a voice segment).
    let mut sig: Vec<f32> = Vec::with_capacity(SEG * SEGS);
    let mut truth: Vec<bool> = Vec::with_capacity(SEG * SEGS);
    for s in 0..SEGS {
        let voice = s % 2 == 1;
        for n in 0..SEG {
            let t = n as f32 / SR as f32;
            let mut x = noise() * 0.004; // ~-48 dBFS background floor
            if voice {
                // Voiced-ish: 140 Hz fundamental + harmonics, AM "syllable" envelope.
                let env = 0.5 + 0.5 * (2.0 * pi * 4.0 * t).sin().abs();
                let v: f32 = (0..6)
                    .map(|h| {
                        let k = h as f32 + 1.0;
                        (1.0 / k) * (2.0 * pi * 140.0 * k * t).sin()
                    })
                    .sum();
                x += 0.12 * env * v; // ~-18 dBFS
            }
            sig.push(x.clamp(-1.0, 1.0));
            truth.push(voice);
        }
    }
    let base = InputConfig {
        noise_suppression: false,
        echo_cancellation: false,
        auto_gain: false,
        gate: false,
        gate_auto: true,
        gate_threshold: 0.5,
        advanced_vad: false,
    };
    let configs = [
        ("bypass (all off)", base),
        ("noise suppression", InputConfig { noise_suppression: true, ..base }),
        ("NS + AGC", InputConfig { noise_suppression: true, auto_gain: true, ..base }),
        (
            "NS + AGC + gate (RNNoise VAD)",
            InputConfig { noise_suppression: true, auto_gain: true, gate: true, ..base },
        ),
        (
            "NS + AGC + gate (earshot VAD)",
            InputConfig {
                noise_suppression: true,
                auto_gain: true,
                gate: true,
                advanced_vad: true,
                ..base
            },
        ),
    ];
    println!("audio-test: synthetic 4 s, 0.5 s silence/voice alternating @ 48 kHz");
    println!("levels are 0..1 on the meter dBFS scale (raw=input, clean=after chain)\n");
    println!(
        "{:<32} | {:^24} | {:^24}",
        "config", "VOICE segments", "SILENCE segments"
    );
    println!(
        "{:<32} | {:>5} {:>5} {:>5} {:>4} | {:>5} {:>5} {:>5} {:>4}",
        "", "raw", "clean", "open", "vad", "raw", "clean", "open", "vad"
    );
    for (name, cfg) in configs {
        let mut p = InputProcessor::new(cfg);
        let mut cnt = [0usize; 2];
        let mut raw = [0f64; 2];
        let mut clean = [0f64; 2];
        let mut open = [0usize; 2];
        let mut vad = [0f64; 2];
        let mut i = 0;
        while i + FRAME <= sig.len() {
            let mut fr = [0f32; FRAME];
            fr.copy_from_slice(&sig[i..i + FRAME]);
            let k = truth[i + FRAME / 2] as usize;
            let o = p.process(&fr, None);
            cnt[k] += 1;
            raw[k] += o.raw as f64;
            clean[k] += o.clean as f64;
            open[k] += o.open as usize;
            vad[k] += o.vad as f64;
            i += FRAME;
        }
        let avg = |s: f64, n: usize| if n > 0 { s / n as f64 } else { 0.0 };
        let cell = |k: usize| {
            format!(
                "{:>5.2} {:>5.2} {:>4.0}% {:>4.2}",
                avg(raw[k], cnt[k]),
                avg(clean[k], cnt[k]),
                100.0 * open[k] as f64 / cnt[k].max(1) as f64,
                avg(vad[k], cnt[k])
            )
        };
        println!("{:<32} | {} | {}", name, cell(1), cell(0));
    }

    // AGC ramp test: a QUIET continuous voice (~-32 dBFS), 6 s, so the gain has time to ramp.
    // Print the PEAK clean level per second (what the meter's bars show) so the climb into the
    // green band (0.80..0.90) is visible; a quiet mic should rise toward it and hold there.
    println!("\nAGC ramp (continuous quiet voice ~-32 dBFS, PEAK clean level per second):");
    let secs_voice = 6;
    let mut quiet: Vec<f32> = Vec::with_capacity(SR * secs_voice);
    for n in 0..SR * secs_voice {
        let t = n as f32 / SR as f32;
        let env = 0.5 + 0.5 * (2.0 * pi * 4.0 * t).sin().abs();
        let v: f32 = (0..6)
            .map(|h| {
                let k = h as f32 + 1.0;
                (1.0 / k) * (2.0 * pi * 140.0 * k * t).sin()
            })
            .sum();
        quiet.push((0.025 * env * v + noise() * 0.002).clamp(-1.0, 1.0)); // ~-32 dBFS
    }
    for (label, agc) in [("AGC off", false), ("AGC on", true)] {
        let cfg = InputConfig { noise_suppression: true, auto_gain: agc, ..base };
        let mut p = InputProcessor::new(cfg);
        let mut per_sec = vec![0f32; secs_voice];
        let mut i = 0;
        while i + FRAME <= quiet.len() {
            let mut fr = [0f32; FRAME];
            fr.copy_from_slice(&quiet[i..i + FRAME]);
            let o = p.process(&fr, None);
            let s = (i / SR).min(secs_voice - 1);
            per_sec[s] = per_sec[s].max(o.clean); // PEAK, like the waveform bars
            i += FRAME;
        }
        let cols: Vec<String> = per_sec.iter().map(|s| format!("{s:.2}")).collect();
        println!("  {:<8} {}", label, cols.join("  "));
    }
}

/// Headless smoke test of the recording mic path (`--mic-rec-test`): exercise
/// `setup_clean_mic_tap` (DRAGON-125/127: the tap mode is the ONLY recording-path
/// mic consumer now — the legacy FIFO feeder this test used to drive was retired
/// with the recording path it fed) by collecting ~3s of cleaned mono PCM straight
/// off its channel, then mux that against a synthetic video + the live system
/// monitor into a real file, so the channel layout (mono mic / stereo system) and
/// the cleaned audio itself can be inspected/listened to without the GUI recorder.
fn mic_rec_test() {
    use crate::audio::InputConfig;
    let cfg = InputConfig {
        noise_suppression: true,
        echo_cancellation: false,
        auto_gain: true,
        gate: true,
        gate_auto: true,
        gate_threshold: 0.5,
        advanced_vad: false,
    };
    // DRAGON-241: `/tmp` doesn't exist for a native Windows process (it resolves to a
    // nonexistent `<drive>:\tmp`, so the raw/out writes below would fail), so use the
    // real per-user temp dir there — the same place the `windows-*` tests write. Linux
    // and macOS keep the historical `/tmp` path byte-for-byte.
    #[cfg(not(target_os = "windows"))]
    let out = "/tmp/cck-rectest.mp4";
    #[cfg(target_os = "windows")]
    let out_buf = std::env::temp_dir().join("cck-rectest.mp4");
    #[cfg(target_os = "windows")]
    let out = out_buf.to_str().expect("temp dir path is valid UTF-8");
    let _ = std::fs::remove_file(out);
    let (w, h, fps) = (320u32, 240u32, 30u32);

    // No external far-end ring: this diagnostic runs no system capture of its own,
    // so echo cancellation (off here anyway) would use the dedicated capture.
    let Some((handle, rx)) = crate::audio::clean_mic::setup_clean_mic_tap(cfg, "", None) else {
        eprintln!("mic-rec-test: setup_clean_mic_tap failed (no mic?)");
        return;
    };
    const SECS: f64 = 3.0;
    const SR: usize = 48_000;
    let want_samples = (SR as f64 * SECS) as usize;
    let mut samples: Vec<f32> = Vec::with_capacity(want_samples);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(SECS + 2.0);
    while samples.len() < want_samples && std::time::Instant::now() < deadline {
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(tap) => samples.extend_from_slice(&tap.samples),
            Err(_) => break,
        }
    }
    drop(handle); // teardown: kill the mic/monitor captures
    if samples.is_empty() {
        eprintln!("mic-rec-test: no cleaned mic audio captured");
        return;
    }
    let dur = samples.len() as f64 / SR as f64;
    #[cfg(not(target_os = "windows"))]
    let mic_raw = std::path::Path::new("/tmp/cck-mic-tap-raw.f32");
    #[cfg(target_os = "windows")]
    let mic_raw_buf = std::env::temp_dir().join("cck-mic-tap-raw.f32");
    #[cfg(target_os = "windows")]
    let mic_raw = mic_raw_buf.as_path();
    let raw_bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    if let Err(e) = std::fs::write(mic_raw, &raw_bytes) {
        eprintln!("mic-rec-test: could not write the cleaned mic's raw PCM temp file: {e}");
        return;
    }

    // Mux the already-fully-captured cleaned mic (a plain file input now, not a
    // live FIFO) against a synthetic video + the live system monitor.
    let mut cmd = crate::util::ffmpeg_command();
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    cmd.args(["-f", "lavfi", "-i", &format!("color=c=black:s={w}x{h}:r={fps}:d={dur:.3}")]);
    cmd.args(["-f", "f32le", "-ar", "48000", "-ac", "1", "-i"]).arg(mic_raw);
    #[cfg(target_os = "linux")]
    {
        cmd.args(["-thread_queue_size", "1024", "-f", "pulse", "-i", "@DEFAULT_MONITOR@"]);
        cmd.args(["-map", "0:v:0", "-map", "1:a:0", "-map", "2:a:0"]);
        cmd.args([
            "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-b:a", "192k",
            "-metadata:s:a:0", "title=mic", "-metadata:s:a:1", "title=system",
            "-shortest",
        ]);
    }
    // Windows (DRAGON-241): there is no pulse monitor. Exercise the SAME WASAPI-loopback
    // seam the real recorder drives (`audio::capture::MonitorCapture`'s `#[cfg(windows)]`
    // arm) — capture ~`dur`s of the default render endpoint's loopback into a raw f32le
    // stereo file, then mux it beside the mic as a plain input (a non-silent system track
    // when audio is playing, an honestly-silent one otherwise). If loopback can't start or
    // delivers nothing, fall back to a mic-only mux with a clear message.
    #[cfg(target_os = "windows")]
    {
        let sys_raw = std::env::temp_dir().join("cck-sys-loopback-raw.f32");
        match capture_system_loopback(&sys_raw, dur) {
            Some(peak) => {
                eprintln!(
                    "mic-rec-test: WASAPI loopback captured system audio (peak {peak:.4}, {})",
                    if peak > 0.0001 { "non-silent" } else { "silent — nothing was playing" }
                );
                cmd.args(["-f", "f32le", "-ar", "48000", "-ac", "2", "-i"]).arg(&sys_raw);
                cmd.args(["-map", "0:v:0", "-map", "1:a:0", "-map", "2:a:0"]);
                cmd.args([
                    "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-b:a", "192k",
                    "-metadata:s:a:0", "title=mic", "-metadata:s:a:1", "title=system",
                    "-shortest",
                ]);
            }
            None => {
                eprintln!("mic-rec-test: WASAPI loopback unavailable; writing a mic-only mux");
                cmd.args(["-map", "0:v:0", "-map", "1:a:0"]);
                cmd.args([
                    "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-b:a", "192k",
                    "-metadata:s:a:0", "title=mic",
                    "-shortest",
                ]);
            }
        }
    }
    // macOS: there is no pulse monitor to grab a live system track from (and no
    // standalone system capture at all outside a recording's owned SCK stream), so
    // this diagnostic muxes the synthetic video + cleaned mic only.
    #[cfg(target_os = "macos")]
    {
        eprintln!("mic-rec-test: no pulse system monitor on macOS; writing a mic-only mux");
        cmd.args(["-map", "0:v:0", "-map", "1:a:0"]);
        cmd.args([
            "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-b:a", "192k",
            "-metadata:s:a:0", "title=mic",
            "-shortest",
        ]);
    }
    cmd.arg(out);
    let status = cmd.status();
    let _ = std::fs::remove_file(mic_raw);
    eprintln!("mic-rec-test: ffmpeg exited {status:?} ({dur:.2}s of cleaned mic captured)");
    let probe = crate::util::ffprobe_command()
        .args([
            "-v", "error", "-show_entries",
            "stream=index,codec_type,channels,channel_layout:stream_tags=title",
            "-of", "default=noprint_wrappers=1", out,
        ])
        .output();
    match probe {
        Ok(o) => println!("{}", String::from_utf8_lossy(&o.stdout)),
        Err(e) => eprintln!("mic-rec-test: ffprobe failed: {e}"),
    }
}

/// Capture ~`secs` of the default render endpoint's WASAPI loopback — the SAME seam the
/// real recorder drives ([`crate::audio::capture::MonitorCapture`]'s `#[cfg(windows)]`
/// arm) — into a raw interleaved-stereo f32le file at `path`, for the Windows `mic-rec`
/// mux. Returns the peak absolute sample (so the caller can honestly report
/// silent-vs-non-silent), or `None` if loopback can't start or delivered nothing (the
/// caller then muxes mic-only). DRAGON-241.
#[cfg(target_os = "windows")]
fn capture_system_loopback(path: &std::path::Path, secs: f64) -> Option<f32> {
    use std::time::{Duration, Instant};
    let (capture, rx) = crate::audio::capture::MonitorCapture::start(None, None)?;
    // Stereo @ 48 kHz → 2 samples per frame.
    let want = (48_000.0 * 2.0 * secs) as usize;
    let mut samples: Vec<f32> = Vec::with_capacity(want);
    let deadline = Instant::now() + Duration::from_secs_f64(secs + 1.0);
    while samples.len() < want && Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(chunk) => samples.extend_from_slice(&chunk.samples),
            Err(_) => break,
        }
    }
    let _ = capture.stop();
    if samples.is_empty() {
        return None;
    }
    let peak = samples.iter().copied().map(f32::abs).fold(0f32, f32::max);
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    std::fs::write(path, &bytes).ok()?;
    Some(peak)
}

/// Headless diagnostic: exercise the native gather + screencopy paths and print
/// what they produce (no overlay). `cosmic-capture-kit --selftest`.
fn selftest() {
    let t0 = std::time::Instant::now();
    let groups = crate::platform::compositor::list_toplevels();
    eprintln!(
        "gather: {} output group(s) in {}ms",
        groups.len(),
        t0.elapsed().as_millis()
    );
    let mut ids = Vec::new();
    for (name, wins) in &groups {
        eprintln!("  output {name}: {} window(s)", wins.len());
        for w in wins {
            ids.push(w.id.clone());
        }
    }
    ids.sort();
    ids.dedup();

    let t1 = std::time::Instant::now();
    let thumbs = crate::screenshot::windows(&ids);
    eprintln!(
        "capture_toplevels: {}/{} in {}ms",
        thumbs.len(),
        ids.len(),
        t1.elapsed().as_millis()
    );
    for (id, img) in &thumbs {
        eprintln!("    thumb id={id:?} {}x{}", img.width(), img.height());
    }

    // Try a full-resolution capture of the first toplevel directly, then apply
    // the same finishing the app does (flatten opaque + rounded corners).
    if let Some(id) = ids.first() {
        match crate::screenshot::window(id, false) {
            Some(img) => {
                let opaque = crate::compose::finish_window(img.clone(), 16, false);
                // Active-window border (accent lavender), 12px, concentric rounding.
                let bordered = crate::compose::add_border(opaque.clone(), 12, [151, 125, 236, 255], 16 + 12);
                let black = crate::compose::on_black(bordered.clone());
                crate::media::png::save_png(&bordered, std::path::Path::new("/tmp/cck-window-bordered.png"), "");
                crate::media::png::save_png(&black, std::path::Path::new("/tmp/cck-window-black.png"), "");
                eprintln!(
                    "capture_toplevel({id:?}): {}x{} -> bordered & black-bg saved",
                    bordered.width(),
                    bordered.height()
                );
            }
            None => eprintln!("capture_toplevel({id:?}): FAILED (no image)"),
        }
    }

    // Verify wallpaper crop (jpeg decode + cover-map) for the first window.
    if let (Some(wp), Some(win)) = (
        std::fs::read_to_string(
            dirs::config_dir()
                .unwrap()
                .join("cosmic/com.system76.CosmicBackground/v1/all"),
        )
        .ok()
        .and_then(|t| {
            let i = t.find("Path(\"")? + 6;
            let e = t[i..].find('"')?;
            Some(std::path::PathBuf::from(&t[i..i + e]))
        }),
        groups.values().flatten().next().cloned(),
    ) {
        let (x, y, w, h) = win.rect;
        match crate::wallpaper::wallpaper_crop(&wp, false, 5120, 1440, x, y, w as u32, h as u32) {
            Some(c) => {
                crate::media::png::save_png(&c, std::path::Path::new("/tmp/cck-wpcrop.png"), "");
                eprintln!("wallpaper_crop: {}x{} (from {:?})", c.width(), c.height(), wp);
            }
            None => eprintln!("wallpaper_crop FAILED"),
        }
    }

    // Try a full-output capture of the first output.
    if let Some(name) = groups.keys().next() {
        match crate::screenshot::output(name, None) {
            Some(img) => {
                let p = std::path::Path::new("/tmp/cck-selftest-output.png");
                let ok = crate::media::png::save_png(&img, p, "");
                eprintln!("capture_output({name}): {}x{} saved={ok} -> {}", img.width(), img.height(), p.display());
            }
            None => eprintln!("capture_output({name}): FAILED (no image)"),
        }
    }
}

// ── DRAGON-229 M1: Windows capture checkpoints (W1-SPEC §7) ───────────────────────
// Each saves its PNG under the temp dir and prints the absolute path. These call only
// the Windows plugin primitives (`crate::screenshot` / `platform::windows`) + portable
// PNG/print glue — the same shape as the mac routes above (strict split honored).

/// A filesystem-safe slug of a display name (`\\.\DISPLAY1` → `-----DISPLAY1`).
#[cfg(target_os = "windows")]
fn win_slug(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// `--test windows-monitors`: DPI awareness + every monitor's physical geometry + DPI.
#[cfg(target_os = "windows")]
fn windows_monitors_test() {
    println!(
        "thread DPI awareness: {} (want per-monitor)",
        crate::platform::windows::dpi::thread_dpi_awareness_str()
    );
    let lines = crate::platform::windows::monitor_report();
    println!("monitors ({}):", lines.len());
    for line in &lines {
        println!("  {line}");
    }
    if lines.is_empty() {
        println!("(no monitors — headless session?)");
    }
}

/// `--test windows-shot-output [name]`: a full-monitor BitBlt flat → PNG.
#[cfg(target_os = "windows")]
fn windows_shot_output_test(name: &str) {
    let descs = crate::screenshot::output_descs();
    let target = if name.is_empty() {
        descs
            .iter()
            .find(|d| d.logical_pos == (0, 0))
            .or_else(|| descs.first())
            .map(|d| d.name.clone())
    } else {
        Some(name.to_string())
    };
    let Some(target) = target else {
        eprintln!("windows-shot-output: no monitors");
        return;
    };
    match crate::screenshot::output(&target, None) {
        Some(img) => {
            let p = std::env::temp_dir().join(format!("cck-win-output-{}.png", win_slug(&target)));
            match img.save(&p) {
                Ok(()) => println!(
                    "captured {target}: {}x{} -> {}",
                    img.width(),
                    img.height(),
                    p.display()
                ),
                Err(e) => eprintln!("windows-shot-output: save failed: {e}"),
            }
        }
        None => eprintln!("windows-shot-output({target}): FAILED"),
    }
}

/// `--test windows-list`: the toplevel window list (id, rect, active, title).
#[cfg(target_os = "windows")]
fn windows_list_test() {
    let wins = crate::platform::windows::list_windows();
    println!("windows ({}):", wins.len());
    for w in &wins {
        let (x, y, ww, wh) = w.rect;
        println!(
            "  id={} rect=({x},{y},{ww},{wh}) active={} title={:?}",
            w.id, w.active, w.title
        );
    }
}

/// `--test windows-shot-window <hwnd>`: one window's pixels (PrintWindow ladder) → PNG.
#[cfg(target_os = "windows")]
fn windows_shot_window_test(id: &str) {
    if id.is_empty() {
        eprintln!("usage: --test windows-shot-window <hwnd-decimal>");
        return;
    }
    match crate::screenshot::window(id, false) {
        Some(img) => {
            let p = std::env::temp_dir().join(format!("cck-win-window-{id}.png"));
            match img.save(&p) {
                Ok(()) => println!(
                    "captured window {id}: {}x{} -> {}",
                    img.width(),
                    img.height(),
                    p.display()
                ),
                Err(e) => eprintln!("windows-shot-window: save failed: {e}"),
            }
        }
        None => eprintln!(
            "windows-shot-window({id}): None — bad grab on an OCCLUDED GPU-composited \
             window (WGC rescue is M3), or an invalid hwnd. See the DRAGON-229 log line."
        ),
    }
}

/// `--test windows-thumbs`: run the FULL window pre-capture enumeration
/// (`screenshot::windows` over every listed toplevel) — the exact path window MODE runs
/// at launch, and the one that used to hang forever on a single uncooperative window
/// (DRAGON-232). Times the whole bounded grab and reports each window as OK (real pixels)
/// or SKIP (pre-filtered / timed out → a placeholder tile in the live picker). A bounded
/// total elapsed with the process exiting cleanly is the headless proof the picker now
/// populates; run it with a hung/remote app (e.g. RustDesk) open to exercise a skip.
#[cfg(target_os = "windows")]
fn windows_thumbs_test() {
    let wins = crate::platform::windows::list_windows();
    let ids: Vec<String> = wins.iter().map(|w| w.id.clone()).collect();
    println!("enumerating {} windows; running the bounded per-window grab...", ids.len());
    let t0 = std::time::Instant::now();
    let raw = crate::screenshot::windows(&ids);
    let elapsed = t0.elapsed();
    let skipped = ids.len().saturating_sub(raw.len());
    println!(
        "screenshot::windows: {} grabbed / {skipped} skipped in {:.2}s",
        raw.len(),
        elapsed.as_secs_f64()
    );
    for w in &wins {
        match raw.get(&w.id) {
            Some(img) => {
                println!("  OK   id={} {}x{} title={:?}", w.id, img.width(), img.height(), w.title)
            }
            None => println!("  SKIP id={} -> placeholder tile title={:?}", w.id, w.title),
        }
    }
}

/// `--test windows-cursor`: the cursor sprite (real alpha) → PNG + position/hotspot/scale.
#[cfg(target_os = "windows")]
fn windows_cursor_test() {
    match crate::screenshot::capture_cursor() {
        Some((img, pos, hot, scale)) => {
            let p = std::env::temp_dir().join("cck-win-cursor.png");
            let ok = img.save(&p).is_ok();
            println!(
                "cursor: {}x{} at {:?} hotspot {:?} sprite_scale={:.3} saved={ok} -> {}",
                img.width(),
                img.height(),
                pos,
                hot,
                scale,
                p.display()
            );
        }
        None => println!("cursor: none (hidden?)"),
    }
}

/// `--test windows-wallpaper [name]`: `detect()` + the per-output resolver (honest None
/// under slideshow / solid color / undecodable), decoding a copy to confirm.
#[cfg(target_os = "windows")]
fn windows_wallpaper_test(name: &str) {
    println!("detect() (primary / cap): {:?}", crate::wallpaper::detect());
    let descs = crate::screenshot::output_descs();
    let target = if name.is_empty() {
        descs.first().map(|d| d.name.clone())
    } else {
        Some(name.to_string())
    };
    let Some(target) = target else {
        eprintln!("windows-wallpaper: no monitors");
        return;
    };
    match crate::platform::windows::wallpaper::wallpaper_for_output(&target) {
        Some((path, stretch)) => {
            println!("wallpaper_for_output({target}): {} (stretch={stretch})", path.display());
            if let Some(wp) = crate::wallpaper::decode_wallpaper(&path) {
                let p = std::env::temp_dir()
                    .join(format!("cck-win-wallpaper-{}.png", win_slug(&target)));
                let ok = wp.save(&p).is_ok();
                println!("  decoded {}x{} saved={ok} -> {}", wp.width(), wp.height(), p.display());
            }
        }
        None => println!(
            "wallpaper_for_output({target}): None (slideshow / solid color / undecodable — honest)"
        ),
    }
}

/// `--test windows-window-composite <hwnd> [black]`: the FINAL PROOF — the full
/// decorate/compose/wallpaper pipeline (single-window-on-wallpaper). `black` = the
/// wallpaper-OFF variant (window over black).
#[cfg(target_os = "windows")]
fn windows_window_composite_test(id: &str, mode: &str) {
    if id.is_empty() {
        eprintln!("usage: --test windows-window-composite <hwnd> [black]");
        return;
    }
    let Some(win) = crate::platform::windows::list_windows().into_iter().find(|w| w.id == id) else {
        eprintln!("windows-window-composite: hwnd {id} not in the window list");
        return;
    };
    let (x, y, w, h) = win.rect;
    let sel = crate::selection::Selection {
        x,
        y,
        width: w as u32,
        height: h as u32,
        output: None,
        window_id: Some(id.to_string()),
    };
    let wallpaper = mode != "black";
    // The Active-border DEFAULT follows the resolved appearance accent (DRAGON-239).
    // A CLI process never applies the global theme, so pass the accent resolved from
    // the PERSISTED appearance settings explicitly — the same colour the running app's
    // `None -> accent_rgba()` (applied theme) would draw — so the pixel proof is
    // faithful. An explicit `active_border_color` in config would win in the app; the
    // diag proves the auto/default path.
    let active_default = crate::app::theme::resolved_appearance_accent_rgba();
    println!("windows-window-composite: resolved active-border accent = {active_default:?}");
    let borders =
        crate::decoration::WindowBorders::resolve(Some(active_default), 3, [65, 69, 80, 255], 1);
    let frozen_geom: Vec<_> = crate::screenshot::output_descs()
        .into_iter()
        .map(|o| (o.name, o.logical_pos, o.logical_size))
        .collect();
    let job = crate::screenshot::WindowCaptureJob {
        id: id.to_string(),
        cursor: false,
        sel,
        capture_transparency: false,
        capture_wallpaper: wallpaper,
        window_radius: 12.0,
        border: borders.active,
        window_shadow: true,
        pad_logical: 24.0,
        dark: true,
        frozen_geom,
        frozen_px: None,
        // DRAGON-278: this diagnostic composites a live grab, not an activated focus-grab.
        active_grab: None,
        cursor_overlay: None,
    };
    match job.run() {
        Some(img) => {
            let tag = if wallpaper { "wp" } else { "black" };
            let p = std::env::temp_dir().join(format!("cck-win-composite-{id}-{tag}.png"));
            match img.save(&p) {
                Ok(()) => println!(
                    "composite {id} (wallpaper={wallpaper}): {}x{} -> {}",
                    img.width(),
                    img.height(),
                    p.display()
                ),
                Err(e) => eprintln!("windows-window-composite: save failed: {e}"),
            }
        }
        None => eprintln!("windows-window-composite({id}): FAILED (window grab returned None)"),
    }
}

/// `--test windows-wgc-frame <monitor|window|region> [args]`: grab ONE WGC frame and save
/// it as a PNG — the S1 checkpoint (isolated WGC video de-risk). `monitor [name]` (default
/// primary), `window <hwnd>`, or `region <x> <y> <w> <h>`.
#[cfg(target_os = "windows")]
fn windows_wgc_frame_test(kind: &str, a: &str, b: &str, c: &str, d: &str) {
    use crate::record::WinRecordTarget;
    let (target, x, y, w, h) = match kind {
        "window" => {
            if a.is_empty() {
                eprintln!("usage: --test windows-wgc-frame window <hwnd-decimal>");
                return;
            }
            (WinRecordTarget::Window(a.to_string()), 0, 0, 0, 0)
        }
        "region" => {
            let x: i32 = a.parse().unwrap_or(0);
            let y: i32 = b.parse().unwrap_or(0);
            let w: u32 = c.parse().unwrap_or(0);
            let h: u32 = d.parse().unwrap_or(0);
            if w == 0 || h == 0 {
                eprintln!("usage: --test windows-wgc-frame region <x> <y> <w> <h>");
                return;
            }
            (WinRecordTarget::Region, x, y, w, h)
        }
        _ => {
            let descs = crate::screenshot::output_descs();
            let name = if a.is_empty() {
                descs
                    .iter()
                    .find(|d| d.logical_pos == (0, 0))
                    .or_else(|| descs.first())
                    .map(|d| d.name.clone())
            } else {
                Some(a.to_string())
            };
            let Some(name) = name else {
                eprintln!("windows-wgc-frame: no monitors");
                return;
            };
            (WinRecordTarget::Display(name), 0, 0, 0, 0)
        }
    };
    match crate::record::wgc_capture_one_frame(target, x, y, w, h, false) {
        Ok((fw, fh, bytes)) => {
            let Some(img) = image::RgbaImage::from_raw(fw, fh, bytes) else {
                eprintln!("windows-wgc-frame: malformed frame buffer");
                return;
            };
            let p = std::env::temp_dir().join(format!("cck-wgc-frame-{kind}.png"));
            match img.save(&p) {
                Ok(()) => println!("wgc frame ({kind}): {fw}x{fh} -> {}", p.display()),
                Err(e) => eprintln!("windows-wgc-frame: save failed: {e}"),
            }
        }
        Err(e) => eprintln!("windows-wgc-frame({kind}): FAILED — {e}"),
    }
}

/// `--test windows-record <secs> [mic] [nosys] [pause] [nvenc|amf|qsv|software]`: record the
/// primary monitor for `secs` through the full owned pipeline (WGC + named-pipe audio +
/// WASAPI system + the chosen encoder) and print the finalized file. S2/S4 checkpoint (+
/// DRAGON-238 encoder-tier proof). `mic` enables the mic track, `nosys` gates the system
/// track OFF (it is still captured for the pre-flight), `pause` pauses at the midpoint for
/// 2s (the media-clock freeze check). The trailing encoder id (default `software`) sets the
/// preferred encoder; the RESOLVED id is logged so "picks nvenc when preferred" is provable
/// headlessly (a machine without the hardware degrades honestly to software).
#[cfg(target_os = "windows")]
fn windows_record_test(rest: &[String]) {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    let secs: u64 = rest.first().and_then(|s| s.parse().ok()).unwrap_or(5);
    let want_pause = rest.iter().any(|a| a == "pause");
    let mic = rest.iter().any(|a| a == "mic");
    let sys = !rest.iter().any(|a| a == "nosys");
    // Optional trailing encoder id (DRAGON-238); default software. Log what `resolve`
    // actually picks — the honest availability gate degrades an unusable encoder to software.
    let enc = rest
        .iter()
        .find(|a| matches!(a.as_str(), "nvenc" | "amf" | "qsv" | "software"))
        .cloned()
        .unwrap_or_else(|| "software".to_string());
    let presets = crate::encode::Presets::default();
    let resolved = crate::encode::EncodePlan::resolve_encoder_id(&enc, &presets);
    eprintln!("windows-record: preferred encoder={enc} -> resolved={resolved}");
    let descs = crate::screenshot::output_descs();
    let Some(name) = descs
        .iter()
        .find(|d| d.logical_pos == (0, 0))
        .or_else(|| descs.first())
        .map(|d| d.name.clone())
    else {
        eprintln!("windows-record: no monitors");
        return;
    };
    let out = std::env::temp_dir().join(format!("cck-winrec-{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&out);
    // DRAGON-272: `region` drives the REGION worker (a WGC crop of the most-overlapped
    // monitor) instead of a whole-display capture, to reproduce the region-recording stop
    // path headlessly. A fixed 800x600 crop at (100,100).
    let region = rest.iter().any(|a| a == "region");
    let (rx, ry, rw, rh, target) = if region {
        (100, 100, 800, 600, crate::record::WinRecordTarget::Region)
    } else {
        (0, 0, 0, 0, crate::record::WinRecordTarget::Display(name.clone()))
    };
    let params = crate::record::RegionRecordParams {
        x: rx,
        y: ry,
        w: rw,
        h: rh,
        cursor: true,
        win_target: target,
        settings: crate::record::RecordSettings {
            fps: 30,
            preferred_encoder: enc.clone(),
            presets,
            zero_copy: false,
            mic,
            system_audio: sys,
            bitrate_kbps: 8000,
            audio_offset_ms: 0,
            auto_device_compensation: true,
            max_res: (0, 0),
            metadata: "Cosmic Capture Kit | windows-record test".to_string(),
            out_path: out.clone(),
        },
    };
    eprintln!(
        "windows-record: {}, {secs}s, mic={mic} sys={sys} pause={want_pause} -> {}",
        if region { format!("REGION {rw}x{rh}@({rx},{ry}) of {name}") } else { format!("monitor {name}") },
        out.display()
    );
    let handle = crate::record::start_region_recording(params);
    let start = Instant::now();
    let mut did_pause = false;
    while start.elapsed().as_secs() < secs {
        if handle.done.lock().map(|g| g.is_some()).unwrap_or(false) {
            break; // early failure (e.g. audio pre-flight)
        }
        if want_pause && !did_pause && start.elapsed().as_secs_f64() >= secs as f64 / 2.0 {
            handle.paused.store(true, Ordering::Relaxed);
            eprintln!("windows-record: paused at {:.1}s", start.elapsed().as_secs_f64());
            std::thread::sleep(Duration::from_secs(2));
            handle.paused.store(false, Ordering::Relaxed);
            did_pause = true;
            eprintln!("windows-record: resumed");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    handle.stop.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Ok(g) = handle.done.lock()
            && let Some(res) = g.as_ref()
        {
            match res {
                Ok(p) => println!("windows-record: DONE -> {}", p.display()),
                Err(e) => eprintln!("windows-record: FAILED — {e}"),
            }
            return;
        }
        if Instant::now() >= deadline {
            eprintln!("windows-record: TIMEOUT waiting for finalize");
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `--test windows-audio-endpoints` (DRAGON-282): list every ACTIVE WASAPI render endpoint
/// (`id`, friendly name) the Output-device picker offers and `MonitorCapture` loops back.
/// The enumeration proof — and a live picker of what a persisted `speaker_device` id can be.
#[cfg(target_os = "windows")]
fn windows_audio_endpoints_test() {
    let eps = crate::platform::windows::wasapi_loopback::render_endpoints();
    println!("active render endpoints ({}):", eps.len());
    for (id, name) in &eps {
        println!("  {name}\n      id = {id}");
    }
    if eps.is_empty() {
        println!("(none — COM enumeration failed or no active render devices)");
    }
    println!("\n\"System (automatic)\" (empty id) records the OS default endpoint.");
}

/// `--test windows-shot-region <x> <y> <w> <h>`: a stitched region flat → PNG (for
/// mixed-DPI / negative-origin cross-monitor stitch verification).
#[cfg(target_os = "windows")]
fn windows_shot_region_test(xs: &str, ys: &str, ws: &str, hs: &str) {
    let x: i32 = xs.parse().unwrap_or(0);
    let y: i32 = ys.parse().unwrap_or(0);
    let w: u32 = ws.parse().unwrap_or(0);
    let h: u32 = hs.parse().unwrap_or(0);
    if w == 0 || h == 0 {
        eprintln!("usage: --test windows-shot-region <x> <y> <w> <h>");
        return;
    }
    match crate::screenshot::region(x, y, w, h, None) {
        Some(img) => {
            let p = std::env::temp_dir().join(format!("cck-win-region-{x}_{y}_{w}x{h}.png"));
            match img.save(&p) {
                Ok(()) => println!(
                    "region ({x},{y},{w},{h}): {}x{} -> {}",
                    img.width(),
                    img.height(),
                    p.display()
                ),
                Err(e) => eprintln!("windows-shot-region: save failed: {e}"),
            }
        }
        None => eprintln!("windows-shot-region: FAILED (off every monitor?)"),
    }
}
