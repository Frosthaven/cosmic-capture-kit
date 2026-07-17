mod app;
mod audio;
mod mixer;
mod record;
// The ext-image-copy-capture (Wayland) still + cursor capture client. macOS uses
// ScreenCaptureKit via the mac backend (DRAGON-94 phase 2), so this is Linux-only.
#[cfg(target_os = "linux")]
#[path = "platform/linux/native/screencopy.rs"]
mod screencopy;
// The still-grab layer: Wayland screencopy on Linux, an API-compatible stub
// elsewhere until the SCK grabs land (DRAGON-94 phase 2).
#[cfg(target_os = "linux")]
#[path = "platform/linux/native/screenshot.rs"]
mod screenshot;
#[cfg(not(target_os = "linux"))]
#[path = "platform/mac/screenshot.rs"]
mod screenshot;
mod compose;
mod decoration;
mod glass;
mod instance;
mod media;
mod wallpaper;
mod detect;
mod geometry;
mod platform;
mod widgets;
mod encode;
mod selection;
mod share;
mod shortcuts;
mod state;
// System-tray recording controls: ksni/StatusNotifierItem (D-Bus) on Linux; a menu-bar
// NSStatusItem + NSMenu on macOS (DRAGON-159); a no-op stub on any other platform. All
// three expose the same `TraySession` seam so the app's tray handling is platform-free.
#[cfg(target_os = "linux")]
#[path = "platform/linux/tray.rs"]
mod tray;
#[cfg(target_os = "macos")]
#[path = "platform/mac/tray.rs"]
mod tray;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[path = "platform/tray_stub.rs"]
mod tray;
mod util;
mod cli;
// In-app update channel (DRAGON-175): the manifest fetch/parse, semver compare,
// and (macOS) the one-click download+verify+swap install flow. The channel is a
// SEPARATE public Pages repo because the source repo is private (Pages needs a
// paid plan for private repos). Pure islands are unit-tested; I/O is not.
mod update;
// Portable recording-control UI model (DRAGON-172): the three-state icon geometry +
// `IconState`, the Pause/Resume label, and the in-recording menu MODEL — the ONE source
// every control surface maps onto its widgets (macOS daemon NSMenu + own status item,
// the Linux ksni tray, and the future Linux resident tray, DRAGON-173). Not gated, so
// mac + Linux share the geometry/labels/order and can never drift.
mod recording_ui;
// The resident menu-bar DAEMON (macOS): a tiny AppKit-only process that owns the
// menu-bar item + PrintScreen hotkey and spawns the full app as one-shot capture
// children. A bare `resident` launch early-branches here (see `main`), never
// touching the iced/wgpu stack. cfg(macos) inside the module keeps Linux clean.
#[cfg(target_os = "macos")]
#[path = "platform/mac/daemon.rs"]
mod daemon;
// The resident system-tray RESIDENT (Linux, DRAGON-173): the full-parity port of the
// mac daemon — a tiny process (NO iced/wgpu) owning ONE ksni StatusNotifierItem with the
// same three-state icon + menu, spawning one-shot capture children. A bare `resident`
// launch early-branches here (see `main`). cfg(linux) inside the module keeps mac clean.
#[cfg(target_os = "linux")]
#[path = "platform/linux/daemon.rs"]
mod daemon_linux;
// Recording control IPC between the resident (macOS daemon / Linux resident) and a
// recording capture child (DRAGON-170/173): the resident owns the ONE tray/menu-bar item
// + the in-recording actions; the child relays its recording state and receives the
// resident's menu commands over a Unix socket. Portable — mac AND Linux share the exact
// same wire protocol + socket path so the surfaces can never drift.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "platform/daemon_ipc.rs"]
mod daemon_ipc;

/// Install a macOS crash logger: chain a panic hook that appends the panic
/// message + a backtrace to `~/Library/Logs/cosmic-capture-kit/panic.log`
/// before delegating to the default hook (so terminal runs still print).
/// The app runs bundled (no terminal) in resident mode, where AppKit/winit
/// panics like the stop→preview `frame_did_change` crash would otherwise be
/// invisible. Kept tiny: last-writer-wins single file, best-effort I/O.
#[cfg(target_os = "macos")]
fn install_macos_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(home) = std::env::var_os("HOME") {
            let mut dir = std::path::PathBuf::from(home);
            dir.push("Library/Logs/cosmic-capture-kit");
            if std::fs::create_dir_all(&dir).is_ok() {
                use std::io::Write as _;
                let bt = std::backtrace::Backtrace::force_capture();
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("panic.log"))
                {
                    let _ = writeln!(f, "\n===== panic @ unix {ts} =====\n{info}\n{bt}");
                }
            }
        }
        default_hook(info);
    }));
}

fn main() -> cosmic::iced::Result {
    util::timing_start();
    util::timing_mark("main() entry (after dyld + static init)");
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    #[cfg(target_os = "macos")]
    install_macos_panic_hook();
    // GUI launches (Spotlight, login item, the resident daemon + every capture child
    // it spawns) inherit launchd's minimal PATH — missing /opt/homebrew/bin etc. — so
    // bare-name tools (tesseract, and the PATH-scan probes behind it) fail to resolve
    // even when installed. Normalize PATH ONCE here, before anything spawns or probes;
    // children inherit the corrected env. Idempotent — a no-op for terminal launches.
    #[cfg(target_os = "macos")]
    platform::mac::env::normalize_path_env();
    // The hidden test/diagnostic harnesses (benchmarks + capture/encode/audio probes)
    // all dispatch through one `--test <name> [args]` entry point, keeping the binary's
    // flag surface small. Genuine helpers (clipboard, notify, reveal, inspect, settings)
    // stay first-class below.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return Ok(());
    }
    if let Some(i) = args.iter().position(|a| a == "--test") {
        let name = args.get(i + 1).map(String::as_str).unwrap_or("");
        cli::run_test(name, &args[(i + 2).min(args.len())..]);
        return Ok(());
    }
    // Hidden post-capture helpers (a separate, single-threaded process for each).
    let after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(std::path::PathBuf::from)
    };
    if let Some(p) = after(share::reexec::COPY_IMAGE) {
        share::run_copy(&p, false);
        return Ok(());
    }
    if let Some(p) = after(share::reexec::COPY_FILE) {
        share::run_copy(&p, true);
        return Ok(());
    }
    if let Some(p) = after(share::reexec::COPY_TEXT) {
        share::run_copy_text(&p);
        return Ok(());
    }
    if let Some(i) = args.iter().position(|a| a == share::reexec::OPEN_URI) {
        if let Some(uri) = args.get(i + 1) {
            share::run_open_uri(uri);
        }
        return Ok(());
    }
    if let Some(p) = after(share::reexec::REVEAL) {
        share::run_reveal(&p);
        return Ok(());
    }
    if let Some(p) = after("--inspect") {
        cli::inspect(&p);
        return Ok(());
    }
    // A/V-sync calibration subcommands (one-shot, no GUI): write the reference
    // clip / measure a recording of it. Parsed positionally (not via `after`) so
    // a missing/optional argument can't fall through into a GUI launch.
    if let Some(i) = args.iter().position(|a| a == "--make-sync-clip") {
        let path = args.get(i + 1).filter(|a| !a.starts_with("--")).map(String::as_str);
        cli::make_sync_clip(path);
        return Ok(());
    }
    if let Some(i) = args.iter().position(|a| a == "--calibrate-sync") {
        let file = args
            .get(i + 1)
            .filter(|a| !a.starts_with("--"))
            .map(std::path::PathBuf::from);
        cli::calibrate_sync(file.as_deref(), args.iter().any(|a| a == "--apply"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--focus-settings") {
        // Detached helper: focus the already-open settings window (by title) so
        // the activation survives the launching instance exiting.
        platform::compositor::activate_title(app::WINDOW_TITLE);
        return Ok(());
    }
    if let Some(p) = after(share::reexec::NOTIFY_COPIED) {
        share::run_notify(&p, true);
        return Ok(());
    }
    if let Some(p) = after(share::reexec::NOTIFY_SAVED) {
        share::run_notify(&p, false);
        return Ok(());
    }
    // Babysitter for pausing other apps' media while a preview soundtrack is open — runs here, in a
    // clean single-threaded child (Linux: zbus from the GUI's threads can stall; macOS: the
    // MediaRemote proxy launches perl). It pauses, waits for the GUI to let go (stdin EOF), resumes.
    if args.iter().any(|a| a == crate::audio::ducking::DUCK_FLAG) {
        crate::audio::ducking::run_duck();
        return Ok(());
    }
    // macOS (DRAGON-130): the AeroSpace death-pipe babysitter child. Runs here in a
    // clean single-threaded child; it blocks on stdin and re-enables the paused tiling
    // WM when the parent exits/crashes (the OS closes the pipe). Absent off macOS
    // (cfg-gated) so Linux stays byte-identical.
    #[cfg(target_os = "macos")]
    if args.iter().any(|a| a == platform::mac::window::AEROSPACE_BABYSIT_FLAG) {
        platform::mac::window::run_restore_aerospace();
        return Ok(());
    }
    // macOS resident DAEMON (DRAGON-130): a BARE launch (no capture-mode / settings /
    // preview / worker flag) with the persisted `resident` setting on runs the tiny
    // AppKit-only menu-bar daemon and NEVER touches the iced/wgpu GUI stack — that is
    // what keeps the resident idle footprint at ~15-40MB instead of the ~440MB the old
    // in-app resident idle cost. All worker/subcommand flags above already returned, so
    // reaching here means this is a launch that WOULD open the GUI; gate the daemon on
    // it being bare + resident. Every other launch (capture flags, `--settings`,
    // `--preview`, or non-resident) falls through to `app::run` exactly as before.
    // A capture-mode flag is "capture NOW" intent, so it must NOT start the daemon —
    // it spawns a one-shot capture app, same as a daemon-spawned child.
    #[cfg(target_os = "macos")]
    {
        let bare = !args.iter().any(|a| {
            matches!(
                a.as_str(),
                "--settings"
                    | "--permissions"
                    | "--region"
                    | "--window"
                    | "--monitor"
                    | "--image"
                    | "--video"
                    | "--scan"
                    | "--scanner"
                    | "--countdown"
                    | "--overlay"
            )
        });
        // `--preview <file>` set `after("--preview")` below; a bare check for the flag
        // presence covers it here without consuming the value.
        let bare = bare && !args.iter().any(|a| a == "--preview");
        if bare && state::load().resident {
            daemon::run(); // never returns — runs the AppKit run loop or exits
        }
    }
    // Linux resident RESIDENT (DRAGON-173): the full-parity counterpart of the mac
    // branch above. A BARE launch (no capture-mode / settings / preview / worker flag)
    // with the persisted `resident` setting on runs the tiny ksni-tray resident and NEVER
    // touches the iced/wgpu GUI stack — the same lightweight technique the mac daemon
    // uses. Every other launch (capture flags, `--settings`, `--preview`, or non-resident)
    // falls through to `app::run` exactly as before. `--permissions` is macOS-only, so it
    // is not in the Linux bare check. Byte-identical to a bare launch when `resident` is
    // off (the historical Linux default).
    #[cfg(target_os = "linux")]
    {
        let bare = !args.iter().any(|a| {
            matches!(
                a.as_str(),
                "--settings"
                    | "--region"
                    | "--window"
                    | "--monitor"
                    | "--image"
                    | "--video"
                    | "--scan"
                    | "--scanner"
                    | "--countdown"
                    | "--overlay"
                    | "--preview"
            )
        });
        if bare && state::load().resident {
            // Intent from the argv shape (DRAGON-180): the autostart entry launches
            // with a literal `resident` argument (daemon-intent: keep the restart-
            // handoff lock retry); a truly bare launch is the global hotkey asking
            // to capture (single lock attempt, signal the live daemon immediately).
            let daemon_intent = args.iter().any(|a| a == "resident");
            daemon_linux::run(daemon_intent); // never returns — runs the tray loop or exits
        }
    }
    // `--preview <file>` opens the preview overlay directly for an existing image/video
    // (no capture overlay, no lock — it's a viewer). Reject unsupported types up front.
    if let Some(p) = after("--preview") {
        if !p.exists() {
            eprintln!("--preview: file not found: {}", p.display());
            std::process::exit(1);
        }
        if app::preview_media_kind(&p).is_none() {
            eprintln!("--preview: unsupported file type: {}", p.display());
            eprintln!("supported: images (png, jpg, gif, …) and videos (mp4, mkv, webm, …)");
            std::process::exit(1);
        }
        // `--preview` opens in a WINDOW by default (a file viewer wants a normal,
        // resizable window); pass `--overlay` alongside it for the fullscreen overlay.
        let preview_windowed = Some(!args.iter().any(|a| a == "--overlay"));
        return app::run(app::Startup {
            settings_only: false,
            preview: Some(p),
            preview_windowed,
            ..Default::default()
        });
    }
    // `--permissions` opens the macOS permission-checker window directly (no capture
    // overlay). Like `--settings` it takes NO capture lock. On non-macOS the flag is
    // inert (no TCC grants / no permission window) — it falls through to a normal
    // launch, keeping Linux byte-identical to a bare launch.
    #[cfg(target_os = "macos")]
    let permissions_only = args.iter().any(|a| a == "--permissions");
    #[cfg(not(target_os = "macos"))]
    let permissions_only = false;
    // `--settings` opens the settings window directly (no capture overlay).
    let settings_only = args.iter().any(|a| a == "--settings");
    if settings_only {
        // Only one settings pane may exist (across all instances). A settings-only
        // launch does NOT take the capture lock, so a capture instance can still
        // run alongside it.
        if !instance::acquire_settings_lock() {
            log::info!("settings already open; not opening another");
            // macOS (DRAGON-153): focus the existing pane instead of vanishing
            // silently (the daemon's Settings menu item lands here when a pane is
            // already open). Linux keeps its historical quiet return (the in-app
            // gear path covers focus there via the --focus-settings helper).
            #[cfg(target_os = "macos")]
            platform::compositor::activate_title(app::WINDOW_TITLE);
            return Ok(());
        }
    } else if permissions_only {
        // A permission-checker launch takes no capture lock either — it captures
        // nothing. Multiple can't stack usefully, but there's no shared pane lock to
        // contend (unlike settings), so just proceed.
    } else if !state::load().allow_multiple && !instance::acquire_lock() {
        // Another capture instance already holds the lock — don't open a duplicate
        // overlay. The macOS resident "capture NOW on second launch" UX lives in the
        // daemon now (a bare resident launch early-branches to `daemon::run`, which
        // signals the running daemon); a capture-mode second launch is a genuine
        // one-shot that simply steps aside here, same as on Linux.
        log::info!("cosmic-capture-kit already running; not opening a second overlay");
        return Ok(());
    }
    // Capture-mode flags: launch straight into a mode / kind / countdown. Absent
    // flags leave the fields `None`, so a bare launch is byte-identical to before.
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let mode = if has("--monitor") {
        Some(app::Mode::Monitor)
    } else if has("--window") {
        Some(app::Mode::Window)
    } else if has("--region") {
        Some(app::Mode::Region)
    } else {
        None
    };
    let kind = if has("--scan") || has("--scanner") {
        Some(app::Kind::Scanner)
    } else if has("--video") {
        Some(app::Kind::Video)
    } else if has("--image") {
        Some(app::Kind::Image)
    } else {
        None
    };
    // Exact seconds — not snapped to a UI preset — so `--countdown 7` counts 7.
    // Clamped to the u8 ticker range; 0 is a valid "no delay".
    let countdown_secs = after("--countdown")
        .and_then(|p| p.to_str().and_then(|s| s.parse::<u64>().ok()))
        .map(|s| s.min(u8::MAX as u64));
    util::timing_mark("about to call app::run (into iced/cosmic runtime)");
    app::run(app::Startup {
        settings_only,
        permissions_only,
        preview: None,
        mode,
        kind,
        countdown_secs,
        preview_windowed: None,
    })
}

/// Print the user-facing CLI usage (`--help`/`-h`).
fn print_usage() {
    println!(
        "cosmic-capture-kit: native COSMIC screen capture\n\
\n\
USAGE:\n\
    cosmic-capture-kit [FLAGS]\n\
\n\
Launch (opens the capture overlay by default):\n\
    --region                Start in region-select mode (default)\n\
    --window                Start in window-select mode\n\
    --monitor               Start in monitor-select mode\n\
    --image                 Capture a screenshot (default)\n\
    --video                 Capture a screen recording\n\
    --scan                  Start the QR/OCR scanner (forces region)\n\
    --countdown <secs>      Pre-capture countdown in seconds (any value, e.g. 7)\n\
\n\
Other:\n\
    --preview <file>        Open an existing image/video in the preview (windowed)\n\
    --overlay               With --preview: use the fullscreen overlay, not a window\n\
    --inspect <file>        Print a capture's embedded metadata and exit\n\
    --make-sync-clip [path] Write the A/V-sync reference clip (flash + beep) and exit\n\
    --calibrate-sync <file> Measure a recording of the reference clip; --apply stores it\n\
    --settings              Open the settings window only (no capture overlay)\n\
    --permissions           Open the macOS permission checker (macOS only)\n\
    -h, --help              Show this help\n\
\n\
{}\n\
\n\
Examples:\n\
    cosmic-capture-kit --region --video --countdown 3\n\
    cosmic-capture-kit --monitor\n\
    cosmic-capture-kit --scan",
        cli::SYNC_WORKFLOW
    );
}
